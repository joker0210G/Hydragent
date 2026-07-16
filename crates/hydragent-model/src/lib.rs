pub mod openrouter;
pub mod router;
pub mod custom_openai;
pub mod ollama;
pub mod ollama_native;
pub mod model_trait;
pub mod profiles;
pub mod council;
pub mod registry;

pub use model_trait::ModelProvider;
pub use profiles::{CostTier, ModelProfile};
pub use council::{CouncilError, ModelCouncil, RoutingDecision, RoutingPath};
pub use ollama::{OllamaClient, OllamaProviderConfig};
pub use ollama_native::{
    OllamaNativeClient, OllamaNativeConfig, OllamaModelTag, OllamaModelInfo,
    OllamaModelDetails, CachedModelInfo, ModelInfoCache, ParsedModelfile,
    ThinkingMode, OllamaServerStatus,
    check_ollama_server, ensure_ollama_server, discover_ollama_models,
    fetch_model_info, extract_context_length, extract_architecture,
    extract_parameter_count, parse_modelfile, build_model_definition_from_tag,
    sync_discovered_models_to_yaml, model_supports_thinking, model_supports_tools,
    model_supports_vision, resolve_thinking_param, build_ollama_tool,
    build_structured_format, format_parameter_size, estimate_context_window,
};
pub use registry::{
    AuthMode, ModelDefinition, ModelRegistry, ProviderDefinition, ProviderKind, ProviderRegistry,
    RegistryError, ResolvedModel,
};

