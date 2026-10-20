//! Obsidian 附属的入口层：Vault 核心服务在 `app::obsidian`（端口 `ports::vault`），
//! 这里只保留 QQ 快速记录命令与归档技能，业务逻辑不落地。

pub mod qq_command;

pub use qq_command::ObsidianQqCommand;
