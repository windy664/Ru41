use anyhow::{Context, Result};
use serde::Deserialize;

use crate::service::CompshareConfig;

/// 恒源云 light-gpu 库存监控配置；compshare poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompshareSection {
    /// 恒源云 API 基址；缺省为正式 API，测试可覆盖。
    pub base_url: Option<String>,
    /// 项目 ID，创建页请求中的 ProjectId。
    pub project_id: Option<String>,
    /// 目标镜像 ID。
    pub image_id: Option<String>,
    /// 创建页当前选择的 region/zone/GPU 类型与系统盘大小。
    pub region: Option<String>,
    pub zone: Option<String>,
    pub gpu_type: Option<String>,
    pub disk_size_gb: Option<u64>,
    /// compshare run 的轮询间隔（秒），默认 60。
    pub interval_secs: Option<u64>,
    /// 有货时是否经默认通知出口提醒；默认 true。
    pub notify: Option<bool>,
    /// 控制台 Cookie；建议用环境变量 RU41_COMPSHARE_COOKIE 注入，勿提交。
    pub cookie: Option<String>,
}

impl CompshareSection {
    pub const DEFAULT_BASE_URL: &'static str = "https://api.compshare.cn/";
    pub const DEFAULT_IMAGE_ID: &'static str = "compshareImage-1t1vuerl80np";
    pub const DEFAULT_REGION: &'static str = "cn-wlcb";
    pub const DEFAULT_ZONE: &'static str = "cn-wlcb-01";
    pub const DEFAULT_GPU_TYPE: &'static str = "P40";
    pub const DEFAULT_DISK_SIZE_GB: u64 = 100;
    pub const DEFAULT_INTERVAL_SECS: u64 = 60;

    /// 由 [sources.compshare] 解析出运行时配置；环境变量优先，默认值只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<CompshareConfig> {
        let cookie = std::env::var("RU41_COMPSHARE_COOKIE")
            .ok()
            .or_else(|| self.cookie.clone())
            .filter(|c| !c.trim().is_empty())
            .context(
                "compshare cookie required: RU41_COMPSHARE_COOKIE or sources.compshare.cookie",
            )?;
        Ok(CompshareConfig {
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_BASE_URL.into()),
            project_id: self
                .project_id
                .clone()
                .context("sources.compshare.project_id required")?,
            image_id: self
                .image_id
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_IMAGE_ID.into()),
            region: self
                .region
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_REGION.into()),
            zone: self
                .zone
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_ZONE.into()),
            gpu_type: self
                .gpu_type
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_GPU_TYPE.into()),
            disk_size_gb: self.disk_size_gb.unwrap_or(Self::DEFAULT_DISK_SIZE_GB),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify_target,
            cookie,
        })
    }

    /// 配置是否已齐；schedule list 与装配共用同一套判定。
    pub fn is_ready(&self) -> bool {
        self.project_id.is_some()
            && (self.cookie.is_some() || std::env::var("RU41_COMPSHARE_COOKIE").is_ok())
    }
}
