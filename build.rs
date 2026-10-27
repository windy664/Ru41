//! 嵌入 git commit hash 供插件 ABI 握手（pkg 版本不变时类型布局仍可能漂移）。
//! 无 git 元数据（如容器内构建）时退化为 "unknown"，握手降级为版本号 + 告警。
fn main() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RU41_GIT_HASH={hash}");
}
