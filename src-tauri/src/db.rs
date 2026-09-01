//! Schema versioning and migrations.
//!
//! The database carries its version in SQLite's `user_version` header field.
//! [`prepare`] runs at startup, before any data is read, and brings the file up
//! to [`SCHEMA_VERSION`] one migration at a time. Installations that predate
//! versioning report `user_version = 0`; they are recognised by the tables they
//! already contain and treated as version 1.

use crate::{TrackerError, normalize_subtask_name, subtask_name_key};
use chrono::Utc;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Schema version this build reads and writes.
pub(crate) const SCHEMA_VERSION: i64 = 2;

/// Version marker used for installations created before schema versioning.
const LEGACY_VERSION: i64 = 1;

/// Reported by [`detect_version`] when the file holds no Tracker tables yet.
const EMPTY_VERSION: i64 = 0;

/// Outcome of [`prepare`].
///
/// `backup_path` is reported even when `result` is an error, so a failed
/// migration can tell the user where the untouched copy of their data is.
pub(crate) struct Prepared {
    pub backup_path: Option<PathBuf>,
    pub result: Result<(), TrackerError>,
}

/// Opens a connection for normal application use.
///
/// This never changes the schema; [`prepare`] owns that.
pub(crate) fn open(path: &Path) -> Result<Connection, TrackerError> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

/// Creates or migrates the database at `path` so it matches [`SCHEMA_VERSION`].
///
/// A database that needs migrating is copied to a timestamped backup first.
pub(crate) fn prepare(path: &Path) -> Prepared {
    let mut backup_path = None;
    let result = prepare_inner(path, &mut backup_path);
    Prepared {
        backup_path,
        result,
    }
}

fn prepare_inner(path: &Path, backup_path: &mut Option<PathBuf>) -> Result<(), TrackerError> {
    let mut conn = open(path)?;
    let version = detect_version(&conn)?;

    if version == SCHEMA_VERSION {
        return Ok(());
    }

    if version > SCHEMA_VERSION {
        return Err(TrackerError::SchemaTooNew {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    if version == EMPTY_VERSION {
        create_latest_schema(&conn)?;
        set_version(&conn, SCHEMA_VERSION)?;
        return Ok(());
    }

    // There is existing data to migrate, so keep a copy of the file as it
    // stands before touching it. Close the connection first so the backup
    // cannot capture a partially flushed database.
    drop(conn);
    *backup_path = Some(backup_database(path, version)?);
    conn = open(path)?;

    run_migrations(&mut conn, version)
}

/// Reads the schema version, recognising pre-versioning installations.
fn detect_version(conn: &Connection) -> Result<i64, TrackerError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > 0 {
        return Ok(version);
    }

    if has_tracker_tables(conn)? {
        Ok(LEGACY_VERSION)
    } else {
        Ok(EMPTY_VERSION)
    }
}

fn has_tracker_tables(conn: &Connection) -> Result<bool, TrackerError> {
    let count: i64 = conn.query_row(
        "
        SELECT COUNT(*)
        FROM sqlite_master
        WHERE type = 'table' AND name IN ('tasks', 'subtasks', 'time_entries')
        ",
        [],
        |row| row.get(0),
    )?;

    Ok(count > 0)
}

fn set_version(conn: &Connection, version: i64) -> Result<(), TrackerError> {
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))?;
    Ok(())
}

fn backup_database(path: &Path, version: i64) -> Result<PathBuf, TrackerError> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = format!("tracker-backup-v{version}-{stamp}.sqlite3");
    let backup_path = path
        .parent()
        .map(|parent| parent.join(&name))
        .unwrap_or_else(|| PathBuf::from(&name));

    std::fs::copy(path, &backup_path)?;
    Ok(backup_path)
}

fn run_migrations(conn: &mut Connection, from_version: i64) -> Result<(), TrackerError> {
    let mut version = from_version;

    if version == LEGACY_VERSION {
        // Pre-versioning installations were kept up to date by ad-hoc column
        // top-ups, so bring every one of them to the same shape before the
        // numbered migrations start.
        apply_v1_schema(conn)?;
    }

    while version < SCHEMA_VERSION {
        let next = version + 1;
        match next {
            2 => migrate_1_to_2(conn)?,
            _ => return Err(TrackerError::MissingMigration(next)),
        }
        version = next;
    }

    Ok(())
}

/// The schema as it stood before subtasks became independent of tasks.
///
/// Applied to pre-versioning installations so every version 1 database looks
/// the same to [`migrate_1_to_2`]. Also used by tests to build a legacy file.
pub(crate) fn apply_v1_schema(conn: &Connection) -> Result<(), TrackerError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            github_kind TEXT,
            github_reference TEXT,
            github_state TEXT,
            github_checked_at TEXT,
            closed_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS subtasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE(task_id, name),
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS time_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            subtask_id INTEGER,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            note TEXT,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
            FOREIGN KEY(subtask_id) REFERENCES subtasks(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_time_entries_active
            ON time_entries(ended_at)
            WHERE ended_at IS NULL;

        CREATE INDEX IF NOT EXISTS idx_time_entries_started_at
            ON time_entries(started_at);
        ",
    )?;

    ensure_column(conn, "tasks", "github_state", "TEXT")?;
    ensure_column(conn, "tasks", "github_checked_at", "TEXT")?;
    ensure_column(conn, "tasks", "closed_at", "TEXT")?;

    Ok(())
}

/// The current schema, used when creating a database from scratch.
pub(crate) fn create_latest_schema(conn: &Connection) -> Result<(), TrackerError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            github_kind TEXT,
            github_reference TEXT,
            github_state TEXT,
            github_checked_at TEXT,
            closed_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS subtasks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            name_key TEXT NOT NULL UNIQUE,
            archived_at TEXT,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS time_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            subtask_id INTEGER,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            note TEXT,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
            FOREIGN KEY(subtask_id) REFERENCES subtasks(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_time_entries_active
            ON time_entries(ended_at)
            WHERE ended_at IS NULL;

        CREATE INDEX IF NOT EXISTS idx_time_entries_started_at
            ON time_entries(started_at);

        CREATE INDEX IF NOT EXISTS idx_time_entries_subtask_id
            ON time_entries(subtask_id);
        ",
    )?;

    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<(), TrackerError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;

    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }

    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}"),
        [],
    )?;
    Ok(())
}

/// Moves subtasks out from under tasks and into a shared list.
///
/// Subtask names that differ only in case or surrounding whitespace collapse
/// into a single row, keeping the earliest-created spelling. Time entries are
/// repointed at the surviving rows, so no recorded time is lost.
fn migrate_1_to_2(conn: &mut Connection) -> Result<(), TrackerError> {
    // Both tables are rebuilt, which means dropping tables other tables still
    // reference. Foreign keys are enforced again by the check below, and
    // `legacy_alter_table` keeps RENAME from rewriting foreign key clauses
    // while the old and new tables briefly coexist. Neither pragma takes
    // effect inside a transaction, so both are set up front.
    conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON;")?;
    let result = migrate_1_to_2_inner(conn);
    conn.execute_batch("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON;")?;
    result
}

fn migrate_1_to_2_inner(conn: &mut Connection) -> Result<(), TrackerError> {
    let tx = conn.transaction()?;

    tx.execute_batch(
        "
        CREATE TABLE subtasks_v2 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            name_key TEXT NOT NULL UNIQUE,
            archived_at TEXT,
            created_at TEXT NOT NULL
        );

        CREATE TABLE subtask_id_map_v2 (
            old_id INTEGER PRIMARY KEY,
            new_id INTEGER NOT NULL
        );
        ",
    )?;

    // Oldest first, so the earliest spelling of a name becomes the canonical one.
    let legacy_subtasks = {
        let mut stmt = tx.prepare(
            "
            SELECT id, name, created_at
            FROM subtasks
            ORDER BY created_at ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        items
    };

    let mut canonical_ids: HashMap<String, i64> = HashMap::new();
    for (old_id, name, created_at) in legacy_subtasks {
        let display_name = normalize_subtask_name(&name);
        let name_key = subtask_name_key(&display_name);

        // A blank legacy name carries no meaning once it is shared across
        // tasks. Leaving it out of the map unlinks its entries, which keeps
        // their recorded time while dropping the empty label.
        if name_key.is_empty() {
            continue;
        }

        let new_id = match canonical_ids.get(&name_key) {
            Some(id) => *id,
            None => {
                tx.execute(
                    "INSERT INTO subtasks_v2 (name, name_key, created_at) VALUES (?1, ?2, ?3)",
                    params![display_name, name_key, created_at],
                )?;
                let id = tx.last_insert_rowid();
                canonical_ids.insert(name_key, id);
                id
            }
        };

        tx.execute(
            "INSERT INTO subtask_id_map_v2 (old_id, new_id) VALUES (?1, ?2)",
            params![old_id, new_id],
        )?;
    }

    // Swap in the shared subtask table, then rebuild time_entries against it.
    // Remapping happens in one statement so every lookup sees the original
    // identifiers; assigning row by row would let a reused identifier move the
    // same entry twice.
    tx.execute_batch(
        "
        DROP TABLE subtasks;
        ALTER TABLE subtasks_v2 RENAME TO subtasks;

        CREATE TABLE time_entries_v2 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id INTEGER NOT NULL,
            subtask_id INTEGER,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            note TEXT,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
            FOREIGN KEY(subtask_id) REFERENCES subtasks(id) ON DELETE SET NULL
        );

        INSERT INTO time_entries_v2 (id, task_id, subtask_id, started_at, ended_at, note)
        SELECT e.id,
               e.task_id,
               (SELECT m.new_id FROM subtask_id_map_v2 m WHERE m.old_id = e.subtask_id),
               e.started_at,
               e.ended_at,
               e.note
        FROM time_entries e;

        DROP TABLE time_entries;
        ALTER TABLE time_entries_v2 RENAME TO time_entries;

        DROP TABLE subtask_id_map_v2;

        CREATE INDEX IF NOT EXISTS idx_time_entries_active
            ON time_entries(ended_at)
            WHERE ended_at IS NULL;

        CREATE INDEX IF NOT EXISTS idx_time_entries_started_at
            ON time_entries(started_at);

        CREATE INDEX IF NOT EXISTS idx_time_entries_subtask_id
            ON time_entries(subtask_id);
        ",
    )?;

    let broken_references: i64 =
        tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if broken_references > 0 {
        // Dropping out here rolls the transaction back, leaving the database
        // on version 1.
        return Err(TrackerError::MigrationIntegrity(broken_references));
    }

    tx.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION))?;
    tx.commit()?;

    Ok(())
}

/// Reads the schema version of an already-open database.
pub(crate) fn schema_version(conn: &Connection) -> Result<i64, TrackerError> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(TrackerError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a version 1 database with the given subtasks and time entries.
    fn legacy_database() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        apply_v1_schema(&conn).expect("apply version 1 schema");
        conn
    }

    fn insert_legacy_task(conn: &Connection, id: i64, name: &str) {
        conn.execute(
            "
            INSERT INTO tasks (id, name, created_at, updated_at)
            VALUES (?1, ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')
            ",
            params![id, name],
        )
        .expect("insert legacy task");
    }

    fn insert_legacy_subtask(
        conn: &Connection,
        id: i64,
        task_id: i64,
        name: &str,
        created_at: &str,
    ) {
        conn.execute(
            "INSERT INTO subtasks (id, task_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, task_id, name, created_at],
        )
        .expect("insert legacy subtask");
    }

    fn insert_legacy_entry(conn: &Connection, id: i64, task_id: i64, subtask_id: Option<i64>) {
        conn.execute(
            "
            INSERT INTO time_entries (id, task_id, subtask_id, started_at, ended_at)
            VALUES (?1, ?2, ?3, '2026-01-02T09:00:00Z', '2026-01-02T10:00:00Z')
            ",
            params![id, task_id, subtask_id],
        )
        .expect("insert legacy time entry");
    }

    fn subtask_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM subtasks ORDER BY id ASC")
            .expect("prepare subtask names");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query subtask names");
        rows.map(|row| row.expect("read subtask name")).collect()
    }

    fn entry_subtask_name(conn: &Connection, entry_id: i64) -> Option<String> {
        conn.query_row(
            "
            SELECT s.name
            FROM time_entries e
            LEFT JOIN subtasks s ON s.id = e.subtask_id
            WHERE e.id = ?1
            ",
            params![entry_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .expect("read entry subtask name")
    }

    #[test]
    fn fresh_database_is_created_at_the_current_version() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        assert_eq!(
            detect_version(&conn).expect("detect version"),
            EMPTY_VERSION
        );

        create_latest_schema(&conn).expect("create schema");
        set_version(&conn, SCHEMA_VERSION).expect("set version");

        assert_eq!(schema_version(&conn).expect("read version"), SCHEMA_VERSION);
    }

    #[test]
    fn pre_versioning_database_is_detected_as_version_one() {
        let conn = legacy_database();

        assert_eq!(
            detect_version(&conn).expect("detect version"),
            LEGACY_VERSION
        );
    }

    #[test]
    fn migration_merges_the_same_subtask_across_tasks() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_task(&conn, 2, "Second task");
        insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");
        insert_legacy_subtask(&conn, 2, 2, "Review", "2026-01-03T00:00:00Z");
        insert_legacy_entry(&conn, 1, 1, Some(1));
        insert_legacy_entry(&conn, 2, 2, Some(2));

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        assert_eq!(subtask_names(&conn), vec!["Review".to_owned()]);
        assert_eq!(entry_subtask_name(&conn, 1).as_deref(), Some("Review"));
        assert_eq!(entry_subtask_name(&conn, 2).as_deref(), Some("Review"));
        assert_eq!(schema_version(&conn).expect("read version"), SCHEMA_VERSION);
    }

    #[test]
    fn migration_merges_case_and_whitespace_variants_keeping_the_earliest_spelling() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_task(&conn, 2, "Second task");
        insert_legacy_task(&conn, 3, "Third task");
        insert_legacy_subtask(&conn, 1, 1, "Deploy", "2026-01-01T00:00:00Z");
        insert_legacy_subtask(&conn, 2, 2, "deploy", "2026-01-02T00:00:00Z");
        insert_legacy_subtask(&conn, 3, 3, "  Deploy  ", "2026-01-03T00:00:00Z");
        insert_legacy_entry(&conn, 1, 1, Some(1));
        insert_legacy_entry(&conn, 2, 2, Some(2));
        insert_legacy_entry(&conn, 3, 3, Some(3));

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        assert_eq!(subtask_names(&conn), vec!["Deploy".to_owned()]);
        for entry_id in 1..=3 {
            assert_eq!(
                entry_subtask_name(&conn, entry_id).as_deref(),
                Some("Deploy")
            );
        }
    }

    #[test]
    fn migration_keeps_distinct_subtasks_apart_and_remaps_every_entry() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_task(&conn, 2, "Second task");
        // Identifiers are ordered so that a row-by-row remap would corrupt the
        // result: the new id assigned to "Review" collides with an old id that
        // has not been remapped yet.
        insert_legacy_subtask(&conn, 5, 1, "Review", "2026-01-01T00:00:00Z");
        insert_legacy_subtask(&conn, 1, 1, "Deploy", "2026-01-02T00:00:00Z");
        insert_legacy_subtask(&conn, 3, 2, "Testing", "2026-01-03T00:00:00Z");
        insert_legacy_entry(&conn, 1, 1, Some(5));
        insert_legacy_entry(&conn, 2, 1, Some(1));
        insert_legacy_entry(&conn, 3, 2, Some(3));
        insert_legacy_entry(&conn, 4, 2, None);

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        assert_eq!(
            subtask_names(&conn),
            vec![
                "Review".to_owned(),
                "Deploy".to_owned(),
                "Testing".to_owned()
            ]
        );
        assert_eq!(entry_subtask_name(&conn, 1).as_deref(), Some("Review"));
        assert_eq!(entry_subtask_name(&conn, 2).as_deref(), Some("Deploy"));
        assert_eq!(entry_subtask_name(&conn, 3).as_deref(), Some("Testing"));
        assert_eq!(entry_subtask_name(&conn, 4), None);
    }

    #[test]
    fn migration_preserves_entry_identifiers_and_notes() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");
        conn.execute(
            "
            INSERT INTO time_entries (id, task_id, subtask_id, started_at, ended_at, note)
            VALUES (7, 1, 1, '2026-01-02T09:00:00Z', '2026-01-02T10:30:00Z', 'a note')
            ",
            [],
        )
        .expect("insert legacy entry");

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        let (task_id, started_at, ended_at, note): (i64, String, String, String) = conn
            .query_row(
                "SELECT task_id, started_at, ended_at, note FROM time_entries WHERE id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("read migrated entry");

        assert_eq!(task_id, 1);
        assert_eq!(started_at, "2026-01-02T09:00:00Z");
        assert_eq!(ended_at, "2026-01-02T10:30:00Z");
        assert_eq!(note, "a note");
    }

    #[test]
    fn migration_unlinks_blank_legacy_subtask_names_without_losing_entries() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_subtask(&conn, 1, 1, "   ", "2026-01-01T00:00:00Z");
        insert_legacy_entry(&conn, 1, 1, Some(1));

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        assert!(subtask_names(&conn).is_empty());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM time_entries", [], |row| row.get(0))
            .expect("count entries");
        assert_eq!(count, 1);
        assert_eq!(entry_subtask_name(&conn, 1), None);
    }

    #[test]
    fn migration_leaves_new_rows_with_fresh_identifiers() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");
        insert_legacy_entry(&conn, 9, 1, Some(1));

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        conn.execute(
            "INSERT INTO time_entries (task_id, started_at) VALUES (1, '2026-01-03T09:00:00Z')",
            [],
        )
        .expect("insert new entry");

        let id: i64 = conn
            .query_row("SELECT MAX(id) FROM time_entries", [], |row| row.get(0))
            .expect("read new entry id");
        assert_eq!(id, 10, "new rows should not reuse migrated identifiers");
    }

    #[test]
    fn migration_leaves_foreign_keys_enforced() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");
        insert_legacy_entry(&conn, 1, 1, Some(1));

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        let enforced: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read foreign_keys pragma");
        assert_eq!(enforced, 1);

        let error = conn.execute(
            "INSERT INTO time_entries (task_id, started_at) VALUES (99, '2026-01-03T09:00:00Z')",
            [],
        );
        assert!(error.is_err(), "unknown task should be rejected");
    }

    #[test]
    fn migrated_subtasks_reject_duplicate_names() {
        let mut conn = legacy_database();
        insert_legacy_task(&conn, 1, "First task");
        insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");

        run_migrations(&mut conn, LEGACY_VERSION).expect("run migrations");

        let error = conn.execute(
            "INSERT INTO subtasks (name, name_key, created_at) VALUES ('Review', 'review', '2026-01-04T00:00:00Z')",
            [],
        );
        assert!(error.is_err(), "duplicate name keys should be rejected");
    }

    #[test]
    fn database_from_a_newer_build_is_refused() {
        let dir = tempdir();
        let path = dir.join("tracker.sqlite3");
        {
            let conn = open(&path).expect("open database");
            create_latest_schema(&conn).expect("create schema");
            set_version(&conn, SCHEMA_VERSION + 1).expect("set version");
        }

        let prepared = prepare(&path);

        assert!(prepared.backup_path.is_none());
        assert!(matches!(
            prepared.result,
            Err(TrackerError::SchemaTooNew { .. })
        ));
    }

    #[test]
    fn prepare_creates_a_fresh_database_without_a_backup() {
        let dir = tempdir();
        let path = dir.join("tracker.sqlite3");

        let prepared = prepare(&path);

        assert!(prepared.result.is_ok());
        assert!(prepared.backup_path.is_none());

        let conn = open(&path).expect("open database");
        assert_eq!(schema_version(&conn).expect("read version"), SCHEMA_VERSION);
    }

    #[test]
    fn prepare_backs_up_and_migrates_an_existing_database() {
        let dir = tempdir();
        let path = dir.join("tracker.sqlite3");
        {
            let conn = open(&path).expect("open database");
            apply_v1_schema(&conn).expect("apply version 1 schema");
            insert_legacy_task(&conn, 1, "First task");
            insert_legacy_subtask(&conn, 1, 1, "Review", "2026-01-01T00:00:00Z");
            insert_legacy_entry(&conn, 1, 1, Some(1));
        }

        let prepared = prepare(&path);
        assert!(prepared.result.is_ok());

        let backup_path = prepared.backup_path.expect("backup path");
        assert!(backup_path.exists(), "backup file should exist");

        let backup = open(&backup_path).expect("open backup");
        assert_eq!(
            detect_version(&backup).expect("detect backup version"),
            LEGACY_VERSION,
            "the backup should still hold the pre-migration schema"
        );

        let conn = open(&path).expect("open database");
        assert_eq!(schema_version(&conn).expect("read version"), SCHEMA_VERSION);
        assert_eq!(entry_subtask_name(&conn, 1).as_deref(), Some("Review"));
    }

    #[test]
    fn prepare_is_a_no_op_once_the_database_is_current() {
        let dir = tempdir();
        let path = dir.join("tracker.sqlite3");

        assert!(prepare(&path).result.is_ok());
        let second = prepare(&path);

        assert!(second.result.is_ok());
        assert!(
            second.backup_path.is_none(),
            "an up to date database should not be copied"
        );
    }

    /// Creates a unique directory under the system temporary directory.
    ///
    /// The directory is left behind; these tests write a few kilobytes and the
    /// crate has no development dependency on a temporary file helper.
    fn tempdir() -> PathBuf {
        let name = format!(
            "tracker-db-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temporary directory");
        path
    }
}
