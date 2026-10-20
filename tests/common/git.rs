//! 集成测试共享 helper：git 命令执行（中文路径断言需要 core.quotepath=false）。

use std::path::Path;
use std::process::Command;

/// 在 dir 下执行 git 参数并断言成功，返回 trim 后的 stdout。
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-c")
        .arg("core.quotepath=false")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
