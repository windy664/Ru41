//! 知识库检索（RAG）核心服务：把 Obsidian vault 的 markdown 切块、向量化、
//! 存进独立的 knowledge.db（可重建的衍生数据，不是状态库），提供语义检索。
//!
//! 检索方：agent 经 `ru41 knowledge search` CLI（vault-search skill 引导）；
//! 索引入库：obsidian 同步服务在每次提交后重扫，或 `ru41 knowledge index` 手动全量。

use crate::ports::embedding::Embedder;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 切块目标长度（字符）；中文按字符近似 token，bge-small 上限 512 token。
const CHUNK_TARGET: usize = 600;
const CHUNK_HARD_MAX: usize = 800;
/// 相邻块重叠，避免语义在边界处被切断。
const CHUNK_OVERLAP: usize = 80;
/// 检索返回的上下文片段长度。
const SNIPPET_CHARS: usize = 200;

pub struct KnowledgeService {
    embedder: Arc<dyn Embedder>,
    db_path: PathBuf,
}

#[derive(Debug, serde::Serialize)]
pub struct KnowledgeHit {
    pub path: String,
    pub heading: String,
    pub score: f32,
    pub snippet: String,
}

#[derive(Debug, serde::Serialize)]
pub struct IndexReport {
    pub files: usize,
    pub chunks: usize,
    pub skipped: usize,
    pub model: String,
    pub elapsed_ms: u128,
}

impl KnowledgeService {
    pub fn new(embedder: Arc<dyn Embedder>, db_path: PathBuf) -> Result<Self> {
        let service = Self { embedder, db_path };
        let conn = service.open()?;
        // 换模型不换库会静默返回错误结果：维度不一致时拒绝使用。
        if let Some(recorded) = meta_get(&conn, "dim")? {
            let recorded: usize = recorded.parse().context("meta dim corrupt")?;
            anyhow::ensure!(
                recorded == service.embedder.dim(),
                "knowledge.db 维度 {recorded} 与当前模型 {} 不符，请删除数据库重建索引",
                service.embedder.dim()
            );
        }
        meta_set(&conn, "dim", &service.embedder.dim().to_string())?;
        meta_set(&conn, "model", service.embedder.model_name())?;
        Ok(service)
    }

    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&self.db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // 读写跨容器（obsidian 写、worker 读），忙等而不是直接报错。
        conn.pragma_update(None, "busy_timeout", 5000_i64)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS chunks(
                path TEXT NOT NULL,
                idx INTEGER NOT NULL,
                heading TEXT NOT NULL DEFAULT '',
                text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                PRIMARY KEY(path, idx)
            );
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(conn)
    }

    /// 全量重建索引。索引是衍生数据：清空重灌，天然幂等。
    pub fn index_vault(&self, vault: &Path) -> Result<IndexReport> {
        let started = std::time::Instant::now();
        let files = collect_markdown(vault);
        let mut all: Vec<(String, String, String)> = Vec::new(); // (path, heading, text)
        let mut skipped = 0_usize;
        for file in &files {
            let Ok(content) = std::fs::read_to_string(file) else {
                skipped += 1;
                continue;
            };
            let relative = file
                .strip_prefix(vault)
                .unwrap_or(file)
                .to_string_lossy()
                .trim_start_matches('/')
                .to_owned();
            let chunks = chunk_markdown(&content);
            if chunks.is_empty() {
                skipped += 1;
                continue;
            }
            for (heading, text) in chunks {
                all.push((relative.clone(), heading, text));
            }
        }
        let conn = self.open()?;
        conn.execute("DELETE FROM chunks", [])?;
        let mut chunks_done = 0_usize;
        for batch in all.chunks(32) {
            let texts: Vec<String> = batch
                .iter()
                .map(|(path, heading, text)| embed_text(path, heading, text))
                .collect();
            let vectors = self.embedder.embed(&texts)?;
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO chunks(path, idx, heading, text, embedding) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for ((path, heading, text), vector) in batch.iter().zip(vectors) {
                    let blob: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
                    stmt.execute(rusqlite::params![
                        path,
                        chunks_done as i64,
                        heading,
                        text,
                        blob
                    ])?;
                    chunks_done += 1;
                }
            }
            tx.commit()?;
        }
        meta_set(&conn, "indexed_at", &crate::util::now().to_string())?;
        Ok(IndexReport {
            files: files.len(),
            chunks: chunks_done,
            skipped,
            model: self.embedder.model_name().to_owned(),
            elapsed_ms: started.elapsed().as_millis(),
        })
    }

    /// 语义检索：余弦相似度 top-k。全表扫描在万块级仍是个位数毫秒。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeHit>> {
        let conn = self.open()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let query_vec = self
            .embedder
            .embed(&[query.to_owned()])?
            .into_iter()
            .next()
            .context("embedder returned no vector")?;
        let mut stmt =
            conn.prepare("SELECT path, heading, text, embedding FROM chunks")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut hits: Vec<KnowledgeHit> = Vec::new();
        for row in rows {
            let (path, heading, text, blob) = row?;
            let score = cosine(&query_vec, &blob);
            hits.push(KnowledgeHit {
                snippet: crate::util::truncate_chars(text.trim(), SNIPPET_CHARS),
                path,
                heading,
                score,
            });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }
}

/// 收集 vault 内全部 markdown；跳过 Obsidian 内部目录与模板。
fn collect_markdown(vault: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![vault.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if path.is_dir() {
                if !name.starts_with('.') && name != "templates" && name != "Templates-模板" {
                    stack.push(path);
                }
            } else if name.ends_with(".md") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// 去掉 YAML frontmatter，按标题层级切块：块内保留标题路径作为语义上下文。
fn chunk_markdown(content: &str) -> Vec<(String, String)> {
    let body = strip_frontmatter(content);
    let mut chunks = Vec::new();
    let mut heading = String::new();
    let mut buf = String::new();
    for line in body.lines() {
        if let Some(title) = line.strip_prefix('#') {
            if !buf.trim().is_empty() {
                flush(&mut chunks, &heading, &mut buf);
            }
            heading = title.trim_start_matches('#').trim().to_owned();
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
        if buf.chars().count() >= CHUNK_HARD_MAX {
            flush(&mut chunks, &heading, &mut buf);
        }
    }
    if !buf.trim().is_empty() {
        flush(&mut chunks, &heading, &mut buf);
    }
    // 长块已在写入时切断；这里只合并过短的尾巴块进前一块。
    let mut merged: Vec<(String, String)> = Vec::new();
    for (h, text) in chunks {
        let text_chars = text.chars().count();
        if text_chars < CHUNK_TARGET / 3
            && let Some((last_h, last_text)) = merged.last_mut()
            && *last_h == h
            && last_text.chars().count() + text_chars < CHUNK_HARD_MAX
        {
            last_text.push('\n');
            last_text.push_str(&text);
            continue;
        }
        merged.push((h, text));
    }
    merged
}

fn flush(chunks: &mut Vec<(String, String)>, heading: &str, buf: &mut String) {
    let text = buf.trim();
    if !text.is_empty() {
        chunks.push((heading.to_owned(), text.to_owned()));
    }
    // 重叠：保留尾巴一段，语义不断裂。
    let keep: String = buf.chars().rev().take(CHUNK_OVERLAP).collect();
    *buf = keep.chars().rev().collect();
}

fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return content;
    };
    match rest.find("\n---") {
        Some(end) => &rest[end + 4..],
        None => content,
    }
}

/// 向量化的文本形态：路径与标题是很强的语义信号，一并送入。
fn embed_text(path: &str, heading: &str, text: &str) -> String {
    if heading.is_empty() {
        format!("{path}\n{text}")
    } else {
        format!("{path}｜{heading}\n{text}")
    }
}

fn cosine(query: &[f32], blob: &[u8]) -> f32 {
    let mut dot = 0_f32;
    let mut norm_q = 0_f32;
    let mut norm_d = 0_f32;
    for (q, bytes) in query.iter().zip(blob.chunks_exact(4)) {
        let d = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        dot += q * d;
        norm_q += q * q;
        norm_d += d * d;
    }
    if norm_q == 0.0 || norm_d == 0.0 {
        return 0.0;
    }
    dot / (norm_q.sqrt() * norm_d.sqrt())
}

fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
        [key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{chunk_markdown, cosine, strip_frontmatter};

    #[test]
    fn frontmatter_is_stripped() {
        let content = "---\ntags: [a]\n---\n# 标题\n正文";
        assert!(strip_frontmatter(content).contains("正文"));
        assert!(!strip_frontmatter(content).contains("tags"));
    }

    #[test]
    fn chunks_carry_heading_context() {
        let content = "# 竞赛\n\nCISCN 是信息安全竞赛。\n\n## 报名时间\n\n每年四月。";
        let chunks = chunk_markdown(content);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|(h, t)| h.contains("报名时间") && t.contains("四月")));
    }

    #[test]
    fn short_tail_chunks_merge_into_previous() {
        let content = "# A\n\n".to_owned() + &"正文。".repeat(200) + "\n\n## B\n\n短";
        let chunks = chunk_markdown(&content);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].0, "B");
        assert!(chunks[1].1.contains('短'));
    }

    #[test]
    fn cosine_matches_identical_vectors() {
        let v = [1.0_f32, 2.0, 3.0];
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        assert!((cosine(&v, &blob) - 1.0).abs() < 1e-5);
    }
}
