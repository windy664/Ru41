//! 微助教 CLI 专属处理（main.rs 只做参数路由，模块内部细节不放核心入口）。

use super::config::extract_openid;
use super::source::TeachermateClient;
use anyhow::{Context, Result};

/// bind：从粘贴内容提取 openid → 原子写入 openid_path → 探测学籍验证。
/// 走默认官方基址；验证失败（失效/网络）时 openid 仍已落盘，便于排查。
pub async fn bind_json(link: &str, openid_path: &std::path::Path) -> Result<serde_json::Value> {
    bind_json_with_base(link, openid_path, super::source::DEFAULT_API_BASE).await
}

/// 测试用：基址可指向本地 fixture 服务。
pub async fn bind_json_with_base(
    link: &str,
    openid_path: &std::path::Path,
    api_base: &str,
) -> Result<serde_json::Value> {
    let openid = extract_openid(link)
        .context("无法从输入提取 openid：应为服务号链接（含 openid=）或 32 位串")?;
    if let Some(parent) = openid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    let tmp = openid_path.with_extension("tmp");
    std::fs::write(&tmp, &openid)
        .with_context(|| format!("写入 openid 失败: {}", tmp.display()))?;
    std::fs::rename(&tmp, openid_path)
        .with_context(|| format!("落盘 openid 失败: {}", openid_path.display()))?;

    let client = TeachermateClient::with_api_base(openid.clone(), api_base.to_owned());
    let roles = client.role().await?;
    Ok(serde_json::json!({
        "openid": openid,
        "openid_path": openid_path.display().to_string(),
        "valid": true,
        "student": roles.first().map(|role| role.summary()),
    }))
}
