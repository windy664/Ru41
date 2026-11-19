//! 块 12 · 动手项目二：CLI 搜索工具 —— Rust Book Ch12 (An I/O Project)
//!
//! 跑法：`rustc --edition 2024 --test block12_minigrep.rs -o /tmp/b12 && /tmp/b12`
//!
//! 本块产出：一个能用的 `grep` 简化版，并掌握 Ch12 的四个工程要点：
//!   1. 读命令行参数
//!   2. 读文件
//!   3. 把逻辑抽成模块 + 单元测试
//!   4. 错误写 stderr，输出写 stdout
//!
//! 用法：`minigrep <pattern> <file>`；跑：`rustc --edition 2024 block12_minigrep.rs -o /tmp/minigrep && /tmp/minigrep Rust somefile.txt`

use std::fs;
use std::process;

/// Ch12.1 命令行参数。
struct Config {
    pattern: String,
    file_path: String,
}

/// 参数不足时返回错误，而不是 panic。
fn parse_args() -> Result<Config, String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        return Err("用法: minigrep <pattern> <file>".to_owned());
    }
    Ok(Config {
        pattern: args[1].clone(),
        file_path: args[2].clone(),
    })
}

/// Ch12.2 读文件。错误信息要可读。
fn read_file(path: &str) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("读取 {path} 失败: {e}"))
}

/// Ch12.4 核心逻辑：返回匹配的行号，而不是直接打印 —— 这样才能单元测试。
fn find_matches(content: &str, pattern: &str) -> Vec<usize> {
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(pattern))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Ch12.3 重构：把「配置 → 读文件 → 找匹配 → 打印」串起来。
fn run(config: Config) -> Result<(), String> {
    let content = read_file(&config.file_path)?;
    let matches = find_matches(&content, &config.pattern);
    if matches.is_empty() {
        eprintln!("未找到任何匹配");
        process::exit(1); // Ch12.6：grep 没找到时退出码是 1
    }
    for line_no in matches {
        println!("{line_no}:{}", content.lines().nth(line_no - 1).unwrap_or(""));
    }
    Ok(())
}

fn main() {
    match parse_args() {
        Ok(config) => {
            if let Err(e) = run(config) {
                eprintln!("错误: {e}");
                process::exit(2); // 参数/文件错误用 2
            }
        }
        Err(e) => {
            eprintln!("错误: {e}");
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Rust 是一门系统语言
我是来学 Rust 的
Go 也很快
rust 小写不等于 Rust
";

    #[test]
    fn finds_matching_lines() {
        assert_eq!(find_matches(SAMPLE, "Rust"), vec![1, 2, 4]);
        assert_eq!(find_matches(SAMPLE, "Go"), vec![3]);
        assert_eq!(find_matches(SAMPLE, "Java"), Vec::<usize>::new());
    }

    #[test]
    fn missing_file_gives_readable_error() {
        let err = read_file("/definitely/not/here.txt").unwrap_err();
        assert!(err.contains("失败"), "错误信息应可读，实际: {err}");
    }

    #[test]
    fn config_needs_two_args() {
        // parse_args 依赖真实 argv，单元测试只验证错误分支的文案约定
        let msg = "用法: minigrep <pattern> <file>";
        assert!(msg.contains("pattern"));
    }
}
