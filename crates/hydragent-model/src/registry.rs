use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::custom_openai::{CustomOpenAIClient, CustomProviderConfig};
use crate::model_trait::ModelProvider;
use crate::ollama::{OllamaClient, OllamaProviderConfig};
use crate::openrouter::OpenRouterClient;
use crate::profiles::CostTier;

/// Backward-compatible alias: the old code base called this `ModelRegistry`.
pub type ModelRegistry = ProviderRegistry;

/// Kinds of provider backend we know how to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    /// OpenRouter-compatible provider.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Ollama local server.
    #[serde(rename = "ollama")]
    Ollama,
    /// Generic OpenAI-compatible endpoint.
    #[serde(rename = "custom_openai")]
    CustomOpenAi,
    /// Explicit custom endpoint.
    #[serde(rename = "custom_endpoint")]
    CustomEndpoint,
}

impl ProviderKind {
    /// Normalize a free-form provider id string to a known kind.
    pub fn from_id(id: &str) -> Self {
        match ProviderRegistry::normalize_provider_id(id) {
            "openrouter" => ProviderKind::OpenRouter,
            "openai" => ProviderKind::CustomOpenAi,
            "ollama" => ProviderKind::Ollama,
            "lmstudio" => ProviderKind::CustomOpenAi,
            _ => ProviderKind::CustomOpenAi,
        }
    }
}

/// How a provider authenticates requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// No authentication required (e.g. local Ollama).
    #[default]
    None,
    /// Standard `Authorization: Bearer <key>`.
    ApiKey,
    /// Provider-specific custom auth header.
    Custom,
}

fn default_timeout() -> u64 {
    180
}

fn default_max_retries() -> u32 {
    3
}

/// A declarative provider definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderDefinition {
    pub id: String,
    pub display_name: String,
    pub kind: ProviderKind,
    pub default_base_url: String,
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub supports_custom_models: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub default_headers: HashMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub default_params: HashMap<String, serde_yaml::Value>,
}

/// A declarative model definition under a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDefinition {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub api_model_id: String,
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub streaming: bool,
    pub max_input_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub request_headers: HashMap<String, String>,
    #[serde(default)]
    pub default_params: HashMap<String, serde_yaml::Value>,
    pub cost_per_1k: Option<f64>,
    pub cost_tier: Option<CostTier>,
}

impl ModelDefinition {
    /// Capability flags advertised by this model.
    pub fn capability_flags(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.tool_calling {
            flags.push("tools");
        }
        if self.vision {
            flags.push("vision");
        }
        if self.reasoning {
            flags.push("reasoning");
        }
        if self.streaming {
            flags.push("streaming");
        }
        flags
    }

    /// True if this model satisfies all requested capability names.
    pub fn satisfies_capabilities(&self, requirements: &[String]) -> bool {
        requirements.iter().all(|req| match req.trim().to_lowercase().as_str() {
            "tools" | "tool_calling" => self.tool_calling,
            "vision" => self.vision,
            "reasoning" => self.reasoning,
            "streaming" => self.streaming,
            "any" | "" => true,
            _ => true,
        })
    }

    /// Higher scores are better for the given role.
    pub fn role_score(&self, role: &str) -> u32 {
        let role = role.trim().to_lowercase();
        let mut score = 0_u32;

        if self.streaming {
            score += 5;
        }
        if let Some(tier) = self.cost_tier {
            score += match tier {
                CostTier::Free => 8,
                CostTier::Cheap => 6,
                CostTier::Standard => 4,
                CostTier::Premium => 2,
                CostTier::Any => 0,
            };
        }

        match role.as_str() {
            "coding" => {
                if self.tool_calling { score += 40; }
                if self.reasoning { score += 15; }
            }
            "planning" | "research" => {
                if self.reasoning { score += 40; }
                if self.tool_calling { score += 10; }
            }
            "inline_chat" => {
                if self.streaming { score += 20; }
            }
            "utility" => {
                if self.tool_calling { score += 10; }
            }
            _ => {}
        }

        score
    }
}

/// Runtime resolution result: a concrete provider + model pair.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub provider_id: String,
    pub model_id: String,
    pub api_model_id: String,
    pub role: String,
}

/// Errors that can occur while loading or using the registry.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("unsupported registry version: {0}")]
    UnsupportedVersion(u32),
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("invalid model ref: {0}")]
    InvalidModelRef(String),
}

/// Internal YAML file shape.
#[derive(Debug, Deserialize)]
struct RegistryFile {
    version: u32,
    #[serde(default)]
    defaults: HashMap<String, String>,
    providers: Vec<ProviderDefinition>,
    #[serde(default)]
    models: Vec<ModelDefinition>,
}

/// A registry of providers and models.
///
/// Providers and models are keyed by their declared ids. Models are also
/// addressable by aliases and by role defaults.
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderDefinition>,
    models: HashMap<String, ModelDefinition>,
    aliases: HashMap<String, String>,
    role_defaults: HashMap<String, String>,
}

impl ProviderRegistry {
    /// Load a registry from a YAML file.
    pub fn load_from_yaml<P: AsRef<Path>>(path: P) -> Result<Self, RegistryError> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Self::load_from_yaml_str(&text)
    }

    /// Load a registry from a YAML string.
    pub fn load_from_yaml_str(text: &str) -> Result<Self, RegistryError> {
        let file: RegistryFile = serde_yaml::from_str(text)?;
        if file.version != 1 {
            return Err(RegistryError::UnsupportedVersion(file.version));
        }

        let mut providers = HashMap::new();
        for p in file.providers {
            providers.insert(p.id.clone(), p);
        }

        let mut models = HashMap::new();
        let mut aliases = HashMap::new();
        for m in file.models {
            if !providers.contains_key(&m.provider_id) {
                return Err(RegistryError::UnknownProvider(m.provider_id.clone()));
            }
            let full_id = format!("{}/{}", m.provider_id, m.id);
            models.insert(full_id.clone(), m.clone());
            aliases.insert(format!("{}/{}", m.provider_id, m.id), full_id.clone());
            for alias in &m.aliases {
                aliases.insert(format!("{}/{}", m.provider_id, alias), full_id.clone());
            }
        }

        // Validate role defaults point to known models.
        for (role, model_ref) in &file.defaults {
            if !model_ref.contains('/') {
                return Err(RegistryError::InvalidModelRef(format!(
                    "role default for '{}' must be provider/model, got '{}'",
                    role, model_ref
                )));
            }
            let key = aliases.get(model_ref).map(|s| s.as_str()).unwrap_or(model_ref);
            if !models.contains_key(key) {
                return Err(RegistryError::UnknownModel(format!(
                    "role default for '{}' -> '{}'",
                    role, model_ref
                )));
            }
        }

        Ok(Self {
            providers,
            models,
            aliases,
            role_defaults: file.defaults,
        })
    }

    /// Built-in registry shipped with the binary, used when no external file
    /// is provided.
    pub fn builtin_default() -> Self {
        Self::load_from_yaml_str(BUILTIN_REGISTRY_YAML)
            .expect("builtin registry is valid YAML")
    }

    /// Look up a provider by id (with normalization).
    pub fn provider(&self, id: &str) -> Option<&ProviderDefinition> {
        let normalized = Self::normalize_provider_id(id);
        self.providers.get(normalized)
    }

    /// Look up a model by its full id `provider_id/model_id` (with normalization).
    pub fn model(&self, id: &str) -> Option<&ModelDefinition> {
        let (provider_id, model_id) = id.split_once('/')?;
        let normalized_provider = Self::normalize_provider_id(provider_id);
        let key = format!("{}/{}", normalized_provider, model_id);
        self.models.get(&key).or_else(|| {
            self.aliases
                .get(&key)
                .and_then(|full_id| self.models.get(full_id))
        })
    }

    /// Resolve a model reference or role to a concrete model.
    ///
    /// `model_ref` may be:
    /// - empty: use the role default
    /// - `provider_id/model_id`: look up the model directly
    /// - `provider_id/alias`: resolve through an alias
    pub fn resolve(&self, model_ref: &str, role: Option<&str>) -> Option<ResolvedModel> {
        let role = role.unwrap_or("chat");
        let model_ref = model_ref.trim();

        let effective_ref = if model_ref.is_empty() {
            self.role_defaults.get(role)?.as_str()
        } else {
            model_ref
        };

        let (provider_id, model_part) = effective_ref.split_once('/')?;
        let provider_id = Self::normalize_provider_id(provider_id).to_string();
        let provider = self.providers.get(&provider_id)?;

        let key = format!("{}/{}", provider_id, model_part);
        let model_id = self
            .aliases
            .get(&key)
            .map(|s| {
                s.strip_prefix(&format!("{}/", provider_id))
                    .unwrap_or(s)
                    .to_string()
            })
            .unwrap_or_else(|| model_part.to_string());
        let full_key = format!("{}/{}", provider_id, model_id);
        let model = self.models.get(&full_key)?;

        Some(ResolvedModel {
            provider_id: provider.id.clone(),
            model_id,
            api_model_id: model.api_model_id.clone(),
            role: role.to_string(),
        })
    }

    /// Backward-compatible resolution: old callers passed provider id,
    /// requested model, and role separately.
    pub fn resolve_model(
        &self,
        provider_id: &str,
        requested: &str,
        role: Option<&str>,
    ) -> Option<ResolvedModel> {
        let role = role.unwrap_or("chat");
        if requested.trim().is_empty() {
            self.resolve("", Some(role))
        } else {
            self.resolve(&format!("{}/{}", provider_id, requested), Some(role))
        }
    }

    /// Look up a model definition by full id.
    pub fn model_definition(&self, provider_id: &str, model_id: &str) -> Option<&ModelDefinition> {
        let normalized_provider = Self::normalize_provider_id(provider_id);
        self.model(&format!("{}/{}", normalized_provider, model_id))
    }

    /// All providers in insertion order.
    pub fn providers(&self) -> Vec<&ProviderDefinition> {
        let mut v: Vec<&ProviderDefinition> = self.providers.values().collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Override a role default at runtime (e.g. from environment variables).
    /// Silently ignored if `model_ref` is not a known model.
    pub fn set_role_default(&mut self, role: String, model_ref: String) {
        let key = self
            .aliases
            .get(&model_ref)
            .map(|s| s.as_str())
            .unwrap_or(&model_ref);
        if self.models.contains_key(key) || self.resolve(&model_ref, None).is_some() {
            self.role_defaults.insert(role, model_ref);
        }
    }

    /// All models, optionally filtered by provider id.
    pub fn models(&self, provider_id: Option<&str>) -> Vec<&ModelDefinition> {
        let mut v: Vec<&ModelDefinition> = self.models.values().collect();
        if let Some(pid) = provider_id {
            let normalized = Self::normalize_provider_id(pid);
            v.retain(|m| m.provider_id == normalized);
        }
        v.sort_by(|a, b| a.provider_id.cmp(&b.provider_id).then(a.id.cmp(&b.id)));
        v
    }

    /// Models for a provider, sorted by how suitable they are for a role.
    pub fn models_for_role(&self, provider_id: &str, role: &str) -> Vec<&ModelDefinition> {
        let mut models = self.models(Some(provider_id));
        models.sort_by(|a, b| {
            b.role_score(role)
                .cmp(&a.role_score(role))
                .then(a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
        models
    }

    /// True if a model definition satisfies a list of capability requirements.
    pub fn satisfies_requirements(
        &self,
        model: &ModelDefinition,
        requirements: &[String],
    ) -> bool {
        model.satisfies_capabilities(requirements)
    }

    /// Build a concrete provider client from a registry definition.
    pub fn build_provider(
        &self,
        provider_id: &str,
        base_url: &str,
        api_key: &str,
        model: &str,
        timeout_secs: u64,
    ) -> Arc<dyn ModelProvider> {
        let normalized = Self::normalize_provider_id(provider_id);
        match normalized {
            "ollama" => Arc::new(OllamaClient::new(OllamaProviderConfig {
                base_url: base_url.trim_end_matches('/').to_string(),
                default_model: model.to_string(),
                timeout: Duration::from_secs(timeout_secs),
                default_num_ctx: 8192,
                keep_alive: None,
                num_thread: None,
            })),
            "openrouter" => {
                let keys = api_key
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                Arc::new(OpenRouterClient::new(keys))
            }
            _ => Arc::new(CustomOpenAIClient::new(CustomProviderConfig {
                base_url: base_url.trim_end_matches('/').to_string(),
                api_key: api_key.to_string(),
                default_model: model.to_string(),
                provider_label: normalized.to_string(),
                timeout: Duration::from_secs(timeout_secs),
                max_retries: 3,
            })),
        }
    }

    /// Backward-compatible label-to-id helper.
    pub fn provider_id_for_label(&self, label: &str) -> Option<&'static str> {
        let normalized = label.trim().to_lowercase();
        if normalized.starts_with("openrouter") {
            return Some("openrouter");
        }
        if normalized.starts_with("openai") {
            return Some("openai");
        }
        if normalized.starts_with("ollama") {
            return Some("ollama");
        }
        if normalized.starts_with("lm studio") || normalized.starts_with("lmstudio") {
            return Some("lmstudio");
        }
        if normalized.starts_with("custom")
            || normalized.starts_with("together")
            || normalized.starts_with("groq")
        {
            return Some("custom");
        }
        None
    }

    /// Normalize a free-form provider id to the canonical form used by the
    /// registry.
    pub fn normalize_provider_id(id: &str) -> &'static str {
        let normalized = id.trim().to_lowercase();
        match normalized.as_str() {
            "openrouter" | "or" => "openrouter",
            "openai" | "oai" => "openai",
            "ollama" | "local-ollama" => "ollama",
            "lmstudio" | "lm-studio" | "lm_studio" => "lmstudio",
            "custom" | "custom-openai" | "custom-openai-compatible" => "custom",
            "together" | "together-ai" | "together.ai" => "custom",
            "groq" => "custom",
            _ => "custom",
        }
    }
}

const BUILTIN_REGISTRY_YAML: &str = r#"
version: 1
defaults:
  chat: openrouter/openai-gpt-4o-mini
  planning: openrouter/deepseek-deepseek-r1
  coding: openrouter/deepseek-deepseek-coder
  research: openrouter/perplexity-sonar
  utility: openrouter/openai-gpt-4o-mini
  inline_chat: ollama/llama3.1
providers:
  - id: openrouter
    display_name: OpenRouter
    kind: openrouter
    default_base_url: https://openrouter.ai/api/v1
    auth_mode: api_key
    supports_custom_models: true
    supports_reasoning: true
    supports_tools: true
    supports_vision: true
  - id: openai
    display_name: OpenAI
    kind: custom_openai
    default_base_url: https://api.openai.com/v1
    auth_mode: api_key
    supports_reasoning: true
    supports_tools: true
    supports_vision: true
  - id: ollama
    display_name: Ollama
    kind: ollama
    default_base_url: http://localhost:11434/v1
    auth_mode: none
    supports_custom_models: true
  - id: lmstudio
    display_name: LM Studio
    kind: custom_openai
    default_base_url: http://localhost:1234/v1
    auth_mode: none
  - id: custom
    display_name: Custom OpenAI-compatible endpoint
    kind: custom_openai
    default_base_url: ""
    auth_mode: api_key
models:
  - id: openai-gpt-4o-mini
    provider_id: openrouter
    name: GPT-4o Mini
    aliases: [gpt-4o-mini]
    api_model_id: openai/gpt-4o-mini
    tool_calling: true
    vision: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 16384
    cost_per_1k: 0.00015
    cost_tier: cheap
  - id: openai-gpt-4o
    provider_id: openrouter
    name: GPT-4o
    aliases: [gpt-4o]
    api_model_id: openai/gpt-4o
    tool_calling: true
    vision: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 16384
    cost_per_1k: 0.005
    cost_tier: premium
  - id: deepseek-deepseek-coder
    provider_id: openrouter
    name: DeepSeek Coder
    aliases: [deepseek-coder]
    api_model_id: deepseek/deepseek-coder
    tool_calling: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 8192
    cost_per_1k: 0.00014
    cost_tier: cheap
  - id: deepseek-deepseek-r1
    provider_id: openrouter
    name: DeepSeek R1
    aliases: [deepseek-r1]
    api_model_id: deepseek/deepseek-r1
    reasoning: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 8192
    cost_per_1k: 0.002
    cost_tier: standard
  - id: perplexity-sonar
    provider_id: openrouter
    name: Perplexity Sonar
    aliases: [sonar]
    api_model_id: perplexity/sonar
    streaming: true
    max_input_tokens: 127000
    max_output_tokens: 4096
    cost_per_1k: 0.001
    cost_tier: standard
  - id: openai-gpt-4o-mini-openai
    provider_id: openai
    name: GPT-4o Mini
    aliases: [gpt-4o-mini]
    api_model_id: gpt-4o-mini
    tool_calling: true
    vision: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 16384
    cost_per_1k: 0.00015
    cost_tier: cheap
  - id: openai-gpt-4o-openai
    provider_id: openai
    name: GPT-4o
    aliases: [gpt-4o]
    api_model_id: gpt-4o
    tool_calling: true
    vision: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 16384
    cost_per_1k: 0.005
    cost_tier: premium
  - id: llama3.1
    provider_id: ollama
    name: Llama 3.1
    aliases: []
    api_model_id: llama3.1
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 8192
    cost_per_1k: 0.0
    cost_tier: free
  - id: qwen2.5-coder
    provider_id: ollama
    name: Qwen2.5 Coder
    aliases: []
    api_model_id: qwen2.5-coder
    tool_calling: true
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 8192
    cost_per_1k: 0.0
    cost_tier: free
  - id: local-model
    provider_id: lmstudio
    name: Local Model
    aliases: []
    api_model_id: local-model
    streaming: true
    max_input_tokens: 32000
    max_output_tokens: 4096
    cost_per_1k: 0.0
    cost_tier: free
  - id: custom-model
    provider_id: custom
    name: Custom Model
    aliases: []
    api_model_id: gpt-4o-mini
    streaming: true
    max_input_tokens: 128000
    max_output_tokens: 4096
    cost_per_1k: 0.0
    cost_tier: free
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_definition_deserializes() {
        let yaml = r#"
id: openrouter
display_name: OpenRouter
kind: openrouter
default_base_url: https://openrouter.ai/api/v1
auth_mode: api_key
supports_reasoning: true
"#;
        let p: ProviderDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.id, "openrouter");
        assert_eq!(p.display_name, "OpenRouter");
        assert!(p.supports_reasoning);
    }

    #[test]
    fn model_definition_deserializes() {
        let yaml = r#"
id: gpt-4o-mini
provider_id: openrouter
name: GPT-4o Mini
api_model_id: openai/gpt-4o-mini
tool_calling: true
vision: true
streaming: true
max_input_tokens: 128000
max_output_tokens: 16384
cost_per_1k: 0.00015
cost_tier: cheap
"#;
        let m: ModelDefinition = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.id, "gpt-4o-mini");
        assert_eq!(m.api_model_id, "openai/gpt-4o-mini");
        assert_eq!(m.cost_tier, Some(CostTier::Cheap));
    }

    #[test]
    fn builtin_registry_contains_core_providers() {
        let registry = ProviderRegistry::builtin_default();
        assert!(registry.provider("openrouter").is_some());
        assert!(registry.provider("ollama").is_some());
        assert!(registry.provider("custom").is_some());
    }

    #[test]
    fn resolve_model_prefers_role_default_when_available() {
        let registry = ProviderRegistry::builtin_default();
        let model = registry.resolve_model("openrouter", "", Some("chat")).unwrap();
        assert_eq!(model.api_model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn resolve_model_by_full_ref() {
        let registry = ProviderRegistry::builtin_default();
        let model = registry.resolve("openrouter/openai-gpt-4o-mini", None).unwrap();
        assert_eq!(model.provider_id, "openrouter");
        assert_eq!(model.model_id, "openai-gpt-4o-mini");
        assert_eq!(model.api_model_id, "openai/gpt-4o-mini");
        assert_eq!(model.role, "chat");
    }

    #[test]
    fn resolve_role_default() {
        let registry = ProviderRegistry::builtin_default();
        let model = registry.resolve("", Some("coding")).unwrap();
        assert_eq!(model.api_model_id, "deepseek/deepseek-coder");
    }

    #[test]
    fn resolve_alias() {
        let registry = ProviderRegistry::builtin_default();
        let model = registry.resolve("openrouter/gpt-4o-mini", None).unwrap();
        assert_eq!(model.model_id, "openai-gpt-4o-mini");
        assert_eq!(model.api_model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn load_from_yaml_round_trips() {
        let yaml = r#"
version: 1
defaults:
  chat: openrouter/gpt-4o-mini
providers:
  - id: openrouter
    display_name: OpenRouter
    kind: openrouter
    default_base_url: https://openrouter.ai/api/v1
    auth_mode: api_key
models:
  - id: gpt-4o-mini
    provider_id: openrouter
    name: GPT-4o Mini
    api_model_id: openai/gpt-4o-mini
"#;
        let registry = ProviderRegistry::load_from_yaml_str(yaml).unwrap();
        assert!(registry.provider("openrouter").is_some());
        let resolved = registry.resolve("", Some("chat")).unwrap();
        assert_eq!(resolved.api_model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn load_from_yaml_rejects_unknown_role_default() {
        let yaml = r#"
version: 1
defaults:
  chat: openrouter/unknown-model
providers:
  - id: openrouter
    display_name: OpenRouter
    kind: openrouter
    default_base_url: https://openrouter.ai/api/v1
    auth_mode: api_key
models:
  - id: gpt-4o-mini
    provider_id: openrouter
    name: GPT-4o Mini
    api_model_id: openai/gpt-4o-mini
"#;
        let result = ProviderRegistry::load_from_yaml_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_provider_id_maps_aliases() {
        assert_eq!(ProviderRegistry::normalize_provider_id("OR"), "openrouter");
        assert_eq!(ProviderRegistry::normalize_provider_id("oai"), "openai");
        assert_eq!(ProviderRegistry::normalize_provider_id("lm-studio"), "lmstudio");
        assert_eq!(ProviderRegistry::normalize_provider_id("unknown"), "custom");
    }

    #[test]
    fn load_external_config_file() {
        // Walk up from the model crate manifest to find the workspace root.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent().and_then(|p| p.parent());
        let cfg_path = match repo_root {
            Some(root) => root.join("config").join("model_providers.yaml"),
            None => {
                eprintln!("no repo root; skipping");
                return;
            }
        };
        if !cfg_path.exists() {
            eprintln!("{} not present; skipping", cfg_path.display());
            return;
        }

        let registry = ProviderRegistry::load_from_yaml(&cfg_path).unwrap();
        assert!(registry.provider("openrouter").is_some());
        assert!(registry.provider("ollama").is_some());
        assert!(registry.model("openrouter/openai-gpt-4o-mini").is_some());

        let chat = registry.resolve("", Some("chat")).unwrap();
        assert_eq!(chat.api_model_id, "openai/gpt-4o-mini");
    }
}
