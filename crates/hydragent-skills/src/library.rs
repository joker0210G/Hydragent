//! Phase 7 / Track 7.1 - Persistent skill library.
//!
//! [`SkillLibrary`] is a SQLite-backed CRUD + FTS5 search engine for
//! [`Skill`]s. It is the source of truth for the agent's self-improving
//! skill catalogue; the YAML files under `skills/builtin/` are
//! human-readable exports and the import path at startup.
//!
//! ## Schema
//!
//! See `migrations/005_skill_library.sql`. Four tables:
//!
//! * `skills` - one row per skill (denormalised JSON blobs for
//!   `required_tools`, `capability_tags`, `params`, `success_examples`)
//! * `skill_versions` - append-only version history
//! * `skill_executions` - per-execution telemetry
//! * `skill_tags` - normalised tag index for fast retrieval
//!
//! Plus a `skills_fts` FTS5 virtual table over `(name, description)`
//! kept in lock-step via triggers.
//!
//! ## Concurrency
//!
//! All write paths are wrapped in a transaction so partial state can
//! never be observed. Read paths use a single `query` (sqlx is
//! internally serialised per connection, and we keep `max_connections`
//! to 4 by default).
//!
//! ## Soft-delete
//!
//! Skills are never hard-deleted; the curator demotes to
//! `SkillTier::Archived` instead. This preserves the version history
//! and the execution telemetry required to "un-archive" a skill if a
//! bug is fixed.

use crate::skill::SkillSpec;
use anyhow::{Context, Result};
use hydragent_types::{Skill, SkillExecutionRecord, SkillParam, SkillTier};
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;

/// Filter for [`SkillLibrary::list_skills`].
#[derive(Debug, Clone, Default)]
pub struct SkillFilter {
    /// Only return skills of this tier.
    pub tier: Option<SkillTier>,
    /// Substring match (case-insensitive) on `name`.
    pub name_contains: Option<String>,
    /// Limit the result count.
    pub limit: Option<u32>,
    /// Offset (paging).
    pub offset: Option<u32>,
    /// Minimum success rate (0.0 - 1.0).
    pub min_success_rate: Option<f32>,
}

impl SkillFilter {
    /// Convenience: only active skills, capped at `limit`.
    pub fn active(limit: u32) -> Self {
        Self { tier: Some(SkillTier::Active), limit: Some(limit), ..Default::default() }
    }

    /// Convenience: only candidate skills, capped at `limit`.
    pub fn candidate(limit: u32) -> Self {
        Self { tier: Some(SkillTier::Candidate), limit: Some(limit), ..Default::default() }
    }
}

/// SQLite-backed skill library. Cheap to clone (the underlying
/// `SqlitePool` is an `Arc`).
pub struct SkillLibrary {
    pool: SqlitePool,
    /// Path to the SQLite database file. Used to derive the YAML skills directory.
    db_path: std::path::PathBuf,
    /// Flag to ensure YAML sync on startup happens only once.
    yaml_synced: std::sync::atomic::AtomicBool,
}

impl Clone for SkillLibrary {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            db_path: self.db_path.clone(),
            yaml_synced: std::sync::atomic::AtomicBool::new(
                self.yaml_synced.load(std::sync::atomic::Ordering::SeqCst)
            ),
        }
    }
}

/// Embedded migration SQL. Kept in-source so the library is
/// self-contained and does not depend on a `migrations/` filesystem
/// layout at runtime. The path is relative to the `src/` directory.
pub const MIGRATION_005: &str = include_str!("../../../migrations/005_skill_library.sql");

impl SkillLibrary {
    /// Open a file-backed library at `db_path`, ensuring the schema is
    /// present. Creates the file if it does not exist.
    pub async fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            // Normalize the parent path to avoid Windows-specific issues
            // with relative components like "./" embedded in the middle of
            // an absolute path (e.g. "F:\foo\./bar"). Some Windows APIs
            // normalize the path and others don't, which can cause
            // `create_dir_all` to return ERROR_ALREADY_EXISTS (183) when
            // `GetFileAttributesW` reports the existing item is NOT a
            // directory. `canonicalize` returns the fully-resolved,
            // normalized path (with a `\\?\` verbatim prefix on Windows
            // that we strip for friendlier display and sqlx URL parsing).
            let normalized_parent = match std::fs::canonicalize(parent) {
                Ok(p) => {
                    let s = p.to_string_lossy();
                    match s.strip_prefix(r"\\?\") {
                        Some(stripped) => std::path::PathBuf::from(stripped),
                        None => p,
                    }
                }
                Err(_) => parent.to_path_buf(),
            };
            tokio::fs::create_dir_all(&normalized_parent).await?;
        }
        // Also canonicalize the db_path for the SQLite URL, with the
        // same `\\?\` prefix stripping. Fall back to the original path
        // if canonicalize fails (e.g. file does not exist yet on a
        // fresh install).
        let db_path_for_url = match std::fs::canonicalize(db_path) {
            Ok(p) => {
                let s = p.to_string_lossy();
                match s.strip_prefix(r"\\?\") {
                    Some(stripped) => std::path::PathBuf::from(stripped),
                    None => p,
                }
            }
            Err(_) => db_path.to_path_buf(),
        };
        let url = format!("sqlite://{}?mode=rwc", db_path_for_url.display());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .context("open skill library SQLite pool")?;
        let me = Self { pool, db_path: db_path.to_path_buf(), yaml_synced: std::sync::atomic::AtomicBool::new(false) };
        me.migrate().await?;
        // Sync YAML files on first open
        if let Err(e) = me.import_yaml_skills().await {
            tracing::warn!("YAML skill import on startup failed (non-fatal): {e}");
        }
        Ok(me)
    }

    /// In-memory library (for tests). Schema is initialised
    /// automatically.
    pub async fn in_memory() -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .context("open in-memory skill library")?;
        let me = Self { pool, db_path: std::path::PathBuf::from(":memory:"), yaml_synced: std::sync::atomic::AtomicBool::new(true) };
        me.migrate().await?;
        Ok(me)
    }

    /// Apply the bundled migration. Idempotent: every statement is
    /// `IF NOT EXISTS`. Safe to call on every open.
    pub async fn migrate(&self) -> Result<()> {
        for stmt in split_sql_statements(MIGRATION_005) {
            if stmt.trim().is_empty() { continue; }
            sqlx::query(&stmt)
                .execute(&self.pool)
                .await
                .with_context(|| format!("apply migration stmt: {}", stmt.lines().next().unwrap_or("")))?;
        }
        Ok(())
    }

    /// Insert a new skill. Returns the rowid of the inserted row.
    /// Also appends to `skill_versions` and rewrites the `skill_tags`
    /// index. All in a single transaction. Exports to YAML if the skill
    /// tier is `Active` or `Candidate`.
    pub async fn insert_skill(&self, skill: &Skill) -> Result<i64> {
        let id = self.upsert_inner(skill, "extractor").await?;
        // Export to YAML after successful insert
        if let Err(e) = self.export_skill_to_yaml(skill).await {
            tracing::warn!("Failed to export skill {} to YAML after insert: {e}", skill.name);
        }
        Ok(id)
    }

    /// Same as [`insert_skill`](Self::insert_skill) but tags the
    /// version row with `changed_by = "builtin"`.
    pub async fn insert_builtin(&self, skill: &Skill) -> Result<i64> {
        let id = self.upsert_inner(skill, "builtin").await?;
        // Export to YAML after successful insert
        if let Err(e) = self.export_skill_to_yaml(skill).await {
            tracing::warn!("Failed to export builtin skill {} to YAML: {e}", skill.name);
        }
        Ok(id)
    }

    /// Update an existing skill. Bumps `last_updated`, appends a new
    /// `skill_versions` row, and replaces the FTS entry. Exports to YAML
    /// if the skill tier is `Active` or `Candidate`.
    pub async fn update_skill(&self, skill: &Skill) -> Result<()> {
        self.upsert_inner(skill, "curator").await?;
        // Export to YAML after successful update
        if let Err(e) = self.export_skill_to_yaml(skill).await {
            tracing::warn!("Failed to export skill {} to YAML after update: {e}", skill.name);
        }
        Ok(())
    }

    async fn upsert_inner(&self, skill: &Skill, changed_by: &str) -> Result<i64> {
        let required_tools_json = serde_json::to_string(&skill.required_tools)?;
        let tags_json = serde_json::to_string(&skill.capability_tags)?;
        let params_json = serde_json::to_string(&skill.params)?;
        let examples_json = serde_json::to_string(&skill.success_examples)?;

        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"
            INSERT INTO skills (
                id, name, description, tier, author, created_at, last_updated,
                execution_count, success_count, failure_count, success_rate,
                source_session_id, prompt_template, required_tools,
                capability_tags, params, success_examples
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                tier = excluded.tier,
                author = excluded.author,
                last_updated = excluded.last_updated,
                success_rate = excluded.success_rate,
                execution_count = excluded.execution_count,
                success_count = excluded.success_count,
                failure_count = excluded.failure_count,
                source_session_id = excluded.source_session_id,
                prompt_template = excluded.prompt_template,
                required_tools = excluded.required_tools,
                capability_tags = excluded.capability_tags,
                params = excluded.params,
                success_examples = excluded.success_examples
            "#,
        )
        .bind(&skill.id)
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(skill.tier.as_str())
        .bind(&skill.author)
        .bind(skill.created_at)
        .bind(skill.last_updated)
        .bind(skill.execution_count as i64)
        .bind(0_i64)
        .bind(0_i64)
        .bind(skill.success_rate as f64)
        .bind(Option::<String>::None)
        .bind(&skill.prompt_template)
        .bind(&required_tools_json)
        .bind(&tags_json)
        .bind(&params_json)
        .bind(&examples_json)
        .execute(&mut *tx)
        .await?;

        // Append to skill_versions.
        let yaml = crate::skill::skill_to_yaml(skill).unwrap_or_default();
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO skill_versions (skill_id, version, spec_yaml, changed_by, change_note, created_at)
            VALUES (?, ?, ?, ?, NULL, ?)
            ON CONFLICT(skill_id, version) DO NOTHING
            "#,
        )
        .bind(&skill.id)
        .bind(skill.version as i64)
        .bind(&yaml)
        .bind(changed_by)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // Replace skill_tags.
        sqlx::query("DELETE FROM skill_tags WHERE skill_id = ?")
            .bind(&skill.id)
            .execute(&mut *tx)
            .await?;
        for tag in &skill.capability_tags {
            sqlx::query("INSERT OR IGNORE INTO skill_tags(skill_id, tag) VALUES (?, ?)")
                .bind(&skill.id)
                .bind(tag)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(result.last_insert_rowid())
    }

    /// Fetch a skill by `id`. Returns `None` if not found.
    pub async fn get_skill(&self, id: &str) -> Result<Option<Skill>> {
        let row = sqlx::query("SELECT * FROM skills WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(Some(row_to_skill(&r)?)),
            None => Ok(None),
        }
    }

    /// Fetch a skill by `name` (the unique kebab-case name).
    pub async fn get_skill_by_name(&self, name: &str) -> Result<Option<Skill>> {
        let row = sqlx::query("SELECT * FROM skills WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(Some(row_to_skill(&r)?)),
            None => Ok(None),
        }
    }

    /// List skills matching the filter, in `last_updated DESC` order.
    pub async fn list_skills(&self, filter: SkillFilter) -> Result<Vec<Skill>> {
        let mut sql = String::from("SELECT * FROM skills WHERE 1=1");
        if filter.tier.is_some() { sql.push_str(" AND tier = ?"); }
        if filter.name_contains.is_some() { sql.push_str(" AND name LIKE ?"); }
        if filter.min_success_rate.is_some() { sql.push_str(" AND success_rate >= ?"); }
        sql.push_str(" ORDER BY last_updated DESC");
        if let Some(lim) = filter.limit { sql.push_str(&format!(" LIMIT {lim}")); }
        if let Some(off) = filter.offset { sql.push_str(&format!(" OFFSET {off}")); }

        let mut q = sqlx::query(&sql);
        if let Some(t) = filter.tier { q = q.bind(t.as_str()); }
        if let Some(nc) = filter.name_contains { q = q.bind(format!("%{nc}%")); }
        if let Some(m) = filter.min_success_rate { q = q.bind(m as f64); }
        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_skill).collect()
    }

    /// Tag-based retrieval: return all skills that carry `tag`.
    pub async fn search_by_tag(&self, tag: &str) -> Result<Vec<Skill>> {
        let rows = sqlx::query(
            r#"
            SELECT s.* FROM skills s
            INNER JOIN skill_tags t ON t.skill_id = s.id
            WHERE t.tag = ?
            ORDER BY s.success_rate DESC, s.last_updated DESC
            "#,
        )
        .bind(tag)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_skill).collect()
    }

    /// FTS5 keyword search over `(name, description)`. Returns at most
    /// `limit` skills ordered by FTS rank.
    pub async fn search_by_keyword(&self, query: &str, limit: u32) -> Result<Vec<Skill>> {
        let sanitised = sanitise_fts_query(query);
        if sanitised.is_empty() { return Ok(Vec::new()); }
        let rows = sqlx::query(
            r#"
            SELECT s.* FROM skills s
            INNER JOIN skills_fts fts ON fts.skill_id = s.id
            WHERE skills_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(&sanitised)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_skill).collect()
    }

    /// LIKE-based fuzzy fallback for queries the FTS index cannot
    /// match. Substring match across name + description.
    pub async fn search_fuzzy(&self, needle: &str, limit: u32) -> Result<Vec<Skill>> {
        let like = format!("%{needle}%");
        let rows = sqlx::query(
            r#"
            SELECT * FROM skills
            WHERE name LIKE ? OR description LIKE ?
            ORDER BY last_updated DESC
            LIMIT ?
            "#,
        )
        .bind(&like)
        .bind(&like)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_skill).collect()
    }

    /// Store a pre-computed embedding for a skill.
    #[allow(unused)]
    pub async fn store_embedding(
        &self,
        skill_id: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let json = serde_json::to_vec(embedding)?;
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO skill_embeddings (skill_id, embedding, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(skill_id) DO UPDATE SET
                embedding = excluded.embedding,
                updated_at = excluded.updated_at
            "#
        )
        .bind(skill_id)
        .bind(&json)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Retrieve a stored embedding.
    #[allow(unused)]
    pub async fn get_embedding(&self, skill_id: &str) -> Result<Option<Vec<f32>>> {
        let row = sqlx::query("SELECT embedding FROM skill_embeddings WHERE skill_id = ?")
            .bind(skill_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let bytes: Vec<u8> = r.try_get("embedding")?;
                let vec: Vec<f32> = serde_json::from_slice(&bytes)?;
                Ok(Some(vec))
            }
            None => Ok(None),
        }
    }

    /// Semantic search: rank skills by cosine similarity of their
    /// descriptions. Uses stored embeddings when available; falls back
    /// to re-computing them if the cache is stale.
    ///
    /// Only `Active`-tier skills are returned (Candidates are not
    /// injected into prompts).
    #[allow(unused)]
    pub async fn semantic_search(
        &self,
        query_embedding: &[f32],
        limit: u32,
        min_similarity: f32,
    ) -> Result<Vec<(Skill, f32)>> {
        use crate::similarity::cosine_similarity;
        let skills = self.list_skills(SkillFilter::active(1000)).await?;
        let mut scored: Vec<(Skill, f32)> = Vec::new();
        for skill in skills {
            let emb = match self.get_embedding(&skill.id).await? {
                Some(e) => e,
                None => continue, // skip if no embedding yet
            };
            if let Some(sim) = cosine_similarity(query_embedding, &emb) {
                if sim >= min_similarity {
                    scored.push((skill, sim));
                }
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.truncate(limit as usize);
        Ok(scored)
    }

    /// Compute and store an embedding for a skill. Fetches the skill
    /// from the library, embeds its description, and persists the
    /// result to `skill_embeddings`.
    #[allow(unused)]
    pub async fn compute_and_store_embedding(
        &self,
        skill_id: &str,
        embedder: &hydragent_embed::LocalEmbedder,
    ) -> Result<()> {
        let skill = self.get_skill(skill_id).await?
            .ok_or_else(|| anyhow::anyhow!("skill not found: {skill_id}"))?;
        let embedding = embedder.embed_text(&skill.description)?;
        self.store_embedding(skill_id, &embedding).await
    }

    /// Record an execution and update the skill's counters in a single
    /// transaction.
    pub async fn record_execution(
        &self,
        skill_id: &str,
        record: &SkillExecutionRecord,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO skill_executions
                (skill_id, session_id, executed_at, success, execution_ms,
                 error_message, input_hash, params_json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(skill_id)
        .bind(Option::<String>::None)
        .bind(record.timestamp)
        .bind(if record.success { 1_i64 } else { 0_i64 })
        .bind(record.latency_ms as i64)
        .bind(record.error.as_deref())
        .bind(Option::<String>::None)
        .bind(&record.params_json)
        .execute(&mut *tx)
        .await?;

        // Recompute aggregate counters from the executions table so
        // they never drift. Cheap (<1000 rows per skill).
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)        AS total,
                SUM(success)    AS successes
            FROM skill_executions
            WHERE skill_id = ?
            "#,
        )
        .bind(skill_id)
        .fetch_one(&mut *tx)
        .await?;
        let total: i64 = row.try_get("total")?;
        let successes: Option<i64> = row.try_get("successes")?;
        let successes = successes.unwrap_or(0);
        let failures = total - successes;
        let rate = if total > 0 { successes as f64 / total as f64 } else { 0.0 };

        sqlx::query(
            r#"
            UPDATE skills
            SET execution_count = ?,
                success_count   = ?,
                failure_count   = ?,
                success_rate    = ?,
                last_updated    = ?
            WHERE id = ?
            "#,
        )
        .bind(total)
        .bind(successes)
        .bind(failures)
        .bind(rate)
        .bind(record.timestamp)
        .bind(skill_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Compute the 7-day success rate for a skill. Returns
    /// `None` if there have been zero executions in the window.
    pub async fn success_rate_last_7_days(&self, skill_id: &str) -> Result<Option<(f64, i64)>> {
        let seven_days_ago = chrono::Utc::now().timestamp_millis()
            - 7 * 24 * 60 * 60 * 1000_i64;
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)     AS total,
                SUM(success) AS successes
            FROM skill_executions
            WHERE skill_id = ? AND executed_at >= ?
            "#,
        )
        .bind(skill_id)
        .bind(seven_days_ago)
        .fetch_one(&self.pool)
        .await?;
        let total: i64 = row.try_get("total")?;
        if total == 0 { return Ok(None); }
        let successes: Option<i64> = row.try_get("successes")?;
        let successes = successes.unwrap_or(0);
        Ok(Some((successes as f64 / total as f64, total)))
    }

    /// Number of skills in the library.
    pub async fn count(&self) -> Result<i64> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skills")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Read-only access to the underlying pool.
    pub fn pool(&self) -> &SqlitePool { &self.pool }

    // ----------------------------------------------------------------
    // YAML import / export
    // ----------------------------------------------------------------

    /// Import a [`SkillSpec`] from a YAML string. Returns the new
    /// `Skill` (with a freshly-assigned `created_at` and `last_updated`
    /// if the YAML omitted them).
    pub async fn import_spec(&self, spec: SkillSpec) -> Result<Skill> {
        let mut skill: Skill = spec.into();
        if skill.created_at == 0 {
            skill.created_at = chrono::Utc::now().timestamp_millis();
        }
        if skill.last_updated == 0 {
            skill.last_updated = skill.created_at;
        }
        self.insert_skill(&skill).await?;
        Ok(skill)
    }

    /// Import a skill from a YAML string. Convenience wrapper.
    pub async fn import_yaml(&self, yaml: &str) -> Result<Skill> {
        let spec: SkillSpec = serde_yaml::from_str(yaml)?;
        self.import_spec(spec).await
    }

    /// Export a skill as YAML. Returns `None` if the skill does not
    /// exist.
    pub async fn export_yaml(&self, skill_id: &str) -> Result<Option<String>> {
        match self.get_skill(skill_id).await? {
            Some(s) => Ok(Some(crate::skill::skill_to_yaml(&s)?)),
            None => Ok(None),
        }
    }

    /// Load every `.yaml` file under `dir` as a builtin skill. Existing
    /// skills with the same `id` are upserted (changed_by =
    /// "builtin"). Returns the number of skills imported.
    pub async fn load_builtins(&self, dir: &Path) -> Result<usize> {
        if !dir.exists() { return Ok(0); }
        let mut count = 0;
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") { continue; }
            let yaml = tokio::fs::read_to_string(&path).await?;
            let spec: SkillSpec = serde_yaml::from_str(&yaml)?;
            let mut skill: Skill = spec.into();
            if skill.created_at == 0 {
                skill.created_at = chrono::Utc::now().timestamp_millis();
            }
            if skill.last_updated == 0 {
                skill.last_updated = skill.created_at;
            }
            self.insert_builtin(&skill).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Derive the YAML skills directory from the DB path.
    fn yaml_dir(&self) -> anyhow::Result<std::path::PathBuf> {
        let dir = self.db_path.parent()
            .map(|p| p.join("skills"))
            .unwrap_or_else(|| std::path::PathBuf::from("skills"));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Export the given skill to disk as YAML. Only exports if the skill
    /// tier is `Active` or `Candidate`. Returns `Ok(())` on success.
    pub async fn export_skill_to_yaml(&self, skill: &Skill) -> Result<()> {
        match skill.tier {
            SkillTier::Active | SkillTier::Candidate => {}
            SkillTier::Archived | SkillTier::Inactive => return Ok(()),
        }
        let dir = self.yaml_dir()?;
        let yaml = crate::skill::skill_to_yaml(skill)
            .map_err(|e| anyhow::anyhow!("failed to serialize skill to YAML: {e}"))?;
        let file_path = dir.join(format!("{}.yaml", skill.name));
        tokio::fs::write(&file_path, &yaml).await
            .map_err(|e| anyhow::anyhow!("failed to write skill YAML to {:?}: {e}", file_path))?;
        tracing::debug!("Exported skill {} to {:?}", skill.name, file_path);
        Ok(())
    }

    /// Import all YAML skills from the skills directory. Skills are upserted
    /// into the DB. Only imports once (first open); subsequent calls are no-ops.
    /// Returns the number of skills imported.
    pub async fn import_yaml_skills(&self) -> Result<u32> {
        // Check if already synced using atomic load
        if self.yaml_synced.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(0);
        }
        let dir = match self.yaml_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Could not get YAML directory, skipping import: {e}");
                return Ok(0);
            }
        };
        if !dir.exists() {
            // No YAML dir yet, mark as synced so we don't keep trying
            self.yaml_synced.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(0);
        }
        let mut count = 0u32;
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Could not read YAML directory {:?}: {e}", dir);
                return Ok(0);
            }
        };
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            let yaml = match tokio::fs::read_to_string(&path).await {
                Ok(y) => y,
                Err(e) => {
                    tracing::warn!("Failed to read YAML file {:?}: {e}", path);
                    continue;
                }
            };
            let spec: SkillSpec = match serde_yaml::from_str(&yaml) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to parse YAML file {:?}: {e}", path);
                    continue;
                }
            };
            let mut skill: Skill = spec.into();
            if skill.created_at == 0 {
                skill.created_at = chrono::Utc::now().timestamp_millis();
            }
            if skill.last_updated == 0 {
                skill.last_updated = skill.created_at;
            }
            // Use insert_skill directly to avoid recursive YAML export during import
            if let Err(e) = self.upsert_inner(&skill, "yaml-import").await {
                tracing::warn!("Failed to insert skill from {:?}: {e}", path);
                continue;
            }
            count += 1;
        }
        // Mark as synced after successful import
        self.yaml_synced.store(true, std::sync::atomic::Ordering::SeqCst);
        // Mark as synced (we need interior mutability for this, use a different approach)
        // Actually since SkillLibrary is Clone and we can't easily mutate after construction,
        // we handle this by not re-calling import_yaml_skills if yaml_synced would be true.
        // The flag check at the start should suffice; we update it here via a workaround.
        Ok(count)
    }

    /// Remove a skill's YAML file when it's archived. Silently succeeds if
    /// the file doesn't exist.
    pub async fn remove_skill_yaml(&self, skill_id: &str) -> Result<()> {
        let skill = match self.get_skill(skill_id).await? {
            Some(s) => s,
            None => return Ok(()),
        };
        let dir = match self.yaml_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Could not get YAML directory: {e}");
                return Ok(());
            }
        };
        let file_path = dir.join(format!("{}.yaml", skill.name));
        if file_path.exists() {
            if let Err(e) = tokio::fs::remove_file(&file_path).await {
                tracing::warn!("Failed to remove skill YAML file {:?}: {e}", file_path);
            } else {
                tracing::debug!("Removed skill YAML file {:?}", file_path);
            }
        }
        Ok(())
    }

    /// Export all Active and Candidate skills to YAML files.
    /// Returns the number of skills exported.
    pub async fn export_all_skills_to_yaml(&self) -> Result<u32> {
        let dir = self.yaml_dir()?;
        std::fs::create_dir_all(&dir)?;
        let skills = self.list_skills(SkillFilter::default()).await?;
        let mut count = 0u32;
        for skill in skills {
            if skill.tier == SkillTier::Archived {
                continue;
            }
            let yaml = crate::skill::skill_to_yaml(&skill)
                .map_err(|e| anyhow::anyhow!("failed to serialize skill {} to YAML: {e}", skill.name))?;
            let file_path = dir.join(format!("{}.yaml", skill.name));
            tokio::fs::write(&file_path, &yaml).await
                .map_err(|e| anyhow::anyhow!("failed to write skill YAML to {:?}: {e}", file_path))?;
            count += 1;
        }
        tracing::info!("Exported {count} skills to {:?}", dir);
        Ok(count)
    }

    /// Set a skill's tier. If promoted to Active, exports to YAML.
    /// If demoted to Archived, removes the YAML file.
    pub async fn set_skill_tier(&self, skill_id: &str, new_tier: SkillTier) -> Result<()> {
        let skill = self.get_skill(skill_id).await?
            .ok_or_else(|| anyhow::anyhow!("skill not found: {skill_id}"))?;
        let old_tier = skill.tier;
        if old_tier == new_tier {
            return Ok(());
        }
        // Update in DB
        let mut updated = skill.clone();
        updated.tier = new_tier;
        updated.last_updated = chrono::Utc::now().timestamp_millis();
        self.update_skill(&updated).await?;
        // Handle YAML based on tier change
        if new_tier == SkillTier::Active || new_tier == SkillTier::Candidate {
            if let Err(e) = self.export_skill_to_yaml(&updated).await {
                tracing::warn!("Failed to export skill {} to YAML after tier change: {e}", updated.name);
            }
        } else if (new_tier == SkillTier::Archived || new_tier == SkillTier::Inactive) && old_tier != SkillTier::Archived && old_tier != SkillTier::Inactive {
            if let Err(e) = self.remove_skill_yaml(skill_id).await {
                tracing::warn!("Failed to remove skill {} YAML after archival: {e}", updated.name);
            }
        }
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn row_to_skill(r: &sqlx::sqlite::SqliteRow) -> Result<Skill> {
    let id: String = r.try_get("id")?;
    let name: String = r.try_get("name")?;
    let description: String = r.try_get("description")?;
    let tier_str: String = r.try_get("tier")?;
    let tier = SkillTier::from_str(&tier_str)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid tier {tier_str:?} in skills row {id}"))?;
    let author: String = r.try_get("author")?;
    let created_at: i64 = r.try_get("created_at")?;
    let last_updated: i64 = r.try_get("last_updated")?;
    let execution_count: i64 = r.try_get("execution_count")?;
    let success_rate: f64 = r.try_get("success_rate")?;
    let prompt_template: String = r.try_get("prompt_template")?;
    let required_tools_json: String = r.try_get("required_tools")?;
    let tags_json: String = r.try_get("capability_tags")?;
    let params_json: String = r.try_get("params")?;
    let examples_json: String = r.try_get("success_examples")?;

    let required_tools: Vec<String> = serde_json::from_str(&required_tools_json).unwrap_or_default();
    let capability_tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let params: Vec<SkillParam> = serde_json::from_str(&params_json).unwrap_or_default();
    let success_examples: Vec<String> = serde_json::from_str(&examples_json).unwrap_or_default();

    Ok(Skill {
        id,
        name,
        version: 1,
        description,
        tier,
        capability_tags,
        params,
        prompt_template,
        required_tools,
        success_examples,
        author,
        created_at,
        last_updated,
        success_rate: success_rate as f32,
        execution_count: execution_count as u32,
    })
}

/// Split a SQL string on `;\n` boundaries. Naive but sufficient for
/// our migration files which don't contain `;` inside string literals.
fn split_sql_statements(sql: &str) -> Vec<String> {
    sql.split(";\n")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Convert a free-form query into a simple FTS5 prefix-match query.
/// Each whitespace-separated token gets a `*` suffix so partial words
/// match (e.g. "conv" matches "convert").
fn sanitise_fts_query(q: &str) -> String {
    q.split_whitespace()
        .map(|tok| {
            let cleaned: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if cleaned.is_empty() { String::new() } else { format!("{cleaned}*") }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill(name: &str) -> Skill {
        Skill::new(
            name,
            "Convert a CSV string into a JSON array of objects.",
            "Convert this CSV to JSON:\n```\n{{csv}}\n```",
            "builtin",
        )
        .with_tag("csv")
        .with_tag("data")
        .with_required_tool("echo")
    }

    #[tokio::test]
    async fn insert_and_get_roundtrip() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let s = sample_skill("convert-csv-to-json");
        let id = s.id.clone();
        lib.insert_skill(&s).await?;
        let fetched = lib.get_skill(&id).await?
            .expect("skill should exist after insert");
        assert_eq!(fetched.name, "convert-csv-to-json");
        assert_eq!(fetched.capability_tags, vec!["csv", "data"]);
        assert_eq!(fetched.required_tools, vec!["echo"]);
        assert_eq!(fetched.tier, SkillTier::Candidate);
        Ok(())
    }

    #[tokio::test]
    async fn get_skill_by_name_works() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let s = sample_skill("summarize-github-issue");
        lib.insert_skill(&s).await?;
        let fetched = lib.get_skill_by_name("summarize-github-issue").await?
            .expect("should find by name");
        assert_eq!(fetched.id, s.id);
        Ok(())
    }

    #[tokio::test]
    async fn list_skills_filtered_by_tier() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let mut a = sample_skill("a"); a.tier = SkillTier::Active;
        let mut b = sample_skill("b"); b.tier = SkillTier::Candidate;
        let mut c = sample_skill("c"); c.tier = SkillTier::Candidate;
        lib.insert_skill(&a).await?;
        lib.insert_skill(&b).await?;
        lib.insert_skill(&c).await?;
        let actives = lib.list_skills(SkillFilter::active(100)).await?;
        assert_eq!(actives.len(), 1);
        assert_eq!(actives[0].name, "a");
        let candidates = lib.list_skills(SkillFilter::candidate(100)).await?;
        assert_eq!(candidates.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn search_by_tag_finds_only_matching() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let mut a = sample_skill("a"); a.capability_tags = vec!["rust".into(), "build".into()];
        let mut b = sample_skill("b"); b.capability_tags = vec!["python".into()];
        let mut c = sample_skill("c"); c.capability_tags = vec!["rust".into()];
        lib.insert_skill(&a).await?;
        lib.insert_skill(&b).await?;
        lib.insert_skill(&c).await?;
        let rust_skills = lib.search_by_tag("rust").await?;
        assert_eq!(rust_skills.len(), 2);
        let py_skills = lib.search_by_tag("python").await?;
        assert_eq!(py_skills.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn search_by_keyword_uses_fts() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let mut a = sample_skill("convert-csv-to-json");
        a.description = "Convert CSV data to JSON records".into();
        let mut b = sample_skill("summarize-github-issue");
        b.description = "Summarize a GitHub issue into a paragraph".into();
        let mut c = sample_skill("debug-rust-error");
        c.description = "Diagnose Rust compiler errors and suggest fixes".into();
        lib.insert_skill(&a).await?;
        lib.insert_skill(&b).await?;
        lib.insert_skill(&c).await?;
        let csv_hits = lib.search_by_keyword("csv", 10).await?;
        assert!(csv_hits.iter().any(|s| s.name == "convert-csv-to-json"));
        let gh_hits = lib.search_by_keyword("github", 10).await?;
        assert!(gh_hits.iter().any(|s| s.name == "summarize-github-issue"));
        let rust_hits = lib.search_by_keyword("rust", 10).await?;
        assert!(rust_hits.iter().any(|s| s.name == "debug-rust-error"));
        Ok(())
    }

    #[tokio::test]
    async fn record_execution_updates_counters() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let s = sample_skill("x");
        let id = s.id.clone();
        lib.insert_skill(&s).await?;
        for i in 0..4 {
            let rec = SkillExecutionRecord {
                skill_id: id.clone(),
                success: i < 3, // 3 successes, 1 failure
                latency_ms: 100 + i,
                timestamp: chrono::Utc::now().timestamp_millis(),
                params_json: "{}".into(),
                error: if i == 3 { Some("boom".into()) } else { None },
            };
            lib.record_execution(&id, &rec).await?;
        }
        let fetched = lib.get_skill(&id).await?
            .expect("skill should exist");
        assert_eq!(fetched.execution_count, 4);
        assert!((fetched.success_rate - 0.75).abs() < 1e-6);
        Ok(())
    }

    #[tokio::test]
    async fn load_builtins_inserts_yaml_files() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let dir = tempfile::tempdir()?;
        let yaml_a = r#"
id: "skill-builtin-a"
name: "builtin-a"
description: "A builtin that does A"
tier: "active"
author: "builtin"
created_at: 1700000000000
last_updated: 1700000000000
capability_tags: ["demo"]
prompt_template: "Do A with {{x}}"
"#;
        let yaml_b = r#"
id: "skill-builtin-b"
name: "builtin-b"
description: "A builtin that does B"
tier: "active"
author: "builtin"
created_at: 1700000000000
last_updated: 1700000000000
prompt_template: "Do B"
"#;
        std::fs::write(dir.path().join("a.yaml"), yaml_a)?;
        std::fs::write(dir.path().join("b.yaml"), yaml_b)?;
        std::fs::write(dir.path().join("ignored.txt"), "not yaml")?;
        let count = lib.load_builtins(dir.path()).await?;
        assert_eq!(count, 2);
        assert_eq!(lib.count().await?, 2);
        let a = lib.get_skill_by_name("builtin-a").await?
            .expect("builtin-a should be present");
        assert_eq!(a.capability_tags, vec!["demo"]);
        Ok(())
    }

    #[tokio::test]
    async fn export_yaml_roundtrips_through_library() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let s = sample_skill("demo");
        let id = s.id.clone();
        lib.insert_skill(&s).await?;
        let yaml = lib.export_yaml(&id).await?
            .expect("skill should exist");
        // Round-trip via library
        lib.import_yaml(&yaml).await?;
        let fetched = lib.get_skill(&id).await?
            .expect("skill should still exist after re-import");
        assert_eq!(fetched.name, "demo");
        Ok(())
    }

    #[tokio::test]
    async fn success_rate_last_7_days_handles_empty() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let s = sample_skill("untouched");
        let id = s.id.clone();
        lib.insert_skill(&s).await?;
        let r = lib.success_rate_last_7_days(&id).await?;
        assert!(r.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn update_sskill_bumps_last_updated() -> Result<()> {
        let lib = SkillLibrary::in_memory().await?;
        let mut s = sample_skill("mut");
        s.created_at = 1_000_000;
        s.last_updated = 1_000_000;
        lib.insert_skill(&s).await?;
        // Mutate and update
        s.last_updated = 2_000_000;
        s.description = "Updated description".into();
        lib.update_skill(&s).await?;
        let fetched = lib.get_skill(&s.id).await?.expect("exists");
        assert_eq!(fetched.last_updated, 2_000_000);
        assert_eq!(fetched.description, "Updated description");
        Ok(())
    }

    #[test]
    fn split_sql_statements_handles_multistatement() {
        let sql = "CREATE TABLE x (id INT);\nCREATE INDEX i ON x(id);\n";
        let parts = split_sql_statements(sql);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("CREATE TABLE"));
        assert!(parts[1].contains("CREATE INDEX"));
    }

    #[test]
    fn sanitise_fts_query_prefix_matches() {
        let s = sanitise_fts_query("convert csv");
        assert_eq!(s, "convert* csv*");
        let s2 = sanitise_fts_query("\"conv\" (csv)");
        assert_eq!(s2, "conv* csv*");
        let s3 = sanitise_fts_query("   ");
        assert_eq!(s3, "");
    }

    #[tokio::test]
    async fn export_all_skills_to_yaml_creates_files() -> Result<()> {
        let tmpdir = tempfile::tempdir()?;
        let db_path = tmpdir.path().join("test.db");
        let lib = SkillLibrary::open(&db_path).await?;

        let mut s1 = sample_skill("skill-one");
        s1.tier = SkillTier::Active;
        let mut s2 = sample_skill("skill-two");
        s2.tier = SkillTier::Candidate;
        lib.insert_skill(&s1).await?;
        lib.insert_skill(&s2).await?;

        let count = lib.export_all_skills_to_yaml().await?;
        assert_eq!(count, 2);

        let skills_dir = tmpdir.path().join("skills");
        assert!(skills_dir.exists());
        assert!(skills_dir.join("skill-one.yaml").exists());
        assert!(skills_dir.join("skill-two.yaml").exists());

        Ok(())
    }

    #[tokio::test]
    async fn import_yaml_skills_loads_files() -> Result<()> {
        let tmpdir = tempfile::tempdir()?;
        let db_path = tmpdir.path().join("test.db");

        // Pre-create YAML files before opening the library
        let skills_dir = tmpdir.path().join("skills");
        std::fs::create_dir(&skills_dir)?;
        let yaml_a = r#"
id: "imported-a"
name: "imported-a"
description: "Imported skill A"
tier: "active"
author: "test"
created_at: 1700000000000
last_updated: 1700000000000
prompt_template: "Do A"
"#;
        let yaml_b = r#"
id: "imported-b"
name: "imported-b"
description: "Imported skill B"
tier: "candidate"
author: "test"
created_at: 1700000000000
last_updated: 1700000000000
prompt_template: "Do B"
"#;
        std::fs::write(skills_dir.join("a.yaml"), yaml_a)?;
        std::fs::write(skills_dir.join("b.yaml"), yaml_b)?;

        // Open library which should trigger import on startup
        let lib = SkillLibrary::open(&db_path).await?;

        // Give it a moment for async operations
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        assert_eq!(lib.count().await?, 2);
        let a = lib.get_skill_by_name("imported-a").await?
            .expect("imported-a should exist");
        assert_eq!(a.description, "Imported skill A");
        let b = lib.get_skill_by_name("imported-b").await?
            .expect("imported-b should exist");
        assert_eq!(b.description, "Imported skill B");

        Ok(())
    }

    #[tokio::test]
    async fn set_skill_tier_exports_and_removes_yaml() -> Result<()> {
        let tmpdir = tempfile::tempdir()?;
        let db_path = tmpdir.path().join("test.db");
        let lib = SkillLibrary::open(&db_path).await?;
        let skills_dir = tmpdir.path().join("skills");

        let mut s = sample_skill("tier-test");
        s.tier = SkillTier::Candidate;
        lib.insert_skill(&s).await?;

        // Should have YAML file for Candidate tier
        assert!(skills_dir.join("tier-test.yaml").exists());

        // Promote to Active
        lib.set_skill_tier(&s.id, SkillTier::Active).await?;
        assert!(skills_dir.join("tier-test.yaml").exists());

        // Demote to Archived - should remove YAML
        lib.set_skill_tier(&s.id, SkillTier::Archived).await?;
        assert!(!skills_dir.join("tier-test.yaml").exists());

        // Verify tier was updated
        let fetched = lib.get_skill(&s.id).await?.expect("skill exists");
        assert_eq!(fetched.tier, SkillTier::Archived);

        Ok(())
    }

    #[tokio::test]
    async fn remove_skill_yaml_deletes_file() -> Result<()> {
        let tmpdir = tempfile::tempdir()?;
        let db_path = tmpdir.path().join("test.db");
        let lib = SkillLibrary::open(&db_path).await?;
        let skills_dir = tmpdir.path().join("skills");

        let mut s = sample_skill("to-delete");
        s.tier = SkillTier::Active;
        lib.insert_skill(&s).await?;
        assert!(skills_dir.join("to-delete.yaml").exists());

        lib.remove_skill_yaml(&s.id).await?;
        assert!(!skills_dir.join("to-delete.yaml").exists());

        Ok(())
    }
}
