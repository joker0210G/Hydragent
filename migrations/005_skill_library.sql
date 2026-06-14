-- Phase 7 / Track 7.1: Skill Library
-- Stores skills, version history, capability tags, and execution telemetry.
-- Provides FTS5 keyword search over (name, description).

CREATE TABLE IF NOT EXISTS skills (
    id                TEXT    PRIMARY KEY,                -- UUID v4 or "skill-builtin-{name}"
    name              TEXT    NOT NULL UNIQUE,             -- kebab-case, e.g. "convert-csv-to-json"
    description       TEXT    NOT NULL,
    tier              TEXT    NOT NULL
        CHECK (tier IN ('candidate', 'active', 'inactive', 'archived')),
    author            TEXT    NOT NULL DEFAULT 'induced',
    created_at        INTEGER NOT NULL,
    last_updated      INTEGER NOT NULL,
    execution_count   INTEGER NOT NULL DEFAULT 0,
    success_count     INTEGER NOT NULL DEFAULT 0,
    failure_count     INTEGER NOT NULL DEFAULT 0,
    success_rate      REAL    NOT NULL DEFAULT 0.0,
    source_session_id TEXT,                                -- nullable; not set for builtins
    prompt_template   TEXT    NOT NULL DEFAULT '',
    required_tools    TEXT    NOT NULL DEFAULT '[]',       -- JSON array of strings
    capability_tags   TEXT    NOT NULL DEFAULT '[]',       -- JSON array of strings
    params            TEXT    NOT NULL DEFAULT '[]',       -- JSON array of SkillParam objects
    success_examples  TEXT    NOT NULL DEFAULT '[]'        -- JSON array of strings (capped at 5)
);

CREATE INDEX IF NOT EXISTS idx_skills_tier         ON skills(tier);
CREATE INDEX IF NOT EXISTS idx_skills_success_rate ON skills(success_rate DESC);
CREATE INDEX IF NOT EXISTS idx_skills_updated      ON skills(last_updated DESC);
CREATE INDEX IF NOT EXISTS idx_skills_author       ON skills(author);

-- FTS5 virtual table for keyword search. We store a denormalised
-- copy of (name, description) so we don't depend on a contentless
-- join, and we keep an UNINDEXED `skill_id` column to JOIN back to
-- the source row. FTS5's internal rowid is an auto-increment INTEGER.
CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts
USING fts5(
    name,
    description,
    skill_id UNINDEXED,
    tokenize='unicode61'
);

-- Auto-sync triggers: keep FTS index in lock-step with the source table.
CREATE TRIGGER IF NOT EXISTS skills_ai AFTER INSERT ON skills BEGIN
    INSERT INTO skills_fts(name, description, skill_id)
        VALUES (new.name, new.description, new.id);
END;
CREATE TRIGGER IF NOT EXISTS skills_ad AFTER DELETE ON skills BEGIN
    DELETE FROM skills_fts WHERE skill_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS skills_au AFTER UPDATE ON skills BEGIN
    DELETE FROM skills_fts WHERE skill_id = old.id;
    INSERT INTO skills_fts(name, description, skill_id)
        VALUES (new.name, new.description, new.id);
END;

-- Append-only version history. Updated skills get a new row.
CREATE TABLE IF NOT EXISTS skill_versions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id     TEXT    NOT NULL,
    version      INTEGER NOT NULL,
    spec_yaml    TEXT    NOT NULL,                          -- full YAML at this version
    changed_by   TEXT    NOT NULL,                          -- "curator", "user", "extractor", "builtin"
    change_note  TEXT,
    created_at   INTEGER NOT NULL,
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_skill_versions_unique ON skill_versions(skill_id, version);
CREATE INDEX IF NOT EXISTS idx_skill_versions_skill       ON skill_versions(skill_id, version DESC);

-- Per-execution telemetry. Powers the 7-day curator.
CREATE TABLE IF NOT EXISTS skill_executions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    skill_id        TEXT    NOT NULL,
    session_id      TEXT,
    executed_at     INTEGER NOT NULL,                      -- Unix epoch ms
    success         INTEGER NOT NULL,                      -- 0 or 1 (SQLite boolean)
    execution_ms    INTEGER NOT NULL,
    error_message   TEXT,
    input_hash      TEXT,                                  -- SHA-256 of params_json (dedup; no PII)
    params_json     TEXT    NOT NULL DEFAULT '{}',         -- JSON of input params (for replay)
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_skill_executions_skill_recent
    ON skill_executions(skill_id, executed_at DESC);
CREATE INDEX IF NOT EXISTS idx_skill_executions_recent
    ON skill_executions(executed_at DESC);

-- Capability tags, extracted out of the JSON blob for fast lookup.
CREATE TABLE IF NOT EXISTS skill_tags (
    skill_id TEXT NOT NULL,
    tag      TEXT NOT NULL,
    PRIMARY KEY(skill_id, tag),
    FOREIGN KEY(skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_skill_tags_tag ON skill_tags(tag);
