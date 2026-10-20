use crate::domain::{AgentResult, RunContract};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
pub struct BackendDescriptor {
    /// 稳定的路由键，与底座内部选用的模型无关。
    pub id: String,
    pub model: Option<String>,
    pub contracts: Vec<RunContract>,
    /// 仅声明适配器支持的能力，不代表底层提供方的全部能力。
    pub supports_resume: bool,
}

pub struct AgentRequest {
    pub context: Option<RunContext>,
    pub prompt: String,
    pub run_dir: PathBuf,
    pub contract: RunContract,
}

pub struct AgentOutput {
    pub result: AgentResult,
    /// 不透明的提供方标识；不能跨底座移植。
    pub session_id: Option<String>,
}

/// 一个运行时底座，而不仅是文本补全 API。
/// 实现负责把各自协议翻译为请求的契约。
/// 丢弃运行 future 必须停止其持有的工作；外部 API 必须说明
/// 服务端取消的限制。不做自动底座降级。
pub trait AgentBackend: Send + Sync {
    fn descriptor(&self) -> BackendDescriptor;
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + '_>>;
}

impl<T: AgentBackend + ?Sized> AgentBackend for Arc<T> {
    fn descriptor(&self) -> BackendDescriptor {
        (**self).descriptor()
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + '_>> {
        (**self).run(request)
    }
}

/// 启动时显式装配；不是动态插件加载器。
#[derive(Default)]
pub struct BackendRegistry {
    backends: BTreeMap<String, Arc<dyn AgentBackend>>,
    pub skills: Vec<crate::skills::SkillPackage>,
}
impl BackendRegistry {
    pub fn register(&mut self, backend: impl AgentBackend + 'static) -> Result<()> {
        let descriptor = backend.descriptor();
        ensure!(!descriptor.id.trim().is_empty(), "backend ID is required");
        ensure!(
            !descriptor.contracts.is_empty(),
            "backend must declare a supported contract"
        );
        ensure!(
            !self.backends.contains_key(&descriptor.id),
            "duplicate backend ID: {}",
            descriptor.id
        );
        self.backends.insert(descriptor.id, Arc::new(backend));
        Ok(())
    }
    pub fn get(&self, id: &str) -> Result<Arc<dyn AgentBackend>> {
        self.backends
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("backend is not registered: {id}"))
    }
    pub fn descriptors(&self) -> Vec<BackendDescriptor> {
        self.backends.values().map(|b| b.descriptor()).collect()
    }
}

#[derive(Clone)]
pub struct RunContext {
    pub run_id: String,
    pub interactions: Arc<dyn crate::ports::interaction::InteractionHandler>,
    pub conversation_id: Option<String>,
    pub resume_session_id: Option<String>,
    pub skills: Vec<crate::skills::SkillPackage>,
}
