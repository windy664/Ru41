//! 配置类型定义在 ru41-plugin-api::config（Config 跨宿主↔插件边界，
//! 必须是同一份定义）；本文件保持原路径，并持有依赖宿主注册表的告警逻辑。

pub use ru41_plugin_api::config::*;

/// 校验 [sources] 下的键都属于已注册模块；未知键只警告（服务器存量配置
/// 可能带着新版模块的键跑旧二进制，报错会妨碍回滚）。
/// 须在插件加载之后调用，否则插件模块的配置段会误报。
pub fn warn_unknown_source_keys(sources: &SourcesSection) {
    let known: Vec<&str> = crate::modules::sorted()
        .iter()
        .filter_map(|def| def.config_key)
        .collect();
    for key in sources.keys() {
        if !known.contains(&key) {
            eprintln!("[config] 警告：[sources.{key}] 不属于任何已注册模块，将被忽略");
        }
    }
}
