//! 嵌入 git commit hash 供插件 ABI 握手（pkg 版本不变时类型布局仍可能漂移）。
//! 优先读 RU41_GIT_HASH_OVERRIDE（容器内构建时 .git 是 worktree 指针文件，
//! 容器里解析不到真 gitdir，由构建脚本从宿主机显式注入）；其次 git rev-parse；
//! 都没有则 "unknown"，握手降级为版本号 + 告警。
fn main() {
    println!("cargo:rerun-if-env-changed=RU41_GIT_HASH_OVERRIDE");
    let hash = std::env::var("RU41_GIT_HASH_OVERRIDE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RU41_GIT_HASH={hash}");
}
