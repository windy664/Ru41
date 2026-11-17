use super::PiAgent;
use crate::domain::error::Ru41Error;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 候选模型：供应方 + 模型 ID；主模型与 fallback 共同注入 Pi virtual model。
#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub model: String,
    pub provider: Option<String>,
}

impl Candidate {
    /// 拆成 Pi 认识的 provider 与物理模型 ID；`provider/model` 里只有第一段是 provider。
    fn parts(&self) -> (String, String) {
        match self.model.split_once('/') {
            Some((provider, model)) => (provider.to_owned(), model.to_owned()),
            None => (
                self.provider.clone().unwrap_or_default(),
                self.model.clone(),
            ),
        }
    }

    /// 展示名（供应方/模型），用于日志与用量归属。
    pub(super) fn label(&self) -> String {
        if self.model.contains('/') {
            return self.model.clone();
        }
        match &self.provider {
            Some(provider) if !provider.is_empty() => format!("{provider}/{}", self.model),
            _ => self.model.clone(),
        }
    }
}

/// Pi 支持的兼容端点协议。
const SUPPORTED_APIS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];

impl PiAgent {
    /// 兼容端点配置：models.json 内容；密钥以 `$ENV` 引用写入，真实值只留在进程环境里。
    ///
    /// `providers` 里每个供应方是独立条目（各自的 baseUrl / api / apiKey），
    /// 这样 fallback 链可以跨供应方——主供应方限流时切到另一家的模型。
    pub fn models_json(&self, model: &str, provider: Option<&str>) -> Result<Value> {
        let primary_id = provider
            .filter(|p| !p.is_empty())
            .unwrap_or("ru41")
            .to_owned();
        let model_id = model.split_once('/').map_or(model, |(_, id)| id);

        // 每个供应方负责哪些模型：主模型归主供应方，fallback 按 `供应方/模型` 归位。
        let mut assigned: BTreeMap<String, Vec<String>> = BTreeMap::new();
        assigned
            .entry(primary_id.clone())
            .or_default()
            .push(model_id.to_owned());
        for fallback in &self.fallback_models {
            let candidate = Candidate {
                model: fallback.clone(),
                provider: self.provider.clone(),
            };
            let (pid, id) = candidate.parts();
            let list = assigned.entry(pid).or_default();
            if !list.contains(&id) {
                list.push(id);
            }
        }

        let mut providers = serde_json::Map::new();
        for (pid, ids) in assigned {
            let (base_url, api, api_key_env, mut input) = if pid == primary_id {
                (
                    self.base_url.clone(),
                    self.api.clone(),
                    self.api_key_env.clone(),
                    self.input.clone(),
                )
            } else {
                match self.providers.get(&pid) {
                    Some(spec) => (
                        spec.base_url.clone(),
                        spec.api.clone(),
                        spec.api_key_env.clone(),
                        spec.input.clone(),
                    ),
                    None => {
                        return Err(Ru41Error::Config(format!(
                            "fallback 模型属于未配置的供应方 {pid}：在 [agent.pi.providers.{pid}] 里补 base_url / api / api_key_env"
                        ))
                        .into());
                    }
                }
            };
            let api = api.unwrap_or_else(|| "anthropic-messages".to_owned());
            ensure!(
                SUPPORTED_APIS.contains(&api.as_str()),
                Ru41Error::Config(format!(
                    "pi api must be one of {} (got {api})",
                    SUPPORTED_APIS.join(", ")
                ))
            );
            // 主供应方没配端点 = 用 Pi 自带的 provider 目录，不在 models.json 里重复声明。
            if pid == primary_id && base_url.is_none() {
                continue;
            }
            if input.is_empty() {
                input = self.input.clone();
            }
            let mut provider = serde_json::Map::new();
            provider.insert(
                "baseUrl".into(),
                Value::String(base_url.unwrap_or_default()),
            );
            provider.insert("api".into(), Value::String(api));
            if let Some(env_name) = api_key_env {
                provider.insert("apiKey".into(), Value::String(format!("${env_name}")));
            }
            let mut entries = Vec::new();
            for id in ids {
                let mut entry = serde_json::Map::new();
                entry.insert("id".into(), Value::String(id));
                if !input.is_empty() {
                    entry.insert("input".into(), json!(input));
                }
                entries.push(Value::Object(entry));
            }
            provider.insert("models".into(), Value::Array(entries));
            providers.insert(pid, Value::Object(provider));
        }

        let mut root = serde_json::Map::new();
        root.insert("providers".into(), Value::Object(providers));
        Ok(Value::Object(root))
    }

    /// 把 models.json 写到运行目录下的临时 agent 目录，并返回该目录。
    pub(super) fn write_models_json(
        &self,
        run_dir: &Path,
        candidate: &Candidate,
    ) -> Result<PathBuf> {
        let root = self.models_json(&candidate.model, candidate.provider.as_deref())?;
        let agent_dir = run_dir.join(".pi-agent");
        std::fs::create_dir_all(&agent_dir)
            .with_context(|| format!("创建 Pi agent 目录失败 {}", agent_dir.display()))?;
        let models_path = agent_dir.join("models.json");
        std::fs::write(
            &models_path,
            serde_json::to_vec_pretty(&root).context("序列化 models.json 失败")?,
        )
        .with_context(|| format!("写 models.json 失败 {}", models_path.display()))?;
        Ok(agent_dir)
    }

    /// 生成 Pi 原生 virtual model：同一个 Pi 进程内处理 retry 时的模型切换。
    /// 密钥不进入 extension；物理模型认证仍由 models.json / Pi 内置 provider 处理。
    pub(super) fn write_fallback_extension(
        &self,
        run_dir: &Path,
        chain: &[Candidate],
    ) -> Result<PathBuf> {
        let chain_json = serde_json::to_string(
            &chain
                .iter()
                .map(|candidate| {
                    let (provider, id) = candidate.parts();
                    json!({"provider": provider, "id": id})
                })
                .collect::<Vec<_>>(),
        )
        .context("序列化 fallback 模型链失败")?;
        let source = format!(
            r#"const CHAIN = {chain_json};

function sameModel(model, candidate) {{
  return model && model.provider === candidate.provider && model.id === candidate.id;
}}

function pick(request, ctx, candidate) {{
  const model = ctx.modelRegistry.find(candidate.provider, candidate.id);
  if (!model) throw new Error(`Model ${{candidate.provider}}/${{candidate.id}} is not in the catalog`);
  return {{ model, thinkingLevel: request.thinkingLevel }};
}}

export default function (pi) {{
  pi.registerVirtualModel({{
    provider: "ru41",
    id: "auto",
    name: "Ru41 Auto Fallback",
    thinkingLevels: ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
    route(request, ctx) {{
      if (request.reason === "retry" && request.failed?.model) {{
        const failed = CHAIN.findIndex((candidate) => sameModel(request.failed.model, candidate));
        if (failed >= 0 && failed + 1 < CHAIN.length) return pick(request, ctx, CHAIN[failed + 1]);
        return pick(request, ctx, CHAIN[CHAIN.length - 1]);
      }}
      if (request.reason !== "user" && request.previous?.model) {{
        const previous = CHAIN.findIndex((candidate) => sameModel(request.previous.model, candidate));
        if (previous >= 0) return pick(request, ctx, CHAIN[previous]);
      }}
      return pick(request, ctx, CHAIN[0]);
    }},
  }});
}}
"#
        );
        let path = run_dir.join("ru41-fallback.ts");
        std::fs::write(&path, source)
            .with_context(|| format!("写 Pi fallback extension 失败 {}", path.display()))?;
        Ok(path)
    }

    /// 候选链：主模型在前，备用模型按配置顺序在后。
    pub(super) fn candidates(&self) -> Vec<Candidate> {
        let mut list = Vec::new();
        match &self.model {
            Some(model) => list.push(Candidate {
                model: model.clone(),
                provider: self.provider.clone(),
            }),
            // 没配模型时交给 Pi 用它自己的默认模型（候选的 model 为空表示不传 --model）。
            None if self.fallback_models.is_empty() => list.push(Candidate {
                model: String::new(),
                provider: self.provider.clone(),
            }),
            None => {}
        }
        for fallback in &self.fallback_models {
            let (provider, model) = match fallback.split_once('/') {
                Some((provider, model)) => (Some(provider.to_owned()), model.to_owned()),
                None => (self.provider.clone(), fallback.clone()),
            };
            list.push(Candidate { model, provider });
        }
        list
    }
}
