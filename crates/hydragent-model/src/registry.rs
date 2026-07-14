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
    #[serde(default)]
    pub models: Vec<ModelDefinition>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// A declarative model definition under a provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDefinition {
    pub id: String,
    #[serde(default)]
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
    #[serde(default)]
    pub url: Option<String>,
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

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum RegistryInput {
    Map {
        #[serde(default)]
        version: Option<u32>,
        #[serde(default)]
        defaults: HashMap<String, String>,
        #[serde(default)]
        providers: Vec<RawProviderInput>,
        #[serde(default)]
        models: Vec<RawModelInput>,
        #[serde(default)]
        routing_profiles: Vec<crate::profiles::ModelProfile>,
    },
    Seq(Vec<RawProviderInput>),
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct RawProviderInput {
    id: Option<String>,
    display_name: Option<String>,
    kind: Option<ProviderKind>,
    default_base_url: Option<String>,
    auth_mode: Option<AuthMode>,
    #[serde(default)]
    supports_custom_models: Option<bool>,
    #[serde(default)]
    supports_reasoning: Option<bool>,
    #[serde(default)]
    supports_tools: Option<bool>,
    #[serde(default)]
    supports_vision: Option<bool>,
    #[serde(default)]
    default_headers: Option<HashMap<String, String>>,
    timeout_secs: Option<u64>,
    max_retries: Option<u32>,
    #[serde(default)]
    default_params: Option<HashMap<String, serde_yaml::Value>>,

    name: Option<String>,
    vendor: Option<String>,
    #[serde(alias = "url", alias = "baseUrl")]
    url: Option<String>,
    #[serde(alias = "apiKey", alias = "api_key")]
    api_key: Option<String>,
    #[serde(alias = "apiType")]
    api_type: Option<String>,
    #[serde(default)]
    models: Option<Vec<RawModelInput>>,
    #[serde(default)]
    settings: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Deserialize, Clone)]
struct RawModelInput {
    id: String,
    provider_id: Option<String>,
    name: Option<String>,
    #[serde(default)]
    aliases: Option<Vec<String>>,
    api_model_id: Option<String>,
    #[serde(default)]
    tool_calling: Option<bool>,
    #[serde(default)]
    vision: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    streaming: Option<bool>,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    #[serde(default)]
    request_headers: Option<HashMap<String, String>>,
    #[serde(default)]
    default_params: Option<HashMap<String, serde_yaml::Value>>,
    cost_per_1k: Option<f64>,
    cost_tier: Option<crate::profiles::CostTier>,

    url: Option<String>,
    #[serde(alias = "toolCalling")]
    tool_calling_new: Option<bool>,
    #[serde(alias = "maxInputTokens")]
    max_input_tokens_new: Option<u32>,
    #[serde(alias = "maxOutputTokens")]
    max_output_tokens_new: Option<u32>,
    #[serde(alias = "requestHeaders")]
    request_headers_new: Option<HashMap<String, String>>,
}

/// A registry of providers and models.
///
/// Providers and models are keyed by their declared ids. Models are also
/// addressable by aliases and by role defaults.
///
/// The registry may also carry `routing_profiles` — the smart task-routing
/// layer (formerly known as the Model Council) that picks the best model
/// per task type (code, research, planning …).
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderDefinition>,
    models: HashMap<String, ModelDefinition>,
    aliases: HashMap<String, String>,
    role_defaults: HashMap<String, String>,
    routing_profiles: Vec<crate::profiles::ModelProfile>,
}

impl ProviderRegistry {
    /// Load a registry from a YAML file.
    pub fn load_from_yaml<P: AsRef<Path>>(path: P) -> Result<Self, RegistryError> {
        let text = std::fs::read_to_string(path.as_ref())?;
        Self::load_from_yaml_str(&text)
    }

    /// Load a registry from a YAML string.
    pub fn load_from_yaml_str(text: &str) -> Result<Self, RegistryError> {
        let parsed: RegistryInput = serde_yaml::from_str(text)?;

        let mut providers = HashMap::new();
        let mut models = HashMap::new();
        let mut aliases = HashMap::new();

        let (defaults, raw_providers, raw_models, routing_profiles, is_seq) = match parsed {
            RegistryInput::Map { version: _, defaults, providers, models, routing_profiles } => {
                (defaults, providers, models, routing_profiles, false)
            }
            RegistryInput::Seq(providers) => {
                let builtin = Self::builtin_default();
                (builtin.role_defaults, providers, vec![], builtin.routing_profiles, true)
            }
        };

        let slugify = |s: &str| -> String {
            s.to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-")
        };

        let normalize_vendor = |vendor: &str| -> ProviderKind {
            match vendor.to_lowercase().as_str() {
                "openrouter" => ProviderKind::OpenRouter,
                "ollama" => ProviderKind::Ollama,
                "customendpoint" | "custom_openai" | "openai" | "copilot" | "custom" => ProviderKind::CustomOpenAi,
                _ => ProviderKind::CustomOpenAi,
            }
        };

        let default_base_url_for_kind = |kind: &ProviderKind| -> &'static str {
            match kind {
                ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
                ProviderKind::Ollama => "http://localhost:11434/v1",
                ProviderKind::CustomOpenAi => "",
                ProviderKind::CustomEndpoint => "",
            }
        };

        // Convert RawProviderInput to ProviderDefinition
        for raw_p in raw_providers {
            let id = raw_p.id.clone()
                .or_else(|| raw_p.name.as_ref().map(|n| slugify(n)))
                .unwrap_or_else(|| "default".to_string());

            let display_name = raw_p.display_name.clone()
                .or_else(|| raw_p.name.clone())
                .unwrap_or_else(|| id.clone());

            let kind = raw_p.kind.clone()
                .or_else(|| raw_p.vendor.as_ref().map(|v| normalize_vendor(v)))
                .unwrap_or(ProviderKind::CustomOpenAi);

            let default_base_url = raw_p.default_base_url.clone()
                .or_else(|| raw_p.url.clone())
                .unwrap_or_else(|| default_base_url_for_kind(&kind).to_string());

            let auth_mode = raw_p.auth_mode.clone()
                .unwrap_or_else(|| {
                    if raw_p.api_key.is_some() {
                        AuthMode::ApiKey
                    } else {
                        match kind {
                            ProviderKind::Ollama => AuthMode::None,
                            _ => AuthMode::ApiKey,
                        }
                    }
                });

            let supports_custom_models = raw_p.supports_custom_models.unwrap_or(true);
            let supports_reasoning = raw_p.supports_reasoning.unwrap_or(true);
            let supports_tools = raw_p.supports_tools.unwrap_or(true);
            let supports_vision = raw_p.supports_vision.unwrap_or(true);
            let default_headers = raw_p.default_headers.clone().unwrap_or_default();
            let timeout_secs = raw_p.timeout_secs.unwrap_or(180);
            let max_retries = raw_p.max_retries.unwrap_or(3);
            let default_params = raw_p.default_params.clone().unwrap_or_default();

            // Load models: if none specified, inherit builtin models for standard provider kinds!
            let mut p_models = Vec::new();
            if let Some(raw_models) = raw_p.models {
                for rm in raw_models {
                    let m_id = rm.id.clone();
                    let m_name = rm.name.clone().unwrap_or_else(|| m_id.clone());
                    let api_model_id = rm.api_model_id.clone().unwrap_or_else(|| m_id.clone());
                    let tool_calling = rm.tool_calling_new.or(rm.tool_calling).unwrap_or(false);
                    let vision = rm.vision.unwrap_or(false);
                    let reasoning = rm.reasoning.unwrap_or(false);
                    let streaming = rm.streaming.unwrap_or(true);
                    let max_input_tokens = rm.max_input_tokens_new.or(rm.max_input_tokens);
                    let max_output_tokens = rm.max_output_tokens_new.or(rm.max_output_tokens);
                    let request_headers = rm.request_headers_new.or(rm.request_headers).unwrap_or_default();
                    let default_params = rm.default_params.clone().unwrap_or_default();
                    let cost_per_1k = rm.cost_per_1k;
                    let cost_tier = rm.cost_tier;
                    let aliases_list = rm.aliases.clone().unwrap_or_default();
                    let url = rm.url.clone();

                    let m_def = ModelDefinition {
                        id: m_id,
                        provider_id: id.clone(),
                        name: m_name,
                        aliases: aliases_list,
                        api_model_id,
                        tool_calling,
                        vision,
                        reasoning,
                        streaming,
                        max_input_tokens,
                        max_output_tokens,
                        request_headers,
                        default_params,
                        cost_per_1k,
                        cost_tier,
                        url,
                    };
                    p_models.push(m_def);
                }
            } else {
                // Models list is omitted. Check if this is a standard vendor and inherit its builtin models!
                let builtin = Self::builtin_default();
                let builtin_provider_id = match kind {
                    ProviderKind::OpenRouter => "openrouter",
                    ProviderKind::Ollama => "ollama",
                    ProviderKind::CustomOpenAi if id == "openai" => "openai",
                    ProviderKind::CustomOpenAi if id == "lmstudio" => "lmstudio",
                    _ => "",
                };
                if !builtin_provider_id.is_empty() {
                    for bm in builtin.models(Some(builtin_provider_id)) {
                        let mut m = bm.clone();
                        m.provider_id = id.clone();
                        p_models.push(m);
                    }
                }
            }

            for mut m in p_models.clone() {
                m.provider_id = id.clone();
                let full_id = format!("{}/{}", m.provider_id, m.id);
                models.insert(full_id.clone(), m.clone());
                aliases.insert(format!("{}/{}", m.provider_id, m.id), full_id.clone());
                for alias in &m.aliases {
                    aliases.insert(format!("{}/{}", m.provider_id, alias), full_id.clone());
                }
            }

            let p_def = ProviderDefinition {
                id: id.clone(),
                display_name,
                kind,
                default_base_url,
                auth_mode,
                supports_custom_models,
                supports_reasoning,
                supports_tools,
                supports_vision,
                default_headers,
                timeout_secs,
                max_retries,
                default_params,
                models: p_models,
                api_key: raw_p.api_key.clone(),
            };

            providers.insert(id, p_def);
        }

        // Process any root-level flat models (old format compatibility)
        for rm in raw_models {
            let m_id = rm.id.clone();
            let p_id = rm.provider_id.clone().unwrap_or_else(|| "default".to_string());
            if !providers.contains_key(&p_id) {
                return Err(RegistryError::UnknownProvider(p_id));
            }
            let m_name = rm.name.clone().unwrap_or_else(|| m_id.clone());
            let api_model_id = rm.api_model_id.clone().unwrap_or_else(|| m_id.clone());
            let tool_calling = rm.tool_calling_new.or(rm.tool_calling).unwrap_or(false);
            let vision = rm.vision.unwrap_or(false);
            let reasoning = rm.reasoning.unwrap_or(false);
            let streaming = rm.streaming.unwrap_or(true);
            let max_input_tokens = rm.max_input_tokens_new.or(rm.max_input_tokens);
            let max_output_tokens = rm.max_output_tokens_new.or(rm.max_output_tokens);
            let request_headers = rm.request_headers_new.or(rm.request_headers).unwrap_or_default();
            let default_params = rm.default_params.clone().unwrap_or_default();
            let cost_per_1k = rm.cost_per_1k;
            let cost_tier = rm.cost_tier;
            let aliases_list = rm.aliases.clone().unwrap_or_default();
            let url = rm.url.clone();

            let m_def = ModelDefinition {
                id: m_id,
                provider_id: p_id.clone(),
                name: m_name,
                aliases: aliases_list,
                api_model_id,
                tool_calling,
                vision,
                reasoning,
                streaming,
                max_input_tokens,
                max_output_tokens,
                request_headers,
                default_params,
                cost_per_1k,
                cost_tier,
                url,
            };

            let full_id = format!("{}/{}", m_def.provider_id, m_def.id);
            models.insert(full_id.clone(), m_def.clone());
            aliases.insert(format!("{}/{}", m_def.provider_id, m_def.id), full_id.clone());
            for alias in &m_def.aliases {
                aliases.insert(format!("{}/{}", m_def.provider_id, alias), full_id.clone());
            }
        }

        // Validate role defaults point to known models.
        // For Seq-based configs (simple format), silently drop inherited defaults
        // that don't resolve — the user didn't define every provider, and that's fine.
        let mut valid_defaults = HashMap::new();
        for (role, model_ref) in defaults {
            if !model_ref.contains('/') {
                if !is_seq {
                    tracing::warn!(
                        "Invalid model reference '{}' specified as default for role '{}' (expected provider/model format).",
                        model_ref, role
                    );
                }
                continue;
            }
            let key = aliases.get(&model_ref).map(|s| s.as_str()).unwrap_or(model_ref.as_str());
            
            let mut is_valid = false;
            if models.contains_key(key) {
                is_valid = true;
            } else if let Some((prov_id, _)) = model_ref.split_once('/') {
                if let Some(prov) = providers.get(prov_id) {
                    if prov.supports_custom_models {
                        is_valid = true;
                    }
                }
            }

            if is_valid {
                valid_defaults.insert(role, model_ref);
            } else if !is_seq {
                tracing::warn!(
                    "Unknown model '{}' specified as default for role '{}'. Attempting to use it anyway.",
                    model_ref, role
                );
                valid_defaults.insert(role, model_ref);
            }
        }

        Ok(Self {
            providers,
            models,
            aliases,
            role_defaults: valid_defaults,
            routing_profiles,
        })
    }

    /// Built-in registry shipped with the binary, used when no external file
    /// is provided.
    pub fn builtin_default() -> Self {
        Self::load_from_yaml_str(BUILTIN_REGISTRY_YAML)
            .expect("builtin registry is valid YAML")
    }

    /// Look up a provider by id.
    /// First tries the id exactly as-is (supports user-defined slugified names like "zenmux"),
    /// then falls back to the normalized alias (supports standard aliases like "OR" → "openrouter").
    pub fn provider(&self, id: &str) -> Option<&ProviderDefinition> {
        if let Some(p) = self.providers.get(id) {
            return Some(p);
        }
        let normalized = Self::normalize_provider_id(id);
        self.providers.get(normalized)
    }

    /// Look up a model by its full id `provider_id/model_id`.
    /// First tries the id exactly, then falls back to normalised provider alias.
    pub fn model(&self, id: &str) -> Option<&ModelDefinition> {
        let (provider_id, model_id) = id.split_once('/')?;
        // Try raw provider id first (user-defined names).
        let raw_key = format!("{}/{}", provider_id, model_id);
        if let Some(m) = self.models.get(&raw_key).or_else(|| {
            self.aliases.get(&raw_key).and_then(|fid| self.models.get(fid))
        }) {
            return Some(m);
        }
        // Fallback: try normalized provider alias.
        let normalized_provider = Self::normalize_provider_id(provider_id);
        let norm_key = format!("{}/{}", normalized_provider, model_id);
        self.models.get(&norm_key).or_else(|| {
            self.aliases
                .get(&norm_key)
                .and_then(|full_id| self.models.get(full_id))
        })
    }

    /// Resolve a model reference or role to a concrete model.
    ///
    /// `model_ref` may be:
    /// - empty: use the role default
    /// - `provider_id/model_id`: look up the model directly
    /// - `provider_id/alias`: resolve through an alias
    pub fn role_default(&self, role: &str) -> Option<&str> {
        self.role_defaults.get(role).map(|s| s.as_str())
    }

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

    /// Returns the smart task-routing profiles embedded in this registry
    /// (the "council" layer). Empty when the registry file has no
    /// `routing_profiles:` section.
    pub fn routing_profiles(&self) -> &[crate::profiles::ModelProfile] {
        &self.routing_profiles
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
            "ollama" => {
                let mut cfg = OllamaProviderConfig::from_env();
                if !base_url.is_empty() {
                    cfg.base_url = base_url.trim_end_matches('/').to_string();
                }
                if !model.is_empty() {
                    cfg.default_model = model.to_string();
                }
                if timeout_secs > 0 {
                    cfg.timeout = Duration::from_secs(timeout_secs);
                }
                Arc::new(OllamaClient::new(cfg))
            }
            "openrouter" => {
                let keys = api_key
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                Arc::new(OpenRouterClient::new(keys))
            }
            _ => {
                let mut model_urls = HashMap::new();
                for m in self.models(Some(provider_id)) {
                    if let Some(ref custom_url) = m.url {
                        model_urls.insert(m.api_model_id.clone(), custom_url.clone());
                    }
                }
                Arc::new(CustomOpenAIClient::new(CustomProviderConfig {
                    base_url: base_url.trim_end_matches('/').to_string(),
                    api_key: api_key.to_string(),
                    default_model: model.to_string(),
                    provider_label: normalized.to_string(),
                    timeout: Duration::from_secs(timeout_secs),
                    max_retries: 3,
                    model_urls,
                }))
            }
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

pub const BUILTIN_REGISTRY_YAML: &str = r#"
version: 1
defaults:
  chat: openrouter/gpt-4o-mini
  planning: openrouter/deepseek-r1
  coding: openrouter/deepseek-coder
  research: openrouter/sonar
  utility: openrouter/gpt-4o-mini
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
    models:
      - id: gpt-4o-mini
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
      - id: gpt-4o
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
      - id: deepseek-coder
        name: DeepSeek Coder
        aliases: [deepseek-coder]
        api_model_id: deepseek/deepseek-coder
        tool_calling: true
        streaming: true
        max_input_tokens: 128000
        max_output_tokens: 8192
        cost_per_1k: 0.00014
        cost_tier: cheap
      - id: deepseek-r1
        name: DeepSeek R1
        aliases: [deepseek-r1]
        api_model_id: deepseek/deepseek-r1
        reasoning: true
        streaming: true
        max_input_tokens: 128000
        max_output_tokens: 8192
        cost_per_1k: 0.002
        cost_tier: standard
      - id: sonar
        name: Perplexity Sonar
        aliases: [sonar]
        api_model_id: perplexity/sonar
        streaming: true
        max_input_tokens: 127000
        max_output_tokens: 4096
        cost_per_1k: 0.001
        cost_tier: standard
  - id: openai
    display_name: OpenAI
    kind: custom_openai
    default_base_url: https://api.openai.com/v1
    auth_mode: api_key
    supports_reasoning: true
    supports_tools: true
    supports_vision: true
    models:
      - id: gpt-4o-mini
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
      - id: gpt-4o
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
  - id: ollama
    display_name: Ollama
    kind: ollama
    default_base_url: http://localhost:11434/v1
    auth_mode: none
    supports_custom_models: true
    models:
      - id: llama3.1
        name: Llama 3.1
        aliases: []
        api_model_id: llama3.1
        streaming: true
        max_input_tokens: 128000
        max_output_tokens: 8192
        cost_per_1k: 0.0
        cost_tier: free
      - id: qwen2.5-coder
        name: Qwen2.5 Coder
        aliases: []
        api_model_id: qwen2.5-coder
        tool_calling: true
        streaming: true
        max_input_tokens: 128000
        max_output_tokens: 8192
        cost_per_1k: 0.0
        cost_tier: free
  - id: lmstudio
    display_name: LM Studio
    kind: custom_openai
    default_base_url: http://localhost:1234/v1
    auth_mode: none
    models:
      - id: local-model
        name: Local Model
        aliases: []
        api_model_id: local-model
        streaming: true
        max_input_tokens: 32000
        max_output_tokens: 4096
        cost_per_1k: 0.0
        cost_tier: free
  - id: custom
    display_name: Custom OpenAI-compatible endpoint
    kind: custom_openai
    default_base_url: ""
    auth_mode: api_key
    models:
      - id: custom-model
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
        let model = registry.resolve("openrouter/gpt-4o-mini", None).unwrap();
        assert_eq!(model.provider_id, "openrouter");
        assert_eq!(model.model_id, "gpt-4o-mini");
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
        assert_eq!(model.model_id, "gpt-4o-mini");
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
        name: GPT-4o Mini
        api_model_id: openai/gpt-4o-mini
"#;
        let registry = ProviderRegistry::load_from_yaml_str(yaml).unwrap();
        assert!(registry.provider("openrouter").is_some());
        let resolved = registry.resolve("", Some("chat")).unwrap();
        assert_eq!(resolved.api_model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn load_from_yaml_allows_unknown_role_default_with_warning() {
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
        name: GPT-4o Mini
        api_model_id: openai/gpt-4o-mini
"#;
        let result = ProviderRegistry::load_from_yaml_str(yaml);
        assert!(result.is_ok());
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
        assert!(registry.model("openrouter/gpt-4o-mini").is_some());

        let chat = registry.resolve("", Some("chat")).unwrap();
        assert_eq!(chat.api_model_id, "openai/gpt-4o-mini");
    }

    #[test]
    fn test_vscode_camelcase_and_sequence_loading() {
        let yaml = r#"
- name: zenmux
  vendor: customendpoint
  apiKey: "secret-key-123"
  models:
    - id: moonshotai/kimi-k2.7-code-free
      name: kimi-k2.7-code
      url: https://zenmux.ai/api/v1
      toolCalling: true
      vision: true
      maxInputTokens: 128000
      maxOutputTokens: 16000
- name: Ollama
  vendor: ollama
  url: http://localhost:11434
"#;
        let registry = ProviderRegistry::load_from_yaml_str(yaml).unwrap();
        
        // check zenmux provider
        let p_zen = registry.provider("zenmux").unwrap();
        assert_eq!(p_zen.display_name, "zenmux");
        assert_eq!(p_zen.kind, ProviderKind::CustomOpenAi);
        assert_eq!(p_zen.api_key.as_deref(), Some("secret-key-123"));
        
        // check custom model URL override
        let m_kimi = registry.model("zenmux/moonshotai/kimi-k2.7-code-free").unwrap();
        assert_eq!(m_kimi.name, "kimi-k2.7-code");
        assert_eq!(m_kimi.url.as_deref(), Some("https://zenmux.ai/api/v1"));
        assert!(m_kimi.tool_calling);
        assert!(m_kimi.vision);
        assert_eq!(m_kimi.max_input_tokens, Some(128000));
        
        // check Ollama provider & auto-inherited models!
        let p_oll = registry.provider("ollama").unwrap();
        assert_eq!(p_oll.display_name, "Ollama");
        assert_eq!(p_oll.default_base_url, "http://localhost:11434");
        
        // Ollama models should have been auto-populated from builtin default!
        let m_llama = registry.model("ollama/llama3.1").unwrap();
        assert_eq!(m_llama.provider_id, "ollama");
        assert_eq!(m_llama.api_model_id, "llama3.1");
    }

    #[test]
    fn test_lookup_raw_and_normalized() {
        let yaml = r#"
- name: zenmux
  vendor: customendpoint
  models:
    - id: moonshotai/kimi-k2.7-code-free
      aliases:
        - kimi-k2.7
- name: OpenRouter
  vendor: openrouter
"#;
        let registry = ProviderRegistry::load_from_yaml_str(yaml).unwrap();
        
        // 1. Try exact lookup of user-defined slugified provider name
        assert!(registry.provider("zenmux").is_some());
        
        // 2. Try exact lookup of model
        assert!(registry.model("zenmux/moonshotai/kimi-k2.7-code-free").is_some());
        
        // 3. Try lookup using alias
        assert!(registry.model("zenmux/kimi-k2.7").is_some());
        
        // 4. Try normalized alias lookup (e.g. OR -> openrouter)
        assert!(registry.provider("OR").is_some());
        assert_eq!(registry.provider("OR").unwrap().id, "openrouter");
    }
}
