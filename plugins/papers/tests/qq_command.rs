//! 精读 QQ 命令的别名与帮助文本（原 tests/qq_commands.rs 片段，随模块迁入）。

use ru41::channels::qq::commands;
use ru41::ports::commands::CommandHandler;
use ru41_plugin_papers::{ArxivClient, PapersQqCommand};
use std::sync::Arc;

#[test]
fn papers_matches_aliases_and_help() {
    let papers = PapersQqCommand::new(ArxivClient::new(), String::new(), "codex".into());
    for verb in ["精读", "deep", "deepread"] {
        assert!(papers.matches(verb));
    }
    assert!(!papers.matches("精读一下"));

    let handlers: Vec<Arc<dyn CommandHandler>> = vec![Arc::new(papers)];
    let help = commands::help_text(&handlers);
    assert!(help.contains("## Ru41 机器人"));
    assert!(help.contains("精读 <arxiv-id>"));
}
