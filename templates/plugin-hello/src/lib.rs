//! ru41 动态插件模板（hello）：最小可加载示例。
//!
//! 构建：`cargo build --release` → `target/release/libru41_plugin_hello.so`
//! 部署：把 .so 复制到宿主机插件目录（默认 /etc/ru41/plugins/），
//!       重启对应容器即可；`ru41 schedule list` 应出现 hello，
//!       `ru41 hello poll` 输出 JSON。
//!
//! 注意：插件不用 linkme 自注册，必须通过入口函数显式返回 DEF。

use ru41::ingress::schedule::PollJob;
use ru41::modules::{JobSchedule, ModuleDef};

/// 宿主加载时按固定符号名查找。
///
/// # Safety
/// 插件与宿主必须同一 rustc + 同一 ru41 版本编译（入口握手校验）。
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("hello", || DEF)
}

/// 模块自描述：与内建模块的 DEF 完全同构。
/// 注意返回的是值不是引用——宿主会 Box::leak 成 'static。
static DEF: ModuleDef = ModuleDef {
    name: "hello",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: None, // 无 [sources.hello] 配置段
    schedule: |_config| vec![JobSchedule::simple("hello", 3600, true, None)],
    tracker: |_config, _store, name| {
        Ok((name == "hello").then(|| -> Box<dyn PollJob> { Box::new(HelloJob) }))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};

struct HelloJob;

impl PollJob for HelloJob {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(3600)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async { Ok(serde_json::json!({"ok": true, "from": "hello plugin"})) })
    }
}
