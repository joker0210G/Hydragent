pub mod session_store;
pub mod models;
pub mod semantic_store;
pub mod vector_index;
pub mod retrieval;
pub mod context_injector;
/// The Library — design spec §1 + §2.
///
/// Typed knowledge graph (Page / Book / Shelf) with tag-based
/// Louvain-style clustering and cost-tracking statistics. See
/// [`library::Library`] for the entry point.
pub mod library;
/// The Librarian — design spec §2.
///
/// Orchestrator that runs the 25% LLM / 75% Graphify ingestion
/// loop and tracks the cost split between the two.
pub mod librarian;

pub use session_store::SessionStore;
pub use models::{SemanticMemory, MemoryConsolidationJob};
pub use vector_index::VectorStore;
pub use retrieval::hybrid_search;
pub use context_injector::build_system_prompt_with_memory;
pub use library::{Library, LibraryStats, NodeKind, EdgeRelation, GraphNode, ExpandHit};
pub use librarian::{Librarian, LibrarianStats, IngestionResult};
