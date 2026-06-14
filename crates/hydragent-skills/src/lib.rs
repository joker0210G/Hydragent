//! # hydragent-skills
//!
//! Phase 7 / Track 7.1+ - Self-improving skill engine and curator.
//!
//! This crate adds three things on top of the existing tool + model plumbing:
//!
//! * [`Skill`] - a re-usable prompt + param schema + tool allowlist. The
//!   central artifact of Phase 7. One row in the `skill_library` table.
//! * [`SkillLibrary`] - the persistent SQLite-backed library of skills,
//!   with FTS5 full-text search, tag-based retrieval, and a 7-day
//!   [`Curator`](crate::curator::SevenDayCurator) for tier promotion and
//!   archival.
//! * [`SkillExtractor`] (Hermes induction) - looks at recent successful
//!   trajectories and proposes new candidate skills. The dream job.
//! * [`SkillExecutor`] - actually invokes a skill: fills the template
//!   with the supplied params, enforces the tool allowlist, records the
//!   execution, and returns the rendered prompt + tool calls.
//! * [`SkillComposer`] - "skill of skills": decomposes a complex task
//!   into an ordered set of skill invocations.
//!
//! The crate is intentionally **stateless and I/O-pure**: everything is
//! persisted in SQLite and reproduced deterministically from inputs.
//! That makes it cheap to re-build from scratch on a new host and easy
//! to test in isolation.

/// The core skill type re-exports (also defined in `hydragent-types`).
pub use hydragent_types::{Skill, SkillExecutionRecord, SkillParam, SkillTier, SkillVersion};

/// Library-side skill helpers: YAML shape, template renderer, builtin
/// skill manifest. The canonical [`Skill`] type lives in
/// `hydragent-types` for cross-crate consistency.
pub mod skill;

/// Persistent SQLite-backed library of skills with FTS5 search.
pub mod library;
/// Hermes skill induction from successful trajectories.
pub mod extractor;
/// Skill invocation: render template, enforce tool allowlist, record telemetry.
pub mod executor;
/// 7-day curator: promote `Candidate -> Active`, archive stale skills.
pub mod curator;
/// Multi-skill composition: chain skills into DAGs.
pub mod composer;
/// Tag/embedding similarity search helpers.
pub mod similarity;
/// LLM-callable skill tools (list / invoke / search).
pub mod tools;

pub use library::SkillLibrary;
pub use extractor::SkillExtractor;
pub use executor::{SkillExecutor, SkillRenderResult};
pub use curator::SevenDayCurator;
pub use composer::SkillComposer;
