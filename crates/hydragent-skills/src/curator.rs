//! Phase 7 / Track 7.2 - Seven-Day Curator.
//!
//! Once a day, the curator runs over every skill in the library and
//! decides whether to promote, demote, or archive it. The rules are
//! intentionally simple and tunable via [`CuratorPolicy`]:
//!
//! * **Promote** Candidate -> Active if the skill has been executed
//!   at least `min_executions` times in the last 7 days with a
//!   success rate >= `promote_rate`.
//! * **Demote** Active -> Inactive if the skill's 7-day success rate
//!   falls below `demote_rate`.
//! * **Archive** any tier -> Archived if the skill has not been
//!   executed in the last `archive_after_days` days.
//! * **Reset** Inactive -> Active if it bounces back above
//!   `promote_rate` (forgiveness).
//!
//! The curator emits [`CuratorDecision`]s that the orchestrator can
//! log to the audit chain and apply to the library via
//! [`Curator::apply`].

use crate::library::SkillLibrary;
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use hydragent_types::{Skill, SkillTier};
use std::sync::Arc;

/// Tunable thresholds for the curator.
#[derive(Debug, Clone)]
pub struct CuratorPolicy {
    /// Minimum executions in the 7-day window before a Candidate can
    /// be promoted.
    pub min_executions: u32,
    /// 7-day success rate threshold for promotion.
    pub promote_rate: f32,
    /// 7-day success rate below which an Active skill is demoted.
    pub demote_rate: f32,
    /// Skills with no executions in this many days are archived.
    pub archive_after_days: u32,
    /// Cosine similarity threshold for deduplicating candidates.
    pub dedup_similarity_threshold: f32,
}

impl Default for CuratorPolicy {
    fn default() -> Self {
        Self {
            min_executions: 5,
            promote_rate: 0.85,
            demote_rate: 0.5,
            archive_after_days: 30,
            dedup_similarity_threshold: 0.88,
        }
    }
}

impl CuratorPolicy {
    /// Load curator thresholds from config/curator.toml, falling back to default.
    pub fn load() -> Result<Self> {
        let path = std::path::Path::new("config/curator.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let val: toml::Value = toml::from_str(&content)?;
        
        let min_executions = val.get("promotion")
            .and_then(|v| v.get("min_executions"))
            .and_then(|v| v.as_integer())
            .unwrap_or(5) as u32;

        let promote_rate = val.get("promotion")
            .and_then(|v| v.get("min_success_rate"))
            .and_then(|v| v.as_float())
            .unwrap_or(0.85) as f32;

        let demote_rate = val.get("demotion")
            .and_then(|v| v.get("max_failure_rate"))
            .and_then(|v| v.as_float())
            .unwrap_or(0.50) as f32;

        let archive_after_days = val.get("archival")
            .and_then(|v| v.get("idle_days"))
            .and_then(|v| v.as_integer())
            .unwrap_or(30) as u32;

        let dedup_similarity_threshold = val.get("deduplication")
            .and_then(|v| v.get("similarity_threshold"))
            .and_then(|v| v.as_float())
            .unwrap_or(0.88) as f32;

        Ok(Self {
            min_executions,
            promote_rate,
            demote_rate,
            archive_after_days,
            dedup_similarity_threshold,
        })
    }
}

/// A single promotion/demotion/archival decision.
#[derive(Debug, Clone, PartialEq)]
pub struct CuratorDecision {
    pub skill_id: String,
    pub skill_name: String,
    pub from: SkillTier,
    pub to: SkillTier,
    pub reason: String,
}

/// Run over the library, decide, and apply.
pub struct SevenDayCurator {
    pub policy: CuratorPolicy,
    pub library: Arc<SkillLibrary>,
    pub clock: ClockFn,
}

/// Clock injection for testability. Production uses
/// `Utc::now()`; tests can pin time.
pub type ClockFn = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

impl SevenDayCurator {
    pub fn new(library: Arc<SkillLibrary>, policy: CuratorPolicy) -> Self {
        let clock: ClockFn = Arc::new(Utc::now);
        Self { policy, library, clock }
    }

    /// Override the clock (for tests).
    pub fn with_clock(mut self, c: ClockFn) -> Self { self.clock = c; self }

    /// Decide, do not apply. Useful for `--dry-run` CLI and tests.
    pub async fn decide_all(&self) -> Result<Vec<CuratorDecision>> {
        let mut decisions = Vec::new();
        let now = (self.clock)();
        let skills = self.library.list_skills(crate::library::SkillFilter {
            tier: None,
            limit: None,
            offset: None,
            name_contains: None,
            min_success_rate: None,
        }).await?;
        for s in skills {
            if let Some(d) = self.decide_one(&s, now).await? {
                decisions.push(d);
            }
        }
        Ok(decisions)
    }

    /// Decide for one skill. Returns `None` if no change is
    /// warranted.
    pub async fn decide_one(&self, s: &Skill, now: DateTime<Utc>) -> Result<Option<CuratorDecision>> {
        // Archive: no execution in `archive_after_days`.
        let last_seen_ms = s.last_updated;
        let last_seen = DateTime::<Utc>::from_timestamp_millis(last_seen_ms)
            .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
        let age = now.signed_duration_since(last_seen);
        if age > Duration::days(self.policy.archive_after_days as i64)
            && s.tier != SkillTier::Archived
        {
            return Ok(Some(CuratorDecision {
                skill_id: s.id.clone(),
                skill_name: s.name.clone(),
                from: s.tier,
                to: SkillTier::Archived,
                reason: format!("no executions in {} days", age.num_days()),
            }));
        }

        // Compute the 7-day success rate.
        let rate = self.library.success_rate_last_7_days(&s.id).await?;

        match s.tier {
            SkillTier::Candidate => {
                if let Some((r, n)) = rate {
                    if n >= self.policy.min_executions as i64
                        && r as f32 >= self.policy.promote_rate
                    {
                        return Ok(Some(CuratorDecision {
                            skill_id: s.id.clone(),
                            skill_name: s.name.clone(),
                            from: s.tier,
                            to: SkillTier::Active,
                            reason: format!("{n} executions in 7d, success rate {:.0}%", r * 100.0),
                        }));
                    }
                }
            }
            SkillTier::Active => {
                if let Some((r, _n)) = rate {
                    if (r as f32) < self.policy.demote_rate {
                        return Ok(Some(CuratorDecision {
                            skill_id: s.id.clone(),
                            skill_name: s.name.clone(),
                            from: s.tier,
                            to: SkillTier::Inactive,
                            reason: format!("7d success rate {:.0}% < {:.0}%", r * 100.0, self.policy.demote_rate * 100.0),
                        }));
                    }
                }
            }
            SkillTier::Inactive => {
                // Forgiveness: bounce back to Active.
                if let Some((r, n)) = rate {
                    if n >= self.policy.min_executions as i64
                        && r as f32 >= self.policy.promote_rate
                    {
                        return Ok(Some(CuratorDecision {
                            skill_id: s.id.clone(),
                            skill_name: s.name.clone(),
                            from: s.tier,
                            to: SkillTier::Active,
                            reason: format!("recovered: {n} executions in 7d, success rate {:.0}%", r * 100.0),
                        }));
                    }
                }
            }
            SkillTier::Archived => { /* never auto-promote from Archived */ }
        }
        Ok(None)
    }

    /// Apply a single decision by writing it back to the library.
    pub async fn apply(&self, d: &CuratorDecision) -> Result<()> {
        let mut s = self.library.get_skill(&d.skill_id).await?
            .ok_or_else(|| anyhow::anyhow!("skill {} not found while applying decision", d.skill_id))?;

        // Version Rollback on Demotion (Active -> Inactive)
        if d.from == SkillTier::Active && d.to == SkillTier::Inactive {
            if let Some(prev_version) = self.library.get_previous_version(&d.skill_id).await? {
                tracing::info!(
                    skill = %s.name,
                    from_version = s.version,
                    to_version = prev_version.version,
                    "Curator: Rolling back skill to previous version instead of demoting"
                );
                self.library.restore_version(&d.skill_id, &prev_version).await?;
                self.library.reset_skill_stats(&d.skill_id).await?;
                return Ok(());
            }
        }

        s.tier = d.to;
        s.last_updated = (self.clock)().timestamp_millis();
        self.library.update_skill(&s).await?;
        Ok(())
    }

    /// Run candidate deduplication using stored description embeddings.
    pub async fn deduplicate_candidates(&self) -> Result<()> {
        let candidates = self.library.list_skills(crate::library::SkillFilter {
            tier: Some(SkillTier::Candidate),
            ..Default::default()
        }).await?;

        let actives = self.library.list_skills(crate::library::SkillFilter {
            tier: Some(SkillTier::Active),
            ..Default::default()
        }).await?;

        let similarity_threshold = self.policy.dedup_similarity_threshold;

        for candidate in &candidates {
            let cand_emb = self.library.get_embedding(&candidate.id).await?;
            for active in &actives {
                let mut is_dup = false;
                if let (Some(c_e), Some(a_e)) = (&cand_emb, self.library.get_embedding(&active.id).await?) {
                    if let Some(sim) = crate::similarity::cosine_similarity(c_e, &a_e) {
                        if sim >= similarity_threshold {
                            is_dup = true;
                        }
                    }
                } else {
                    // Fall back to Jaccard tag similarity if embeddings aren't populated
                    let sim = crate::similarity::jaccard(&candidate.capability_tags, &active.capability_tags);
                    if sim >= 0.85 {
                        is_dup = true;
                    }
                }

                if is_dup {
                    tracing::info!(
                        candidate = %candidate.name,
                        active = %active.name,
                        "Curator: Deduplication match. Archiving candidate and merging counts."
                    );
                    
                    // Increment the active skill's stats by candidate's stats
                    let mut updated_active = active.clone();
                    updated_active.execution_count += candidate.execution_count;
                    updated_active.success_rate = (updated_active.success_rate + candidate.success_rate) / 2.0;
                    updated_active.last_updated = (self.clock)().timestamp_millis();
                    self.library.update_skill(&updated_active).await?;

                    // Archive the candidate skill
                    let mut updated_cand = candidate.clone();
                    updated_cand.tier = SkillTier::Archived;
                    updated_cand.last_updated = (self.clock)().timestamp_millis();
                    self.library.update_skill(&updated_cand).await?;

                    break;
                }
            }
        }
        Ok(())
    }

    /// Decide and apply in one pass. Returns the applied decisions.
    pub async fn run(&self) -> Result<Vec<CuratorDecision>> {
        // Run deduplication first
        if let Err(e) = self.deduplicate_candidates().await {
            tracing::warn!("Candidate deduplication failed: {e}");
        }

        let decisions = self.decide_all().await?;
        for d in &decisions {
            self.apply(d).await?;
        }
        Ok(decisions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::SkillLibrary;
    use hydragent_types::{Skill, SkillExecutionRecord};
    use std::sync::Arc;

    fn make_skill(id: &str, name: &str, tier: SkillTier, last_updated_ms: i64) -> Skill {
        let now = last_updated_ms;
        Skill {
            id: id.into(),
            name: name.into(),
            version: 1,
            description: "test".into(),
            tier,
            capability_tags: vec!["test".into()],
            params: vec![],
            prompt_template: "do {{x}}".into(),
            required_tools: vec![],
            success_examples: vec![],
            author: "test".into(),
            created_at: now,
            last_updated: now,
            success_rate: 0.0,
            execution_count: 0,
        }
    }

    async fn record(lib: &SkillLibrary, skill_id: &str, success: bool) {
        let rec = SkillExecutionRecord {
            skill_id: skill_id.into(),
            success,
            latency_ms: 10,
            timestamp: chrono::Utc::now().timestamp_millis(),
            params_json: "{}".into(),
            error: None,
        };
        lib.record_execution(skill_id, &rec).await.unwrap();
    }

    #[tokio::test]
    async fn candidate_promotes_with_enough_successes() {
        let lib = Arc::new(SkillLibrary::in_memory().await.unwrap());
        let s = make_skill("s1", "promote-me", SkillTier::Candidate, chrono::Utc::now().timestamp_millis());
        lib.insert_skill(&s).await.unwrap();
        for _ in 0..6 { record(&lib, "s1", true).await; }
        let curator = SevenDayCurator::new(lib.clone(), CuratorPolicy::default());
        let decisions = curator.run().await.unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].from, SkillTier::Candidate);
        assert_eq!(decisions[0].to, SkillTier::Active);
        let updated = lib.get_skill("s1").await.unwrap().unwrap();
        assert_eq!(updated.tier, SkillTier::Active);
    }

    #[tokio::test]
    async fn active_demotes_on_low_success_rate() {
        let lib = Arc::new(SkillLibrary::in_memory().await.unwrap());
        let s = make_skill("s2", "demote-me", SkillTier::Active, chrono::Utc::now().timestamp_millis());
        lib.insert_skill(&s).await.unwrap();
        for _ in 0..4 { record(&lib, "s2", true).await; }
        for _ in 0..6 { record(&lib, "s2", false).await; }
        let curator = SevenDayCurator::new(lib.clone(), CuratorPolicy::default());
        let decisions = curator.run().await.unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].to, SkillTier::Inactive);
    }

    #[tokio::test]
    async fn stale_skill_gets_archived() {
        let lib = Arc::new(SkillLibrary::in_memory().await.unwrap());
        let old_ms = (chrono::Utc::now() - chrono::Duration::days(45)).timestamp_millis();
        let s = make_skill("s3", "stale", SkillTier::Active, old_ms);
        lib.insert_skill(&s).await.unwrap();
        let curator = SevenDayCurator::new(lib.clone(), CuratorPolicy::default());
        let decisions = curator.run().await.unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].to, SkillTier::Archived);
    }

    #[tokio::test]
    async fn no_decision_when_healthy() {
        let lib = Arc::new(SkillLibrary::in_memory().await.unwrap());
        let s = make_skill("s4", "healthy", SkillTier::Active, chrono::Utc::now().timestamp_millis());
        lib.insert_skill(&s).await.unwrap();
        for _ in 0..10 { record(&lib, "s4", true).await; }
        let curator = SevenDayCurator::new(lib.clone(), CuratorPolicy::default());
        let decisions = curator.run().await.unwrap();
        assert!(decisions.is_empty());
    }
}
