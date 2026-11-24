//! 凭据托管的数据约定：凭据只有**唯一标识**，不分类、不解释。
//!
//! 与调研到的 SecretRef/broker 模式一致（1Password Secure Agentic Autofill、
//! Auth0 Agent Core、OpenClaw RFC #9676）：调用方按唯一 ID 引用凭据，
//! 真值只在执行期注入，永远不进模型上下文。描述性文字只是标签
//! （给人看：名单里能认出这是哪条），不参与寻址、也不构成层级。

use anyhow::Result;
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// 凭据记录类别（`records` 表分区）。
pub const KIND: &str = "credential";

/// 凭据唯一标识：`cred_` + 12 位十六进制。
pub const ID_PREFIX: &str = "cred_";

/// 生成凭据唯一标识（随机、不可预测——ID 会出现在笔记里，别让它可枚举）。
pub fn new_id() -> String {
    let mut rng = rand::rng();
    let hex: String = (0..12)
        .map(|_| format!("{:x}", rng.random_range(0..16)))
        .collect();
    format!("{ID_PREFIX}{hex}")
}

/// 是否形如凭据标识。
pub fn is_id(text: &str) -> bool {
    text.len() == ID_PREFIX.len() + 12
        && text.starts_with(ID_PREFIX)
        && text[ID_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
}

/// 按标识或唯一标签/别名解析凭据。
///
/// 标识优先；标识不认识时按标签**或别名**唯一匹配（标签/别名重复返回歧义错误——
/// 宁可不给，也不拿错一条）。核心 CLI 与 credentials 插件共用这一份寻址逻辑。
///
/// ⚠️ **别名是给「用户心智里的叫法」留的入口**(2026-11-24)：凭据托管 v1 的寻址键
/// 就是用户当时说的那个词（「帮我记住 ftp 的密码是 xxx」→ 名字叫 "ftp"），v2 改成
/// 唯一标识后那个入口凭空消失了 —— 用户问「ftp 的凭据」只能拿到标识。
/// 别名把那个入口补回来，且**只用于寻址**，不参与出站文本替换（见 [`unseal_text`]）。
pub fn resolve(
    store: &dyn crate::ports::storage::TaskStore,
    selector: &str,
) -> Result<Option<crate::domain::records::Document>> {
    let live =
        |doc: &crate::domain::records::Document| doc.value["deleted"].as_bool() != Some(true);
    if let Some(doc) = store.document(KIND, selector)?
        && live(&doc)
    {
        return Ok(Some(doc));
    }
    let hits: Vec<_> = store
        .documents(KIND, 500)?
        .into_iter()
        .filter(live)
        .filter(|doc| {
            doc.value["label"].as_str() == Some(selector)
                || aliases_of(doc).iter().any(|alias| alias == selector)
        })
        .collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(hits.into_iter().next()),
        _ => anyhow::bail!(
            "标签/别名「{selector}」对应多条凭据，请用唯一标识：{}",
            hits.iter()
                .map(|doc| format!("{}（{}）", doc.id, label_of(doc)))
                .collect::<Vec<_>>()
                .join("、")
        ),
    }
}

/// 凭据的别名列表（可选字段 `aliases`）。
pub fn aliases_of(doc: &crate::domain::records::Document) -> Vec<String> {
    doc.value["aliases"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// 凭据的标签（给人看的那句）。
fn label_of(doc: &crate::domain::records::Document) -> String {
    doc.value["label"].as_str().unwrap_or("").to_owned()
}

/// 一条凭据（`records` 里 `credential` 类的 body）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// 真值：只在宿主侧流转，不进模型上下文。
    pub value: String,
    /// 人类可读的描述（标签）：名单里用来认出这条凭据是干什么的。
    #[serde(default)]
    pub label: String,
    /// **别名**：用户心智里的叫法（v1 时代凭据就叫这个名字），用于寻址。
    ///
    /// ⚠️ 只用于**寻址**（`secret get ftp`），**不**参与出站文本替换 ——
    /// 「上传作业」这种词在正文里太常见，当文本模式替换会把整段话换乱。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 更新时间（epoch 秒）。
    #[serde(default)]
    pub updated_at: i64,
}

/// vault 笔记里的凭据引用：`凭据「cred_xxxxxxxxxxxx」`（迁移时写进去的）。
static CITATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"凭据「(cred_[0-9a-f]{12})」").expect("citation regex"));

/// 裸凭据标识。
static ID_IN_TEXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"cred_[0-9a-f]{12}").expect("credential id regex"));

/// **出站解封**：把文本里的凭据标识换成真值。
///
/// 为什么要有这一步(2026-11-24 用户要求): LLM 上下文里只该出现**标识**
/// (`cred_xxx`)，真值等**网关把消息发出去之前**才落地 —— 于是
/// 模型、模型供应商的日志、会话记录里都没有明文，而用户收到的仍是能直接用的明文。
///
/// 只认两种形态，正文里的普通词不受影响:
///   - `凭据「cred_xxxxxxxxxxxx」` —— vault 笔记里的引用形式（整个引用替换成真值）;
///   - `cred_xxxxxxxxxxxx` —— 裸标识。
///
/// ⚠️ **标签/别名不参与文本替换**：「上传作业」这种词在正文里太常见，
/// 换了整段话就乱了。标签/别名只用于**寻址**（见 [`resolve`]）。
/// 凭据已删除或值为空 → 原样保留，不把空串塞给用户。
pub fn unseal_text(store: &dyn crate::ports::storage::TaskStore, text: &str) -> String {
    if !text.contains(ID_PREFIX) {
        return text.to_owned();
    }
    let lookup = |id: &str| -> Option<String> {
        let doc = store.document(KIND, id).ok().flatten()?;
        if doc.value["deleted"].as_bool() == Some(true) {
            return None;
        }
        let value = doc.value["value"].as_str()?.trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    };

    // 先换带引号的引用形式（否则会剩下「凭据「明文」」这种半吊子）
    let replaced = CITATION_RE
        .replace_all(text, |caps: &regex::Captures<'_>| {
            lookup(&caps[1]).unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned();
    ID_IN_TEXT_RE
        .replace_all(&replaced, |caps: &regex::Captures<'_>| {
            lookup(&caps[0]).unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::database::sqlite::SqliteTaskStore;
    use std::sync::Arc;

    fn fixture() -> (Arc<dyn crate::ports::storage::TaskStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn crate::ports::storage::TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        (store, dir)
    }

    fn put(store: &Arc<dyn crate::ports::storage::TaskStore>, id: &str, value: &str, label: &str) {
        store
            .put_document(
                KIND,
                id,
                None,
                &serde_json::json!({"value": value, "label": label, "updated_at": 0}),
            )
            .unwrap();
    }

    /// 出站解封: 裸标识换成明文。
    #[test]
    fn unseal_replaces_bare_identifier() {
        let (store, _dir) = fixture();
        put(&store, "cred_aaaaaaaaaaaa", "s3cret-pw", "上传作业");
        let out = unseal_text(store.as_ref(), "密码是 cred_aaaaaaaaaaaa 记住");
        assert_eq!(out, "密码是 s3cret-pw 记住");
    }

    /// vault 笔记里的引用形式 → 整个引用换成明文(不留「凭据「…」」半吊子)。
    #[test]
    fn unseal_replaces_citation_form() {
        let (store, _dir) = fixture();
        put(&store, "cred_bbbbbbbbbbbb", "ftp-pw", "下载课件");
        let out = unseal_text(store.as_ref(), "课件在 凭据「cred_bbbbbbbbbbbb」 里");
        assert_eq!(out, "课件在 ftp-pw 里");
    }

    /// 标签/普通词**不**参与替换 —— 「上传作业」在正文里太常见, 换了就乱。
    #[test]
    fn unseal_leaves_labels_untouched() {
        let (store, _dir) = fixture();
        put(&store, "cred_cccccccccccc", "s3cret-pw", "上传作业");
        let text = "上传作业的流程是…… 密码用 cred_cccccccccccc";
        let out = unseal_text(store.as_ref(), text);
        assert!(out.contains("上传作业的流程是"), "标签不该被换: {out}");
        assert!(out.contains("s3cret-pw"), "标识该被换: {out}");
    }

    /// 凭据已删除 → 原样保留标识, 别把墓碑当空串塞给用户。
    #[test]
    fn unseal_keeps_deleted_credential_untouched() {
        let (store, _dir) = fixture();
        put(&store, "cred_dddddddddddd", "x", "旧的");
        // 改成墓碑: 更新已有记录要带上 revision(同 credentials 插件的写法)
        let revision = store
            .document(KIND, "cred_dddddddddddd")
            .unwrap()
            .map(|doc| doc.revision);
        let written = store
            .put_document(
                KIND,
                "cred_dddddddddddd",
                revision,
                &serde_json::json!({"value": "x", "label": "旧的", "deleted": true}),
            )
            .unwrap();
        assert!(written, "墓碑没写进去");
        let out = unseal_text(store.as_ref(), "见 cred_dddddddddddd");
        assert!(out.contains("cred_dddddddddddd"), "墓碑不该被换: {out}");
    }

    /// 没有凭据引用的文本原样返回(快路径)。
    #[test]
    fn unseal_is_noop_without_identifiers() {
        let (store, _dir) = fixture();
        let text = "今天天气不错, 没有凭据";
        assert_eq!(unseal_text(store.as_ref(), text), text);
    }
}
