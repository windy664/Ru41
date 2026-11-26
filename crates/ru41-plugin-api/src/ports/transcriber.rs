//! 插件间协作：转写能力注册表。
//!
//! ## 为什么需要它
//!
//! 两个插件都要转写同一类东西，但强弱不同：
//! - `tingwu`（通义听悟）：云端、**带说话人分离 + 时间戳**，但要账号额度；
//! - `lecture`：本地 whisper.cpp / paraformer，**没有说话人分离**，但不要额度。
//!
//! 2026-11-26 之前的现状是「谁先注册 QQ 消息处理器谁先拿到音频」——lecture 抢先把
//! 课堂录音截走，tingwu 根本没机会上场，用户要的「听悟优先、本地兜底」顺序落不了地。
//!
//! 做法：**转写这一步走注册表**，谁需要转写谁去查表，按优先级挑能力；
//! 「谁的处理器先接消息」与「用谁转写」彻底分开。
//!
//! ## 进程边界
//!
//! 注册表是**进程级**的（`OnceLock`）。每个 ru41 容器都会加载全部插件，
//! 所以 worker 进程里 tingwu 的转写器同样注册好了，lecture 查得到。

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;

use crate::ports::storage::TaskStore;

/// 一个可用的转写能力（插件提供）。
///
/// `transcribe` 返回**纯文本**（不含说话人/时间戳结构）——
/// 需要结构化逐字稿的调用方应该直接用 tingwu 插件的 CLI，
/// 这里给的是「要一段文字」的通用场景（课堂笔记、摘要、检索…）。
pub trait Transcriber: Send + Sync {
    /// 能力标识（如 `tingwu`）。注册表按它去重与查找。
    fn id(&self) -> &str;

    /// 转写一个本地音视频文件。
    ///
    /// - `store`：凭据托管库等运行时资源（插件自己取 cookie/配置用）。
    /// - `path`：本地文件路径。
    fn transcribe<'a>(
        &'a self,
        store: Arc<dyn TaskStore>,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;
}

type Registry = Mutex<HashMap<String, Arc<dyn Transcriber>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 注册一个转写能力（同 id 覆盖旧的）。
///
/// 插件在**加载入口**里调用：每个 ru41 容器都会加载全部插件，
/// 所以每个进程里注册表都是齐的。
pub fn register_transcriber(transcriber: Arc<dyn Transcriber>) {
    let mut guard = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.insert(transcriber.id().to_owned(), transcriber);
}

/// 按 id 找一个转写能力。
pub fn find_transcriber(id: &str) -> Option<Arc<dyn Transcriber>> {
    let guard = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(id).cloned()
}

/// 列出全部已注册的转写能力（诊断用）。
pub fn transcribers() -> Vec<Arc<dyn Transcriber>> {
    let guard = registry().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        id: String,
        calls: Arc<AtomicUsize>,
    }

    impl Transcriber for Fake {
        fn id(&self) -> &str {
            &self.id
        }

        fn transcribe<'a>(
            &'a self,
            _store: Arc<dyn TaskStore>,
            _path: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok("文本".to_owned()) })
        }
    }

    #[test]
    fn registers_and_finds_by_id() {
        let calls = Arc::new(AtomicUsize::new(0));
        let fake = Arc::new(Fake {
            id: "fake-transcriber".to_owned(),
            calls: calls.clone(),
        });
        register_transcriber(fake);
        let found = find_transcriber("fake-transcriber").expect("应能按 id 找到");
        assert_eq!(found.id(), "fake-transcriber");
        assert!(
            transcribers().iter().any(|t| t.id() == "fake-transcriber"),
            "注册表里应列出它"
        );
        assert!(find_transcriber("not-exists").is_none());
    }

    #[test]
    fn same_id_overrides_previous() {
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        register_transcriber(Arc::new(Fake {
            id: "dup".to_owned(),
            calls: first.clone(),
        }));
        register_transcriber(Arc::new(Fake {
            id: "dup".to_owned(),
            calls: second.clone(),
        }));
        let found = find_transcriber("dup").expect("应能按 id 找到");
        assert_eq!(found.id(), "dup");
        let count = transcribers()
            .iter()
            .filter(|t| t.id() == "dup")
            .count();
        assert_eq!(count, 1, "同 id 只应留一个（后者覆盖前者）");
    }
}
