use crate::domain::records::Interaction;
use anyhow::Result;
use serde_json::Value;
use std::{future::Future, pin::Pin};

pub trait InteractionHandler: Send + Sync {
    fn request(
        &self,
        request: Interaction,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>;
}
