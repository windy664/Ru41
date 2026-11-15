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
    // 边界源码变了就重算布局 stamp（否则会拿旧 stamp 去比，漏掉布局变更）。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rustc-env=RU41_DEPS_STAMP={}", deps_stamp());
    println!("cargo:rustc-env=RU41_LAYOUT_STAMP={}", layout_stamp());
    println!("cargo:rustc-env=RU41_GIT_HASH={}", git_hash());
}

/// 边界布局 stamp：本 crate 源码的摘要。
///
/// 边界类型/签名一改，stamp 就变 → 旧插件在握手时被**明确拒绝**，而不是
/// 靠「记得 bump crate 版本」这种纪律兜底（漏一次就是 SIGBUS 崩溃循环）。
/// 宿主与插件编译自同一份 api crate 源码，摘要自然一致。
fn layout_stamp() -> String {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_rs(&dir.join("src"), &mut files);
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for file in files {
        let rel = file
            .strip_prefix(&dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for byte in rel.into_bytes().into_iter().chain(text.into_bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
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
