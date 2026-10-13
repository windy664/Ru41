use crate::domain::{AgentResult, RunContract};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc};

#[derive(Clone, Debug, Serialize)]
pub struct BackendDescriptor {
    /// Stable routing key, independent of the model selected inside the backend.
    pub id: String,
    pub model: Option<String>,
    pub contracts: Vec<RunContract>,
    /// Declares adapter support, not everything the underlying provider can do.
    pub supports_resume: bool,
}

pub struct AgentRequest {
    pub prompt: String,
    pub run_dir: PathBuf,
    pub contract: RunContract,
}

pub struct AgentOutput {
    pub result: AgentResult,
    /// Opaque provider identifier; not portable between backends.
    pub session_id: Option<String>,
}

/// A runtime backend, not merely a text-completion API.
/// Implementations translate their protocol into the requested contract.
/// Dropping the run future must stop owned work; external APIs must document
/// any server-side cancellation limitations. No automatic backend fallback.
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

/// Explicit startup assembly; this is not a dynamic plugin loader.
#[derive(Default)]
pub struct BackendRegistry {
    backends: BTreeMap<String, Arc<dyn AgentBackend>>,
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
