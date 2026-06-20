// crates/hydragent-tools/src/lib.rs
pub mod tool_trait;
pub mod registry;
pub mod web_search;
pub mod file_read;
pub mod echo;
pub mod memory_store;
pub mod memory_search;
pub mod memory_forget;
pub mod standing_orders;
pub mod user_profile;
pub mod send_message;
pub mod schedule_task;
pub mod rss_subscribe;
pub mod phase6;
pub mod agent_reach;
pub mod url_fetch;

// ── Phase 7 / Track 7.1 — Skill library tools ───────────────────────
//
// These expose the persistent `SkillLibrary` (hydragent_skills) to the
// chat LLM so it can discover, search, and render skills via the tool
// registry. Each tool opens its own `SkillLibrary` handle per call,
// mirroring the AuditQueryTool pattern.
pub mod skill_list;
pub mod skill_search;
pub mod skill_run;
