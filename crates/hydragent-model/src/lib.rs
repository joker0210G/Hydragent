pub mod openrouter;
pub mod router;
pub mod custom_openai;
pub mod model_trait;
pub mod profiles;
pub mod council;

pub use model_trait::ModelProvider;
pub use profiles::{CostTier, ModelProfile};
pub use council::{CouncilError, ModelCouncil, RoutingDecision, RoutingPath};
