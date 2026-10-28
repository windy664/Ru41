//! 嵌入两类握手元数据：
//! 1. RU41_DEPS_STAMP：ABI 相关外部依赖（tokio/serde 等）在 lockfile 中解析版本的
//!    FNV-1a 摘要。宿主与插件解析到不同版本时类型布局可能漂移，握手硬拒。
//!    lockfile 从本 crate 清单目录向上查找（宿主与插件构建都解析到仓库根的同一份
//!    Cargo.lock；插件目录的 lockfile 由 deploy 脚本从根同步）。
//! 2. RU41_GIT_HASH：诊断用（不再是握手硬门），规则与宿主 build.rs 相同。

/// 布局会跨边界的外部依赖；版本变化必须反映到 stamp。
const STAMPED_PACKAGES: &[&str] = &[
    "anyhow",
    "chrono",
    "futures-util",
    "schemars",
    "serde",
    "serde_json",
    "tokio",
    "toml",
];

fn main() {
    println!("cargo:rerun-if-env-changed=RU41_GIT_HASH_OVERRIDE");
    println!("cargo:rustc-env=RU41_DEPS_STAMP={}", deps_stamp());
    println!("cargo:rustc-env=RU41_GIT_HASH={}", git_hash());
}

fn deps_stamp() -> String {
    let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lock = loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            break std::fs::read_to_string(&candidate).ok();
        }
        if !dir.pop() {
            break None;
        }
    };
    let Some(text) = lock else {
        return "unknown".into();
    };
    // 解析 [[package]] name/version 对，只留关注的包，排序后 FNV-1a。
    let mut pinned: Vec<(String, String)> = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name = \"") {
            name = rest.strip_suffix('"').map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("version = \"") {
            if let (Some(n), Some(v)) = (name.take(), rest.strip_suffix('"')) {
                if STAMPED_PACKAGES.contains(&n.as_str()) {
                    pinned.push((n, v.to_owned()));
                }
            }
        }
    }
    pinned.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pinned
        .iter()
        .flat_map(|(n, v)| format!("{n}={v};").into_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn git_hash() -> String {
    std::env::var("RU41_GIT_HASH_OVERRIDE")
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
        .unwrap_or_else(|| "unknown".into())
}
