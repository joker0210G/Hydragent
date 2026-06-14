//! Phase 7 / Track 7.1 - LLM-callable skill tools.
//!
//! **Status: skeleton**. The tools are surfaced to the LLM through
//! the tool registry, e.g.:
//!
//! * `skill_list`   - return all `Active` skills
//! * `skill_search` - FTS5 search across the library
//! * `skill_invoke` - render + execute a skill
//! * `skill_create` - admin-only; insert a `Candidate` skill
//!
//! Implementations land in Week 28 alongside the executor.
