//! SQLite-backed store (WAL mode). All methods are synchronous and short; callers in async
//! contexts hold the connection lock only for the duration of a query.
//!
//! The schema is written to be portable to Postgres (TEXT timestamps, JSON in TEXT columns,
//! `RETURNING`), so a `PostgresStore` can implement the same interface later.

use crate::ids::{fmt_time, now, now_str};
use crate::models::*;
use anyhow::{anyhow, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub struct Store {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS teams (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
    api_key_hash TEXT NOT NULL UNIQUE,
    api_key_prefix TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    suspended INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    submission_seq INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS team_members (
    team_id TEXT NOT NULL REFERENCES teams(id),
    name TEXT NOT NULL,
    email TEXT,
    student_id TEXT
);
CREATE INDEX IF NOT EXISTS team_members_team ON team_members(team_id);
CREATE TABLE IF NOT EXISTS submissions (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams(id),
    seq INTEGER NOT NULL,
    artifact_path TEXT NOT NULL,
    artifact_sha256 TEXT NOT NULL,
    artifact_size INTEGER NOT NULL,
    manifest_json TEXT NOT NULL,
    status TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    smoke_json TEXT,
    created_at TEXT NOT NULL,
    validated_at TEXT
);
CREATE INDEX IF NOT EXISTS submissions_team ON submissions(team_id, seq DESC);
CREATE TABLE IF NOT EXISTS active_bots (
    team_id TEXT PRIMARY KEY REFERENCES teams(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id),
    activated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    config_json TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    snapshot_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    error TEXT,
    nightly_date TEXT UNIQUE
);
CREATE INDEX IF NOT EXISTS runs_created ON runs(created_at DESC);
CREATE TABLE IF NOT EXISTS run_participants (
    run_id TEXT NOT NULL REFERENCES runs(id),
    team_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    player_id INTEGER NOT NULL,
    team_name TEXT NOT NULL,
    PRIMARY KEY (run_id, team_id)
);
CREATE TABLE IF NOT EXISTS tournaments (
    run_id TEXT NOT NULL REFERENCES runs(id),
    idx INTEGER NOT NULL,
    seed INTEGER NOT NULL,
    status TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    worker_id TEXT,
    total_hands INTEGER,
    winner_team_id TEXT,
    error TEXT,
    outcome_json TEXT,
    PRIMARY KEY (run_id, idx)
);
CREATE TABLE IF NOT EXISTS placements (
    run_id TEXT NOT NULL,
    tournament_idx INTEGER NOT NULL,
    team_id TEXT NOT NULL,
    place INTEGER NOT NULL,
    hands_played INTEGER NOT NULL,
    PRIMARY KEY (run_id, tournament_idx, team_id)
);
CREATE INDEX IF NOT EXISTS placements_team ON placements(team_id, run_id);
CREATE TABLE IF NOT EXISTS leaderboard_scores (
    run_id TEXT NOT NULL,
    team_id TEXT NOT NULL,
    geo_mean REAL NOT NULL,
    rank INTEGER NOT NULL,
    tournaments_counted INTEGER NOT NULL,
    PRIMARY KEY (run_id, team_id)
);
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    run_id TEXT,
    locked_by TEXT,
    locked_at TEXT,
    heartbeat_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 2,
    created_at TEXT NOT NULL,
    finished_at TEXT,
    error TEXT,
    result_json TEXT
);
CREATE INDEX IF NOT EXISTS jobs_queue ON jobs(status, priority DESC, id ASC);
CREATE INDEX IF NOT EXISTS jobs_run ON jobs(run_id);
CREATE TABLE IF NOT EXISTS workers (
    id TEXT PRIMARY KEY,
    hostname TEXT NOT NULL,
    started_at TEXT NOT NULL,
    heartbeat_at TEXT NOT NULL,
    running_jobs INTEGER NOT NULL DEFAULT 0,
    capacity INTEGER NOT NULL DEFAULT 1,
    info_json TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#;

fn json<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("serialisable")
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn ping(&self) -> Result<()> {
        self.c().query_row("SELECT 1", [], |_| Ok(()))?;
        Ok(())
    }

    // ------------------------------------------------------------------ settings

    pub fn settings(&self) -> Result<PlatformSettings> {
        let c = self.c();
        let raw: Option<String> = c
            .query_row(
                "SELECT value_json FROM settings WHERE key = 'platform'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match raw {
            Some(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
            None => Ok(PlatformSettings::default()),
        }
    }

    pub fn save_settings(&self, s: &PlatformSettings) -> Result<()> {
        self.c().execute(
            "INSERT INTO settings(key, value_json, updated_at) VALUES('platform', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![json(s), now_str()],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let c = self.c();
        let raw: Option<String> = c
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.c().execute(
            "INSERT INTO settings(key, value_json, updated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![key, json(value), now_str()],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------ teams

    fn row_team(r: &Row) -> rusqlite::Result<Team> {
        Ok(Team {
            id: r.get("id")?,
            name: r.get("name")?,
            api_key_hash: r.get("api_key_hash")?,
            api_key_prefix: r.get("api_key_prefix")?,
            is_admin: r.get::<_, i64>("is_admin")? != 0,
            suspended: r.get::<_, i64>("suspended")? != 0,
            created_at: r.get("created_at")?,
            submission_seq: r.get::<_, i64>("submission_seq")? as u64,
        })
    }

    pub fn create_team(
        &self,
        id: &str,
        name: &str,
        api_key_hash: &str,
        api_key_prefix: &str,
        is_admin: bool,
        members: &[TeamMember],
    ) -> Result<Team> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO teams(id, name, api_key_hash, api_key_prefix, is_admin, suspended, created_at, submission_seq)
             VALUES(?1, ?2, ?3, ?4, ?5, 0, ?6, 0)",
            params![id, name, api_key_hash, api_key_prefix, is_admin as i64, now_str()],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                StoreError::Conflict(format!("team name '{}' is already taken", name)).into()
            }
            other => anyhow!(other),
        })?;
        for m in members {
            tx.execute(
                "INSERT INTO team_members(team_id, name, email, student_id) VALUES(?1, ?2, ?3, ?4)",
                params![id, m.name, m.email, m.student_id],
            )?;
        }
        let team = tx.query_row(
            "SELECT * FROM teams WHERE id = ?1",
            params![id],
            Self::row_team,
        )?;
        tx.commit()?;
        Ok(team)
    }

    pub fn team(&self, id: &str) -> Result<Option<Team>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM teams WHERE id = ?1",
                params![id],
                Self::row_team,
            )
            .optional()?)
    }

    pub fn team_by_name(&self, name: &str) -> Result<Option<Team>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM teams WHERE name = ?1",
                params![name],
                Self::row_team,
            )
            .optional()?)
    }

    pub fn team_by_key_hash(&self, hash: &str) -> Result<Option<Team>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM teams WHERE api_key_hash = ?1",
                params![hash],
                Self::row_team,
            )
            .optional()?)
    }

    pub fn team_members(&self, team_id: &str) -> Result<Vec<TeamMember>> {
        let c = self.c();
        let mut st =
            c.prepare("SELECT name, email, student_id FROM team_members WHERE team_id = ?1")?;
        let rows = st.query_map(params![team_id], |r| {
            Ok(TeamMember {
                name: r.get(0)?,
                email: r.get(1)?,
                student_id: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn list_teams(&self) -> Result<Vec<Team>> {
        let c = self.c();
        let mut st = c.prepare("SELECT * FROM teams ORDER BY created_at ASC")?;
        let rows = st.query_map([], Self::row_team)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rotate_key(&self, team_id: &str, hash: &str, prefix: &str) -> Result<()> {
        let n = self.c().execute(
            "UPDATE teams SET api_key_hash = ?1, api_key_prefix = ?2 WHERE id = ?3",
            params![hash, prefix, team_id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound("team".into()).into());
        }
        Ok(())
    }

    pub fn set_suspended(&self, team_id: &str, suspended: bool) -> Result<()> {
        let n = self.c().execute(
            "UPDATE teams SET suspended = ?1 WHERE id = ?2",
            params![suspended as i64, team_id],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound("team".into()).into());
        }
        Ok(())
    }

    // ------------------------------------------------------------------ submissions

    fn row_submission(r: &Row) -> rusqlite::Result<Submission> {
        let manifest: Manifest = serde_json::from_str(&r.get::<_, String>("manifest_json")?)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let smoke: Option<String> = r.get("smoke_json")?;
        Ok(Submission {
            id: r.get("id")?,
            team_id: r.get("team_id")?,
            seq: r.get::<_, i64>("seq")? as u64,
            artifact_path: r.get("artifact_path")?,
            artifact_sha256: r.get("artifact_sha256")?,
            artifact_size: r.get::<_, i64>("artifact_size")? as u64,
            manifest,
            status: SubmissionStatus::parse(&r.get::<_, String>("status")?),
            protocol_version: r.get("protocol_version")?,
            smoke_test: smoke.and_then(|s| serde_json::from_str(&s).ok()),
            created_at: r.get("created_at")?,
            validated_at: r.get("validated_at")?,
        })
    }

    /// Allocate the next submission sequence number for a team and insert the row.
    pub fn create_submission(
        &self,
        team_id: &str,
        artifact_path_fn: impl FnOnce(&str) -> String,
        artifact_sha256: &str,
        artifact_size: u64,
        manifest: &Manifest,
    ) -> Result<Submission> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let seq: i64 = tx.query_row(
            "UPDATE teams SET submission_seq = submission_seq + 1 WHERE id = ?1 RETURNING submission_seq",
            params![team_id],
            |r| r.get(0),
        )?;
        let id = crate::ids::submission_id(team_id, seq as u64);
        let path = artifact_path_fn(&id);
        tx.execute(
            "INSERT INTO submissions(id, team_id, seq, artifact_path, artifact_sha256, artifact_size, manifest_json, status, protocol_version, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)",
            params![id, team_id, seq, path, artifact_sha256, artifact_size as i64, json(manifest), manifest.protocol_version, now_str()],
        )?;
        let sub = tx.query_row(
            "SELECT * FROM submissions WHERE id = ?1",
            params![id],
            Self::row_submission,
        )?;
        tx.commit()?;
        Ok(sub)
    }

    pub fn submission(&self, id: &str) -> Result<Option<Submission>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM submissions WHERE id = ?1",
                params![id],
                Self::row_submission,
            )
            .optional()?)
    }

    pub fn list_submissions(
        &self,
        team_id: &str,
        limit: usize,
        before_seq: Option<u64>,
    ) -> Result<Vec<Submission>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT * FROM submissions WHERE team_id = ?1 AND status != 'deleted' AND (?2 IS NULL OR seq < ?2)
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = st.query_map(
            params![team_id, before_seq.map(|s| s as i64), limit as i64],
            Self::row_submission,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_submission_status(
        &self,
        id: &str,
        status: SubmissionStatus,
        smoke: Option<&table_runner::SmokeReport>,
    ) -> Result<()> {
        let validated_at = if status == SubmissionStatus::Validated {
            Some(now_str())
        } else {
            None
        };
        self.c().execute(
            "UPDATE submissions SET status = ?1, smoke_json = COALESCE(?2, smoke_json), validated_at = COALESCE(?3, validated_at) WHERE id = ?4",
            params![status.as_str(), smoke.map(json), validated_at, id],
        )?;
        Ok(())
    }

    pub fn active_submission_id(&self, team_id: &str) -> Result<Option<String>> {
        Ok(self
            .c()
            .query_row(
                "SELECT submission_id FROM active_bots WHERE team_id = ?1",
                params![team_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Make `submission_id` the team's active bot. Returns the previously active submission id.
    pub fn activate(&self, team_id: &str, submission_id: &str) -> Result<Option<String>> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let prev: Option<String> = tx
            .query_row(
                "SELECT submission_id FROM active_bots WHERE team_id = ?1",
                params![team_id],
                |r| r.get(0),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO active_bots(team_id, submission_id, activated_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(team_id) DO UPDATE SET submission_id = excluded.submission_id, activated_at = excluded.activated_at",
            params![team_id, submission_id, now_str()],
        )?;
        tx.commit()?;
        Ok(prev)
    }

    pub fn deactivate(&self, team_id: &str) -> Result<()> {
        self.c().execute(
            "DELETE FROM active_bots WHERE team_id = ?1",
            params![team_id],
        )?;
        Ok(())
    }

    /// Snapshot of all active bots (validated, team not suspended), ordered by team name.
    pub fn active_bots_snapshot(&self) -> Result<Vec<Participant>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT t.id, t.name, a.submission_id FROM active_bots a
             JOIN teams t ON t.id = a.team_id
             JOIN submissions s ON s.id = a.submission_id
             WHERE t.suspended = 0 AND s.status = 'validated'
             ORDER BY t.name ASC",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for (i, row) in rows.enumerate() {
            let (team_id, team_name, submission_id) = row?;
            out.push(Participant {
                team_id,
                team_name,
                submission_id,
                player_id: i as u32 + 1,
            });
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ runs

    fn row_run(r: &Row) -> rusqlite::Result<Run> {
        let config: RunConfig =
            serde_json::from_str(&r.get::<_, String>("config_json")?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        let participants: Vec<Participant> =
            serde_json::from_str(&r.get::<_, String>("snapshot_json")?).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        Ok(Run {
            id: r.get("id")?,
            kind: RunKind::parse(&r.get::<_, String>("kind")?),
            status: RunStatus::parse(&r.get::<_, String>("status")?),
            config,
            participants,
            created_by: r.get("created_by")?,
            created_at: r.get("created_at")?,
            snapshot_at: r.get("snapshot_at")?,
            started_at: r.get("started_at")?,
            finished_at: r.get("finished_at")?,
            error: r.get("error")?,
            nightly_date: r.get("nightly_date")?,
        })
    }

    pub fn create_run(
        &self,
        id: &str,
        kind: RunKind,
        config: &RunConfig,
        participants: &[Participant],
        created_by: &str,
        nightly_date: Option<&str>,
    ) -> Result<Run> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let now = now_str();
        tx.execute(
            "INSERT INTO runs(id, kind, status, config_json, snapshot_json, created_by, created_at, snapshot_at, nightly_date)
             VALUES(?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?6, ?7)",
            params![id, kind.as_str(), json(config), json(participants), created_by, now, nightly_date],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                StoreError::Conflict("a nightly run for that date already exists".into()).into()
            }
            other => anyhow!(other),
        })?;
        for p in participants {
            tx.execute(
                "INSERT INTO run_participants(run_id, team_id, submission_id, player_id, team_name) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![id, p.team_id, p.submission_id, p.player_id as i64, p.team_name],
            )?;
        }
        for idx in 0..config.series_length {
            let seed = table_runner::mix(config.tournament.rng_seed, idx as u64 + 1);
            tx.execute(
                "INSERT INTO tournaments(run_id, idx, seed, status) VALUES(?1, ?2, ?3, 'queued')",
                params![id, idx as i64, seed as i64],
            )?;
        }
        let run = tx.query_row(
            "SELECT * FROM runs WHERE id = ?1",
            params![id],
            Self::row_run,
        )?;
        tx.commit()?;
        Ok(run)
    }

    pub fn run(&self, id: &str) -> Result<Option<Run>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM runs WHERE id = ?1",
                params![id],
                Self::row_run,
            )
            .optional()?)
    }

    pub fn list_runs(
        &self,
        status: Option<RunStatus>,
        kind: Option<RunKind>,
        limit: usize,
        before: Option<&str>,
    ) -> Result<Vec<Run>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT * FROM runs WHERE (?1 IS NULL OR status = ?1) AND (?2 IS NULL OR kind = ?2) AND (?3 IS NULL OR created_at < ?3)
             ORDER BY created_at DESC LIMIT ?4",
        )?;
        let rows = st.query_map(
            params![
                status.map(|s| s.as_str()),
                kind.map(|k| k.as_str()),
                before,
                limit as i64
            ],
            Self::row_run,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn latest_completed_run(&self, kind: Option<RunKind>) -> Result<Option<Run>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM runs WHERE status = 'completed' AND (?1 IS NULL OR kind = ?1) ORDER BY finished_at DESC LIMIT 1",
                params![kind.map(|k| k.as_str())],
                Self::row_run,
            )
            .optional()?)
    }

    /// Compare-and-set the run status. Returns true if the transition happened.
    pub fn transition_run(
        &self,
        id: &str,
        from: &[RunStatus],
        to: RunStatus,
        error: Option<&str>,
    ) -> Result<bool> {
        let now = now_str();
        let from_list: Vec<&str> = from.iter().map(|s| s.as_str()).collect();
        let placeholders = from_list.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE runs SET status = ?1,
                started_at = CASE WHEN ?1 = 'running' AND started_at IS NULL THEN ?2 ELSE started_at END,
                finished_at = CASE WHEN ?1 IN ('completed','failed','cancelled') THEN ?2 ELSE finished_at END,
                error = COALESCE(?3, error)
             WHERE id = ?4 AND status IN ({placeholders})"
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(to.as_str().to_string()),
            Box::new(now),
            Box::new(error.map(|e| e.to_string())),
            Box::new(id.to_string()),
        ];
        for f in from_list {
            args.push(Box::new(f.to_string()));
        }
        let n = self.c().execute(
            &sql,
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
        )?;
        Ok(n > 0)
    }

    pub fn count_recent_ondemand_runs(&self, team_id: &str, hours: i64) -> Result<u32> {
        let since = fmt_time(&(now() - ChronoDuration::hours(hours)));
        let n: i64 = self.c().query_row(
            "SELECT COUNT(*) FROM runs WHERE created_by = ?1 AND kind = 'ondemand' AND created_at >= ?2",
            params![team_id, since],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    // ------------------------------------------------------------------ tournaments

    fn row_tournament(r: &Row) -> rusqlite::Result<TournamentRow> {
        let outcome: Option<String> = r.get("outcome_json")?;
        Ok(TournamentRow {
            run_id: r.get("run_id")?,
            index: r.get::<_, i64>("idx")? as usize,
            seed: r.get::<_, i64>("seed")? as u64,
            status: TournamentStatus::parse(&r.get::<_, String>("status")?),
            started_at: r.get("started_at")?,
            finished_at: r.get("finished_at")?,
            worker_id: r.get("worker_id")?,
            total_hands: r.get::<_, Option<i64>>("total_hands")?.map(|v| v as u64),
            winner_team_id: r.get("winner_team_id")?,
            error: r.get("error")?,
            outcome: outcome.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }

    pub fn tournaments(&self, run_id: &str) -> Result<Vec<TournamentRow>> {
        let c = self.c();
        let mut st = c.prepare("SELECT * FROM tournaments WHERE run_id = ?1 ORDER BY idx ASC")?;
        let rows = st.query_map(params![run_id], Self::row_tournament)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tournament(&self, run_id: &str, idx: usize) -> Result<Option<TournamentRow>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM tournaments WHERE run_id = ?1 AND idx = ?2",
                params![run_id, idx as i64],
                Self::row_tournament,
            )
            .optional()?)
    }

    pub fn tournament_counts(&self, run_id: &str) -> Result<(usize, usize, usize)> {
        let c = self.c();
        let (total, completed, terminal): (i64, i64, i64) = c.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN status IN ('completed','failed','cancelled') THEN 1 ELSE 0 END)
             FROM tournaments WHERE run_id = ?1",
            params![run_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )?;
        Ok((total as usize, completed as usize, terminal as usize))
    }

    pub fn start_tournament(&self, run_id: &str, idx: usize, worker_id: &str) -> Result<bool> {
        let n = self.c().execute(
            "UPDATE tournaments SET status = 'running', started_at = ?1, worker_id = ?2 WHERE run_id = ?3 AND idx = ?4 AND status IN ('queued','running')",
            params![now_str(), worker_id, run_id, idx as i64],
        )?;
        Ok(n > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_tournament(
        &self,
        run_id: &str,
        idx: usize,
        status: TournamentStatus,
        total_hands: Option<u64>,
        winner_team_id: Option<&str>,
        error: Option<&str>,
        outcome: Option<&serde_json::Value>,
        placements: &[PlacementRow],
    ) -> Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE tournaments SET status = ?1, finished_at = ?2, total_hands = ?3, winner_team_id = ?4, error = ?5, outcome_json = ?6
             WHERE run_id = ?7 AND idx = ?8",
            params![
                status.as_str(),
                now_str(),
                total_hands.map(|h| h as i64),
                winner_team_id,
                error,
                outcome.map(json),
                run_id,
                idx as i64
            ],
        )?;
        tx.execute(
            "DELETE FROM placements WHERE run_id = ?1 AND tournament_idx = ?2",
            params![run_id, idx as i64],
        )?;
        for p in placements {
            tx.execute(
                "INSERT INTO placements(run_id, tournament_idx, team_id, place, hands_played) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![run_id, idx as i64, p.team_id, p.place as i64, p.hands_played as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn cancel_pending_tournaments(&self, run_id: &str) -> Result<usize> {
        let n = self.c().execute(
            "UPDATE tournaments SET status = 'cancelled', finished_at = ?1 WHERE run_id = ?2 AND status = 'queued'",
            params![now_str(), run_id],
        )?;
        Ok(n)
    }

    pub fn placements(&self, run_id: &str, idx: Option<usize>) -> Result<Vec<PlacementRow>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT run_id, tournament_idx, team_id, place, hands_played FROM placements
             WHERE run_id = ?1 AND (?2 IS NULL OR tournament_idx = ?2) ORDER BY tournament_idx, place",
        )?;
        let rows = st.query_map(params![run_id, idx.map(|i| i as i64)], |r| {
            Ok(PlacementRow {
                run_id: r.get(0)?,
                tournament_index: r.get::<_, i64>(1)? as usize,
                team_id: r.get(2)?,
                place: r.get::<_, i64>(3)? as usize,
                hands_played: r.get::<_, i64>(4)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn team_placements(&self, team_id: &str, limit: usize) -> Result<Vec<PlacementRow>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT p.run_id, p.tournament_idx, p.team_id, p.place, p.hands_played FROM placements p
             JOIN runs r ON r.id = p.run_id
             WHERE p.team_id = ?1 ORDER BY r.created_at DESC, p.tournament_idx DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![team_id, limit as i64], |r| {
            Ok(PlacementRow {
                run_id: r.get(0)?,
                tournament_index: r.get::<_, i64>(1)? as usize,
                team_id: r.get(2)?,
                place: r.get::<_, i64>(3)? as usize,
                hands_played: r.get::<_, i64>(4)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------ leaderboard

    pub fn save_scores(&self, run_id: &str, scores: &[ScoreRow]) -> Result<()> {
        let mut c = self.c();
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM leaderboard_scores WHERE run_id = ?1",
            params![run_id],
        )?;
        for s in scores {
            tx.execute(
                "INSERT INTO leaderboard_scores(run_id, team_id, geo_mean, rank, tournaments_counted) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![run_id, s.team_id, s.geo_mean, s.rank as i64, s.tournaments_counted as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn scores(&self, run_id: &str) -> Result<Vec<ScoreRow>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT run_id, team_id, geo_mean, rank, tournaments_counted FROM leaderboard_scores WHERE run_id = ?1 ORDER BY rank ASC, team_id ASC",
        )?;
        let rows = st.query_map(params![run_id], |r| {
            Ok(ScoreRow {
                run_id: r.get(0)?,
                team_id: r.get(1)?,
                geo_mean: r.get(2)?,
                rank: r.get::<_, i64>(3)? as usize,
                tournaments_counted: r.get::<_, i64>(4)? as usize,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn team_scores(&self, team_id: &str, limit: usize) -> Result<Vec<ScoreRow>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT s.run_id, s.team_id, s.geo_mean, s.rank, s.tournaments_counted FROM leaderboard_scores s
             JOIN runs r ON r.id = s.run_id WHERE s.team_id = ?1 ORDER BY r.finished_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![team_id, limit as i64], |r| {
            Ok(ScoreRow {
                run_id: r.get(0)?,
                team_id: r.get(1)?,
                geo_mean: r.get(2)?,
                rank: r.get::<_, i64>(3)? as usize,
                tournaments_counted: r.get::<_, i64>(4)? as usize,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------ jobs

    fn row_job(r: &Row) -> rusqlite::Result<Job> {
        let payload: serde_json::Value =
            serde_json::from_str(&r.get::<_, String>("payload_json")?).unwrap_or_default();
        let result: Option<String> = r.get("result_json")?;
        Ok(Job {
            id: r.get("id")?,
            job_type: r.get("type")?,
            payload,
            status: JobStatus::parse(&r.get::<_, String>("status")?),
            priority: r.get("priority")?,
            run_id: r.get("run_id")?,
            locked_by: r.get("locked_by")?,
            locked_at: r.get("locked_at")?,
            heartbeat_at: r.get("heartbeat_at")?,
            attempts: r.get::<_, i64>("attempts")? as u32,
            max_attempts: r.get::<_, i64>("max_attempts")? as u32,
            created_at: r.get("created_at")?,
            finished_at: r.get("finished_at")?,
            error: r.get("error")?,
            result: result.and_then(|s| serde_json::from_str(&s).ok()),
        })
    }

    pub fn enqueue(
        &self,
        job_type: &str,
        payload: &serde_json::Value,
        run_id: Option<&str>,
        priority: i64,
        max_attempts: u32,
    ) -> Result<i64> {
        let c = self.c();
        c.execute(
            "INSERT INTO jobs(type, payload_json, status, priority, run_id, attempts, max_attempts, created_at)
             VALUES(?1, ?2, 'queued', ?3, ?4, 0, ?5, ?6)",
            params![job_type, json(payload), priority, run_id, max_attempts as i64, now_str()],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Atomically claim the next queued job of one of `types`.
    pub fn claim_job(&self, worker_id: &str, types: &[&str]) -> Result<Option<Job>> {
        if types.is_empty() {
            return Ok(None);
        }
        let placeholders = types.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE jobs SET status = 'running', locked_by = ?1, locked_at = ?2, heartbeat_at = ?2, attempts = attempts + 1
             WHERE id = (SELECT id FROM jobs WHERE status = 'queued' AND type IN ({placeholders}) ORDER BY priority DESC, id ASC LIMIT 1)
             RETURNING *"
        );
        let now = now_str();
        let mut args: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(worker_id.to_string()), Box::new(now)];
        for t in types {
            args.push(Box::new(t.to_string()));
        }
        let c = self.c();
        let mut st = c.prepare(&sql)?;
        let job = st
            .query_row(
                rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
                Self::row_job,
            )
            .optional()?;
        Ok(job)
    }

    pub fn heartbeat_job(&self, job_id: i64) -> Result<()> {
        self.c().execute(
            "UPDATE jobs SET heartbeat_at = ?1 WHERE id = ?2",
            params![now_str(), job_id],
        )?;
        Ok(())
    }

    pub fn complete_job(&self, job_id: i64, result: Option<&serde_json::Value>) -> Result<()> {
        self.c().execute(
            "UPDATE jobs SET status = 'done', finished_at = ?1, result_json = ?2, error = NULL WHERE id = ?3",
            params![now_str(), result.map(json), job_id],
        )?;
        Ok(())
    }

    /// Mark a job failed; it is re-queued if attempts remain and `retry` is true.
    pub fn fail_job(&self, job_id: i64, error: &str, retry: bool) -> Result<JobStatus> {
        let c = self.c();
        let (attempts, max): (i64, i64) = c.query_row(
            "SELECT attempts, max_attempts FROM jobs WHERE id = ?1",
            params![job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let status = if retry && attempts < max {
            JobStatus::Queued
        } else {
            JobStatus::Failed
        };
        c.execute(
            "UPDATE jobs SET status = ?1, error = ?2, finished_at = CASE WHEN ?1 = 'failed' THEN ?3 ELSE NULL END, locked_by = NULL WHERE id = ?4",
            params![status.as_str(), error, now_str(), job_id],
        )?;
        Ok(status)
    }

    /// Re-queue running jobs whose heartbeat is older than `stale_secs` (worker died).
    pub fn requeue_stale_jobs(&self, stale_secs: i64) -> Result<usize> {
        let cutoff = fmt_time(&(now() - ChronoDuration::seconds(stale_secs)));
        let n = self.c().execute(
            "UPDATE jobs SET status = CASE WHEN attempts < max_attempts THEN 'queued' ELSE 'failed' END,
                             error = 'worker heartbeat lost', locked_by = NULL
             WHERE status = 'running' AND heartbeat_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    pub fn job(&self, id: i64) -> Result<Option<Job>> {
        Ok(self
            .c()
            .query_row(
                "SELECT * FROM jobs WHERE id = ?1",
                params![id],
                Self::row_job,
            )
            .optional()?)
    }

    pub fn jobs_for_run(&self, run_id: &str) -> Result<Vec<Job>> {
        let c = self.c();
        let mut st = c.prepare("SELECT * FROM jobs WHERE run_id = ?1 ORDER BY id ASC")?;
        let rows = st.query_map(params![run_id], Self::row_job)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn cancel_queued_jobs(&self, run_id: &str) -> Result<usize> {
        let n = self.c().execute(
            "UPDATE jobs SET status = 'failed', error = 'run cancelled', finished_at = ?1 WHERE run_id = ?2 AND status = 'queued'",
            params![now_str(), run_id],
        )?;
        Ok(n)
    }

    pub fn queue_depth(&self) -> Result<Vec<(String, String, i64)>> {
        let c = self.c();
        let mut st = c.prepare("SELECT type, status, COUNT(*) FROM jobs WHERE status IN ('queued','running') GROUP BY type, status")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------ workers

    pub fn upsert_worker(&self, w: &WorkerRow) -> Result<()> {
        self.c().execute(
            "INSERT INTO workers(id, hostname, started_at, heartbeat_at, running_jobs, capacity, info_json)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET heartbeat_at = excluded.heartbeat_at, running_jobs = excluded.running_jobs,
                                          capacity = excluded.capacity, info_json = excluded.info_json",
            params![w.id, w.hostname, w.started_at, w.heartbeat_at, w.running_jobs as i64, w.capacity as i64, json(&w.info)],
        )?;
        Ok(())
    }

    pub fn workers(&self, alive_within_secs: i64) -> Result<Vec<WorkerRow>> {
        let cutoff = fmt_time(&(Utc::now() - ChronoDuration::seconds(alive_within_secs)));
        let c = self.c();
        let mut st =
            c.prepare("SELECT * FROM workers WHERE heartbeat_at >= ?1 ORDER BY started_at ASC")?;
        let rows = st.query_map(params![cutoff], |r| {
            Ok(WorkerRow {
                id: r.get("id")?,
                hostname: r.get("hostname")?,
                started_at: r.get("started_at")?,
                heartbeat_at: r.get("heartbeat_at")?,
                running_jobs: r.get::<_, i64>("running_jobs")? as u32,
                capacity: r.get::<_, i64>("capacity")? as u32,
                info: serde_json::from_str(&r.get::<_, String>("info_json")?).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn remove_worker(&self, id: &str) -> Result<()> {
        self.c()
            .execute("DELETE FROM workers WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ------------------------------------------------------------------ metrics

    pub fn counts(&self) -> Result<serde_json::Value> {
        let c = self.c();
        let teams: i64 = c.query_row("SELECT COUNT(*) FROM teams", [], |r| r.get(0))?;
        let active: i64 = c.query_row("SELECT COUNT(*) FROM active_bots", [], |r| r.get(0))?;
        let subs: i64 = c.query_row("SELECT COUNT(*) FROM submissions", [], |r| r.get(0))?;
        let runs_running: i64 = c.query_row(
            "SELECT COUNT(*) FROM runs WHERE status IN ('queued','running','finalizing')",
            [],
            |r| r.get(0),
        )?;
        let runs_completed: i64 = c.query_row(
            "SELECT COUNT(*) FROM runs WHERE status = 'completed'",
            [],
            |r| r.get(0),
        )?;
        let jobs_queued: i64 = c.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
            [],
            |r| r.get(0),
        )?;
        let jobs_running: i64 = c.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'running'",
            [],
            |r| r.get(0),
        )?;
        let jobs_failed: i64 = c.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'failed'",
            [],
            |r| r.get(0),
        )?;
        Ok(serde_json::json!({
            "teams": teams,
            "active_bots": active,
            "submissions": subs,
            "runs_in_progress": runs_running,
            "runs_completed": runs_completed,
            "jobs_queued": jobs_queued,
            "jobs_running": jobs_running,
            "jobs_failed": jobs_failed,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest {
            language: "python".into(),
            entrypoint: "bot.py".into(),
            runtime: "python3".into(),
            args: vec![],
            notes: None,
            protocol_version: "1".into(),
        }
    }

    #[test]
    fn teams_submissions_activation_roundtrip() {
        let s = Store::open_memory().unwrap();
        let t = s
            .create_team(
                "tm_a",
                "Ace High",
                "hash",
                "pb_live_abcd",
                false,
                &[TeamMember {
                    name: "Ada".into(),
                    email: None,
                    student_id: None,
                }],
            )
            .unwrap();
        assert_eq!(t.name, "Ace High");
        assert!(
            s.create_team("tm_b", "ace high", "hash2", "p", false, &[])
                .is_err(),
            "case-insensitive unique name"
        );
        assert!(s.team_by_key_hash("hash").unwrap().is_some());
        let sub = s
            .create_submission(
                "tm_a",
                |id| format!("{id}/artifact.zip"),
                "sha",
                10,
                &manifest(),
            )
            .unwrap();
        assert_eq!(sub.id, "sub_tm_a_0001");
        assert_eq!(sub.status, SubmissionStatus::Pending);
        let sub2 = s
            .create_submission(
                "tm_a",
                |id| format!("{id}/artifact.zip"),
                "sha",
                10,
                &manifest(),
            )
            .unwrap();
        assert_eq!(sub2.seq, 2);
        s.set_submission_status(&sub.id, SubmissionStatus::Validated, None)
            .unwrap();
        assert!(s.active_bots_snapshot().unwrap().is_empty());
        assert_eq!(s.activate("tm_a", &sub.id).unwrap(), None);
        assert_eq!(s.activate("tm_a", &sub2.id).unwrap(), Some(sub.id.clone()));
        // sub2 is pending => not part of the snapshot
        assert!(s.active_bots_snapshot().unwrap().is_empty());
        s.set_submission_status(&sub2.id, SubmissionStatus::Validated, None)
            .unwrap();
        let snap = s.active_bots_snapshot().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].submission_id, sub2.id);
        assert_eq!(snap[0].player_id, 1);
        let list = s.list_submissions("tm_a", 10, None).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].seq, 2);
    }

    #[test]
    fn job_queue_claims_atomically_and_retries() {
        let s = Store::open_memory().unwrap();
        let id = s
            .enqueue(
                "run_tournament",
                &serde_json::json!({"x": 1}),
                Some("run_1"),
                0,
                2,
            )
            .unwrap();
        s.enqueue(
            "smoke_validate_submission",
            &serde_json::json!({}),
            None,
            10,
            1,
        )
        .unwrap();
        // higher priority first
        let j = s
            .claim_job("w1", &["run_tournament", "smoke_validate_submission"])
            .unwrap()
            .unwrap();
        assert_eq!(j.job_type, "smoke_validate_submission");
        let j2 = s.claim_job("w1", &["run_tournament"]).unwrap().unwrap();
        assert_eq!(j2.id, id);
        assert!(s.claim_job("w1", &["run_tournament"]).unwrap().is_none());
        assert_eq!(s.fail_job(id, "boom", true).unwrap(), JobStatus::Queued);
        let j3 = s.claim_job("w2", &["run_tournament"]).unwrap().unwrap();
        assert_eq!(j3.attempts, 2);
        assert_eq!(s.fail_job(id, "boom", true).unwrap(), JobStatus::Failed);
        s.complete_job(j.id, None).unwrap();
        assert_eq!(s.job(j.id).unwrap().unwrap().status, JobStatus::Done);
    }

    #[test]
    fn runs_tournaments_placements_scores() {
        let s = Store::open_memory().unwrap();
        s.create_team("tm_a", "A", "h1", "p", false, &[]).unwrap();
        s.create_team("tm_b", "B", "h2", "p", false, &[]).unwrap();
        let cfg = RunConfig {
            series_length: 2,
            tournament: Default::default(),
            record_hands: false,
        };
        let parts = vec![
            Participant {
                team_id: "tm_a".into(),
                team_name: "A".into(),
                submission_id: "s1".into(),
                player_id: 1,
            },
            Participant {
                team_id: "tm_b".into(),
                team_name: "B".into(),
                submission_id: "s2".into(),
                player_id: 2,
            },
        ];
        let run = s
            .create_run(
                "run_1",
                RunKind::Nightly,
                &cfg,
                &parts,
                "scheduler",
                Some("2026-08-17"),
            )
            .unwrap();
        assert_eq!(run.status, RunStatus::Queued);
        assert!(s
            .create_run(
                "run_2",
                RunKind::Nightly,
                &cfg,
                &parts,
                "scheduler",
                Some("2026-08-17")
            )
            .is_err());
        assert_eq!(s.tournaments("run_1").unwrap().len(), 2);
        assert!(s
            .transition_run("run_1", &[RunStatus::Queued], RunStatus::Running, None)
            .unwrap());
        assert!(!s
            .transition_run("run_1", &[RunStatus::Queued], RunStatus::Running, None)
            .unwrap());
        assert!(s.start_tournament("run_1", 0, "w1").unwrap());
        s.finish_tournament(
            "run_1",
            0,
            TournamentStatus::Completed,
            Some(10),
            Some("tm_a"),
            None,
            None,
            &[
                PlacementRow {
                    run_id: "run_1".into(),
                    tournament_index: 0,
                    team_id: "tm_a".into(),
                    place: 1,
                    hands_played: 10,
                },
                PlacementRow {
                    run_id: "run_1".into(),
                    tournament_index: 0,
                    team_id: "tm_b".into(),
                    place: 2,
                    hands_played: 10,
                },
            ],
        )
        .unwrap();
        assert_eq!(s.tournament_counts("run_1").unwrap(), (2, 1, 1));
        assert_eq!(s.placements("run_1", None).unwrap().len(), 2);
        s.save_scores(
            "run_1",
            &[ScoreRow {
                run_id: "run_1".into(),
                team_id: "tm_a".into(),
                geo_mean: 1.0,
                rank: 1,
                tournaments_counted: 1,
            }],
        )
        .unwrap();
        assert_eq!(s.scores("run_1").unwrap()[0].team_id, "tm_a");
        assert!(s
            .transition_run(
                "run_1",
                &[RunStatus::Running, RunStatus::Finalizing],
                RunStatus::Completed,
                None
            )
            .unwrap());
        assert_eq!(
            s.latest_completed_run(Some(RunKind::Nightly))
                .unwrap()
                .unwrap()
                .id,
            "run_1"
        );
        assert_eq!(s.count_recent_ondemand_runs("tm_a", 24).unwrap(), 0);
    }
}
