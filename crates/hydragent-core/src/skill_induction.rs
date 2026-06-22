// crates/hydragent-core/src/skill_induction.rs
//
// Phase 7 / Week 27 / Day 6 - Dreaming -> SkillExtractor integration.
//
// At the end of a successful dream cycle (one or more user/assistant
// turns have been consolidated), we re-collect the same turns into a
// [`hydragent_skills::extractor::Trajectory`] and run the skill
// extractor over it. If a candidate is produced and isn't a
// near-duplicate of an existing skill, we insert it at the
// `Candidate` tier so the 7-day curator can later promote / archive
// it.
//
// ## LLM-based extraction
//
// When a [`ModelRouter`] is available (via [`ModelRouterLlmClient`]),
// we first attempt [`SkillExtractor::propose_with_llm`] which uses the
// LLM to propose a refined skill candidate. If that returns `None`
// (quality gate failed or JSON parse error), we fall back to the
// deterministic [`SkillExtractor::extract`].

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use hydragent_model::openrouter::ChatMessage;
use hydragent_model::router::ModelRouter;
use hydragent_skills::extractor::{LlmClient, SkillExtractor, Trajectory, TrajectoryTurn};
use hydragent_skills::library::SkillLibrary;
use hydragent_types::Skill;
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Adapter that makes a [`ModelRouter`] implement the [`LlmClient`]
/// trait used by [`SkillExtractor::propose_with_llm`].
struct ModelRouterLlmClient(Arc<ModelRouter>);

#[async_trait]
impl LlmClient for ModelRouterLlmClient {
    async fn generate(&self, prompt: &str) -> anyhow::Result<String> {
        let (tx, _rx) = mpsc::channel::<String>(100);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: prompt.to_string(),
        }];
        let (content, _) = self.0.chat_stream(messages, tx, None).await?;
        Ok(content)
    }
}

/// Result of one dream-cycle -> skill induction pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct InductionStats {
    /// Trajectories we actually looked at (== successful pages with
    /// at least 2 turns and 1 placeholder match).
    pub trajectories_seen: usize,
    /// A non-duplicate candidate was inserted into the library.
    pub skills_inserted: usize,
    /// The extractor produced a candidate but it was a near-duplicate
    /// of an existing skill (jaccard above threshold).
    pub duplicates_skipped: usize,
    /// Trajectories that didn't pass the extractor's gating (too few
    /// turns, no placeholders, etc.) — perfectly normal, not an
    /// error.
    pub rejected: usize,
    /// Soft errors (library I/O, JSON parse, etc.) — counted but
    /// never propagated, so one bad trajectory doesn't sink the
    /// whole dream cycle.
    pub errors: usize,
}

/// Run one round of skill induction against the given page's recent
/// messages. We re-fetch the same `LIMIT BATCH_SIZE` rows the dream
/// worker just consolidated, build a trajectory, and try to extract
/// a skill. The library path defaults to
/// `./data/skill_library.sqlite` — created if missing.
pub async fn induce_skill_from_page(
    pool: &SqlitePool,
    page_id: &str,
    library_path: &Path,
) -> Result<InductionStats> {
    let mut stats = InductionStats::default();

    // Open (or create) the skill library. We use the file-backed DB
    // so the library persists across restarts; the in-memory mode
    // would forget every candidate the moment the worker exits.
    let library = match SkillLibrary::open(library_path).await {
        Ok(l) => l,
        Err(e) => return Err(anyhow::anyhow!(
            "opening skill library at {}: {}",
            library_path.display(),
            e
        )),
    };

    let turns = fetch_trajectory_turns(pool, page_id).await?;
    if turns.len() < 2 {
        debug!(page_id, "skill induction: trajectory too short, skipping");
        return Ok(stats);
    }
    stats.trajectories_seen += 1;

    let trajectory = Trajectory {
        session_id: Uuid::new_v4().to_string(),
        turns,
        tools_used: Vec::new(),
    };

    let extractor = SkillExtractor::default();
    let candidate = match extractor.extract(&trajectory) {
        Ok(Some(c)) => c,
        Ok(None) => {
            stats.rejected += 1;
            return Ok(stats);
        }
        Err(e) => {
            warn!(page_id, error = %e, "skill induction: extractor failed");
            stats.errors += 1;
            return Ok(stats);
        }
    };

    // Dedup against the existing library. We use a fairly aggressive
    // threshold (0.6) so we don't flood the library with paraphrases
    // of the same skill.
    let existing: Vec<Skill> = library
        .list_skills(hydragent_skills::library::SkillFilter::default())
        .await
        .unwrap_or_default();
    if extractor.is_duplicate(&candidate.skill, &existing, 0.6) {
        debug!(
            page_id,
            skill = %candidate.skill.name,
            "skill induction: candidate is a near-duplicate, skipping"
        );
        stats.duplicates_skipped += 1;
        return Ok(stats);
    }

    match library.insert_skill(&candidate.skill).await {
        Ok(_) => {
            info!(
                page_id,
                skill = %candidate.skill.name,
                confidence = candidate.confidence,
                "skill induction: stored new Candidate skill"
            );
            stats.skills_inserted += 1;
        }
        Err(e) => {
            warn!(page_id, error = %e, "skill induction: insert_skill failed");
            stats.errors += 1;
        }
    }
    Ok(stats)
}

/// Spawn-safe wrapper: open the library once and reuse it for many
/// pages. The library is cheap to clone (it's a `SqlitePool` /
/// `Arc`-shaped internally) so we can hand the same handle to every
/// fan-out task the dream worker creates.
///
/// If a [`ModelRouter`] is supplied (via
/// [`induce_skill_from_page_with_router`]), this function first
/// attempts LLM-based skill proposal via
/// [`SkillExtractor::propose_with_llm`], falling back to the
/// deterministic [`SkillExtractor::extract`] if that returns `None`.
pub async fn induce_skill_from_page_with_library(
    library: Arc<SkillLibrary>,
    pool: &SqlitePool,
    page_id: &str,
) -> InductionStats {
    induce_skill_from_page_with_library_and_router(library, pool, page_id, None).await
}

/// As [`induce_skill_from_page_with_library`] but accepts a
/// [`ModelRouter`] to enable LLM-based skill extraction.
pub async fn induce_skill_from_page_with_router(
    library: Arc<SkillLibrary>,
    pool: &SqlitePool,
    page_id: &str,
    router: Arc<ModelRouter>,
) -> InductionStats {
    induce_skill_from_page_with_library_and_router(library, pool, page_id, Some(router)).await
}

async fn induce_skill_from_page_with_library_and_router(
    library: Arc<SkillLibrary>,
    pool: &SqlitePool,
    page_id: &str,
    router: Option<Arc<ModelRouter>>,
) -> InductionStats {
    let mut stats = InductionStats::default();

    let turns = match fetch_trajectory_turns(pool, page_id).await {
        Ok(t) => t,
        Err(e) => {
            warn!(page_id, error = %e, "skill induction: fetch turns failed");
            stats.errors += 1;
            return stats;
        }
    };
    if turns.len() < 2 {
        return stats;
    }
    stats.trajectories_seen += 1;

    let trajectory = Trajectory {
        session_id: Uuid::new_v4().to_string(),
        turns,
        tools_used: Vec::new(),
    };

    let extractor = SkillExtractor::default();

    // Try LLM-based extraction first if a router is available
    let candidate = if let Some(ref r) = router {
        match extractor.propose_with_llm(&ModelRouterLlmClient(r.clone()), &trajectory).await {
            Ok(Some(c)) => {
                debug!(page_id, "skill induction: LLM proposed a candidate");
                c
            }
            Ok(None) => {
                debug!(page_id, "skill induction: LLM returned None, falling back to deterministic");
                match extractor.extract(&trajectory) {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        stats.rejected += 1;
                        return stats;
                    }
                    Err(e) => {
                        warn!(page_id, error = %e, "skill induction: deterministic extractor failed");
                        stats.errors += 1;
                        return stats;
                    }
                }
            }
            Err(e) => {
                warn!(page_id, error = %e, "skill induction: LLM call failed, falling back");
                match extractor.extract(&trajectory) {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        stats.rejected += 1;
                        return stats;
                    }
                    Err(e) => {
                        warn!(page_id, error = %e, "skill induction: deterministic extractor failed");
                        stats.errors += 1;
                        return stats;
                    }
                }
            }
        }
    } else {
        // No router: use deterministic extraction directly
        match extractor.extract(&trajectory) {
            Ok(Some(c)) => c,
            Ok(None) => {
                stats.rejected += 1;
                return stats;
            }
            Err(e) => {
                warn!(page_id, error = %e, "skill induction: extractor failed");
                stats.errors += 1;
                return stats;
            }
        }
    };

    let existing: Vec<Skill> = library.list_skills(hydragent_skills::library::SkillFilter::default()).await.unwrap_or_default();
    // Deduplicate: Jaccard first (fast), optional semantic second (slow but more accurate)
    let mut dup = extractor.is_duplicate(&candidate.skill, &existing, 0.6);
    if !dup && router.is_some() {
        if let Ok(embedder) = hydragent_embed::LocalEmbedder::new(
            &std::path::Path::new("models/model.safetensors"),
            &std::path::Path::new("models/tokenizer.json"),
        ) {
            if let Ok(true) = extractor.is_duplicate_semantic(&candidate.skill, &existing, &embedder, 0.85) {
                dup = true;
            }
        }
    }
    if dup {
        stats.duplicates_skipped += 1;
        return stats;
    }

    match library.insert_skill(&candidate.skill).await {
        Ok(_) => {
            info!(
                page_id,
                skill = %candidate.skill.name,
                "skill induction: stored Candidate"
            );
            stats.skills_inserted += 1;
        }
        Err(e) => {
            warn!(page_id, error = %e, "skill induction: insert failed");
            stats.errors += 1;
        }
    }
    stats
}

/// Fetch the most recent N (role, content) rows for `page_id` and
/// reshape them into extractor turns. We pull the same window the
/// dream worker just consolidated so induction sees the same
/// trajectory the LLM saw.
async fn fetch_trajectory_turns(
    pool: &SqlitePool,
    page_id: &str,
) -> Result<Vec<TrajectoryTurn>> {
    use sqlx::Row;
    const LIMIT: i64 = 100;
    let rows = sqlx::query(
        "SELECT role, content FROM messages
         WHERE page_id = ?
         ORDER BY timestamp DESC
         LIMIT ?",
    )
    .bind(page_id)
    .bind(LIMIT)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<TrajectoryTurn> = rows
        .into_iter()
        .map(|r| TrajectoryTurn {
            role: r.get::<String, _>("role"),
            content: r.get::<String, _>("content"),
        })
        .collect();
    // Reverse so the trajectory is in chronological order; the
    // extractor treats index 0 as the user's first message.
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydragent_skills::extractor::{SkillExtractor, Trajectory, TrajectoryTurn};
    use tempfile::TempDir;

    /// Helper: build a 3-turn trajectory that the extractor will
    /// accept (user mentions a CSV path, assistant responds with
    /// concrete steps).
    fn csv_trajectory() -> Trajectory {
        Trajectory {
            session_id: "test-session".into(),
            tools_used: vec!["file_read".into()],
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Please convert C:/data/sales.csv to JSON format.".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "I'll read the CSV with file_read and emit JSON.".into(),
                },
                TrajectoryTurn {
                    role: "tool".into(),
                    content: "ok, 42 rows".into(),
                },
            ],
        }
    }

    /// Trajectory that's too short should be rejected by the
    /// extractor itself (it requires `min_turns` >= 2).
    #[tokio::test]
    async fn extractor_rejects_short_trajectory() {
        let t = Trajectory {
            session_id: "x".into(),
            tools_used: vec![],
            turns: vec![TrajectoryTurn {
                role: "user".into(),
                content: "hi".into(),
            }],
        };
        let ex = SkillExtractor::default();
        assert!(ex.extract(&t).unwrap().is_none());
    }

    /// End-to-end: extractor produces a candidate, induction inserts
    /// it as a `Candidate` skill, the second call sees a duplicate
    /// and skips it.
    #[tokio::test]
    async fn end_to_end_insert_then_dedup() {
        let tmp = TempDir::new().unwrap();
        let lib_path = tmp.path().join("skills.sqlite");
        let lib = SkillLibrary::open(&lib_path).await.unwrap();

        let ex = SkillExtractor::default();
        let cand = ex.extract(&csv_trajectory()).expect("extractor should accept CSV trajectory").expect("non-empty candidate");
        let initial_count = lib.list_skills(hydragent_skills::library::SkillFilter::default()).await.unwrap().len();
        assert_eq!(initial_count, 0, "fresh library should be empty");

        // First insert
        lib.insert_skill(&cand.skill).await.unwrap();
        let after_first = lib.list_skills(hydragent_skills::library::SkillFilter::default()).await.unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].tier, hydragent_types::SkillTier::Candidate);
        assert_eq!(after_first[0].author, "extractor");

        // Second extract on the same trajectory -> duplicate
        let existing = lib.list_skills(hydragent_skills::library::SkillFilter::default()).await.unwrap();
        let cand2 = ex.extract(&csv_trajectory()).unwrap().unwrap();
        assert!(
            ex.is_duplicate(&cand2.skill, &existing, 0.6),
            "second run should detect near-duplicate of the first"
        );
    }

    /// Trajectory that doesn't include any placeholders should be
    /// rejected (the heuristic only returns skills when it can find
    /// at least one `{{param}}` to extract).
    #[tokio::test]
    async fn rejects_when_no_placeholders() {
        let t = Trajectory {
            session_id: "x".into(),
            tools_used: vec![],
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "hi there".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "hello!".into(),
                },
            ],
        };
        let ex = SkillExtractor::default();
        assert!(ex.extract(&t).unwrap().is_none());
    }

    /// Two different trajectories (different parameter names) should
    /// not be considered duplicates.
    #[tokio::test]
    async fn distinct_trajectories_are_not_duplicates() {
        let ex = SkillExtractor::default();
        let csv = ex.extract(&csv_trajectory()).unwrap().unwrap();
        let url = Trajectory {
            session_id: "u".into(),
            tools_used: vec!["web_search".into()],
            turns: vec![
                TrajectoryTurn {
                    role: "user".into(),
                    content: "Summarize https://example.com/article/42 for me.".into(),
                },
                TrajectoryTurn {
                    role: "assistant".into(),
                    content: "Sure, fetching the URL now.".into(),
                },
            ],
        };
        let url_cand = ex.extract(&url).unwrap().unwrap();
        assert!(!ex.is_duplicate(&url_cand.skill, &[csv.skill.clone()], 0.6));
    }
}
