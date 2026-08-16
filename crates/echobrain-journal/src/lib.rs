//! The local write journal, on SQLite.
//!
//! The memory API has no list endpoint: discovery is semantic-only, so a memory
//! whose existence you have forgotten is unreachable. This table is the only
//! record of what *this device* wrote — see [`echobrain_core::JournalEntry`] for
//! why it is a cache and never an inventory.
//!
//! It replaces `InMemoryJournal`, which lost every capture on quit and so made
//! the library view empty at each launch. Nothing else changed: the semantics
//! here are the ones in `echobrain_core::testing`, and the contract test mirrors
//! that file case for case.
//!
//! # Why a synchronous driver behind an async port
//!
//! `rusqlite` is blocking, and the writes are microseconds against a local
//! file. Every call still goes through `spawn_blocking` rather than running on
//! the async executor: the journal is written from the capture path, which the
//! conversation loop shares with a realtime audio callback, and a stall there is
//! a dropped word.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{Connection, params};

use echobrain_core::{EchoError, IndexState, Journal, JournalEntry, MemoryId, MemoryKind};

/// Bumped when the schema changes, so a future migration can tell which shape
/// it is looking at. Stored in SQLite's own `user_version`, which costs nothing
/// and cannot drift out of sync with the file it describes.
const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS journal (
        memory_id    TEXT PRIMARY KEY,
        kind         TEXT NOT NULL,
        text         TEXT NOT NULL,
        created_at   INTEGER NOT NULL,
        expires_at   INTEGER,
        source       TEXT NOT NULL,
        state        TEXT NOT NULL,
        github_login TEXT,
        github_repo  TEXT,
        rating       INTEGER,
        active       INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX IF NOT EXISTS journal_by_state ON journal (state, created_at DESC);
";

/// A [`Journal`] that survives a quit.
#[derive(Debug)]
pub struct SqliteJournal {
    /// One connection behind a mutex rather than a pool. The whole workload is
    /// a handful of rows written by one desktop user, and a pool would be a
    /// second thing to reason about at no gain.
    connection: Arc<Mutex<Connection>>,
}

impl SqliteJournal {
    /// Open — creating it, and its parent directory, if absent.
    pub fn open(path: &Path) -> Result<Self, EchoError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(EchoError::journal)?;
        }
        Self::from_connection(Connection::open(path).map_err(EchoError::journal)?)
    }

    /// A journal that lives as long as the process.
    ///
    /// Two callers: the tests, and the app when the real file cannot be opened.
    /// The second is deliberate — a capture that succeeded remotely must not be
    /// lost to a permissions problem on a cache.
    pub fn in_memory() -> Result<Self, EchoError> {
        Self::from_connection(Connection::open_in_memory().map_err(EchoError::journal)?)
    }

    fn from_connection(connection: Connection) -> Result<Self, EchoError> {
        // WAL so a read never blocks the capture path's write. Ignored, not
        // fatal, for the in-memory database, which has no journal file to keep.
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
        connection
            .execute_batch(SCHEMA)
            .map_err(EchoError::journal)?;
        // Migrations for a file created by an earlier version, where `CREATE
        // TABLE IF NOT EXISTS` above is a no-op. Each is run on its own and its
        // error ignored, because "duplicate column name" is the expected result
        // on every launch after the first — and a batch would abandon the later
        // statements the moment the first one reported it.
        for migration in [
            "ALTER TABLE journal ADD COLUMN github_login TEXT",
            "ALTER TABLE journal ADD COLUMN github_repo TEXT",
            "ALTER TABLE journal ADD COLUMN rating INTEGER",
            // A row written before this column existed predates any way to
            // deactivate a memory, so it was active.
            "ALTER TABLE journal ADD COLUMN active INTEGER NOT NULL DEFAULT 1",
        ] {
            let _ = connection.execute(migration, []);
        }
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(EchoError::journal)?;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Run `f` off the async executor, with the connection locked.
    ///
    /// A poisoned lock is recovered rather than propagated, for the same reason
    /// `InMemoryJournal` degrades: the journal is a cache, and a panic here
    /// would surface as "your capture was lost" on a memory that is already
    /// stored remotely.
    async fn with_connection<T, F>(&self, f: F) -> Result<T, EchoError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);

        tokio::task::spawn_blocking(move || {
            let guard = match connection.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            f(&guard)
        })
        .await
        .map_err(EchoError::journal)?
        .map_err(EchoError::journal)
    }
}

#[async_trait]
impl Journal for SqliteJournal {
    async fn record(&self, entry: &JournalEntry) -> Result<(), EchoError> {
        let memory_id = entry.memory_id.as_str().to_string();
        let kind = entry.kind.as_str().to_string();
        let text = entry.text.clone();
        let created_at = to_millis(entry.created_at);
        let expires_at = entry.expires_at.map(to_millis);
        let source = entry.source.clone();
        let state = entry.state.as_str().to_string();
        let github_login = entry.github_login.clone();
        let github_repo = entry.github_repo.clone();
        let rating = entry.rating;
        let active = entry.active;

        self.with_connection(move |connection| {
            // The id is the server's, so a conflict means the same memory was
            // recorded twice. `state` is deliberately left out of the update:
            // re-recording must not resurrect a tombstone, exactly as
            // `mark_indexed` must not.
            connection.execute(
                "INSERT INTO journal
                     (memory_id, kind, text, created_at, expires_at, source, state, github_login, github_repo, rating, active)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(memory_id) DO UPDATE SET
                     kind         = excluded.kind,
                     text         = excluded.text,
                     created_at   = excluded.created_at,
                     expires_at   = excluded.expires_at,
                     source       = excluded.source,
                     github_login = excluded.github_login,
                     github_repo  = excluded.github_repo,
                     rating       = excluded.rating,
                     active       = excluded.active",
                params![memory_id, kind, text, created_at, expires_at, source, state, github_login, github_repo, rating, active],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<JournalEntry>, EchoError> {
        self.with_connection(move |connection| {
            let mut statement = connection.prepare(
                "SELECT memory_id, kind, text, created_at, expires_at, source, state, github_login, github_repo, rating, active
                   FROM journal
                  WHERE state != 'forgotten'
                  ORDER BY created_at DESC
                  LIMIT ?1",
            )?;
            let rows = statement.query_map([limit as i64], row_to_entry)?;
            Ok(collect(rows))
        })
        .await
    }

    async fn awaiting_index(&self) -> Result<Vec<JournalEntry>, EchoError> {
        self.with_connection(move |connection| {
            let mut statement = connection.prepare(
                "SELECT memory_id, kind, text, created_at, expires_at, source, state, github_login, github_repo, rating, active
                   FROM journal
                  WHERE state = 'indexing'
                  ORDER BY created_at DESC",
            )?;
            let rows = statement.query_map([], row_to_entry)?;
            Ok(collect(rows))
        })
        .await
    }

    async fn mark_indexed(&self, memory_id: &MemoryId) -> Result<(), EchoError> {
        let memory_id = memory_id.as_str().to_string();

        self.with_connection(move |connection| {
            // `AND state = 'indexing'` is the whole rule: a tombstoned row stays
            // tombstoned, because the memory being visible in the index says
            // nothing about the user having un-deleted it. Idempotent by
            // construction — a second call updates nothing.
            connection.execute(
                "UPDATE journal SET state = 'indexed'
                  WHERE memory_id = ?1 AND state = 'indexing'",
                [memory_id],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    async fn tombstone(&self, memory_id: &MemoryId) -> Result<(), EchoError> {
        let memory_id = memory_id.as_str().to_string();

        self.with_connection(move |connection| {
            // Zero rows updated is not an error: the journal is a cache, and the
            // memory may have been written by another client of this namespace.
            connection.execute(
                "UPDATE journal SET state = 'forgotten' WHERE memory_id = ?1",
                [memory_id],
            )?;
            Ok(())
        })
        .await
        .map(|_| ())
    }
}

/// Rows that fail to parse are dropped with a warning rather than failing the
/// whole query.
///
/// The only writer of this table is the file you are reading, so an unparseable
/// row means real corruption — and a corrupt row taking the entire library view
/// down with it would turn a lost capture into a broken screen.
fn collect<I>(rows: I) -> Vec<JournalEntry>
where
    I: Iterator<Item = Result<Option<JournalEntry>, rusqlite::Error>>,
{
    rows.filter_map(|row| match row {
        Ok(entry) => entry,
        Err(error) => {
            tracing::warn!(%error, "a journal row could not be read");
            None
        }
    })
    .collect()
}

/// `Ok(None)` for a row whose stored values no longer parse — see [`collect`].
fn row_to_entry(row: &rusqlite::Row<'_>) -> Result<Option<JournalEntry>, rusqlite::Error> {
    let memory_id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let text: String = row.get(2)?;
    let created_at: i64 = row.get(3)?;
    let expires_at: Option<i64> = row.get(4)?;
    let source: String = row.get(5)?;
    let state: String = row.get(6)?;
    let github_login: Option<String> = row.get(7)?;
    let github_repo: Option<String> = row.get(8)?;
    // Read wide and narrow on the way out: the column is whatever an older
    // build or a hand-edited file put there, and a rating outside 1..=5 is
    // meaningless rather than fatal — see `rating`.
    let rating: Option<i64> = row.get(9)?;
    let active: bool = row.get(10)?;

    let (Ok(memory_id), Ok(kind), Some(state)) = (
        MemoryId::new(memory_id),
        kind.parse::<MemoryKind>(),
        index_state(&state),
    ) else {
        return Ok(None);
    };

    Ok(Some(JournalEntry {
        memory_id,
        kind,
        text,
        created_at: from_millis(created_at),
        expires_at: expires_at.map(from_millis),
        source,
        github_login,
        github_repo,
        rating: rating.and_then(rating_in_range),
        active,
        state,
    }))
}

/// A rating the UI cannot have produced is dropped, not clamped.
///
/// Clamping a stored `9` to `5` would invent a judgement the user never made
/// and then show it back to them as their own. `None` is the honest answer:
/// unrated, and rateable again.
fn rating_in_range(value: i64) -> Option<u8> {
    (1..=5).contains(&value).then_some(value as u8)
}

/// The inverse of [`IndexState::as_str`], which is what writes the column.
fn index_state(value: &str) -> Option<IndexState> {
    match value {
        "indexing" => Some(IndexState::Indexing),
        "indexed" => Some(IndexState::Indexed),
        "forgotten" => Some(IndexState::Forgotten),
        _ => None,
    }
}

/// Milliseconds since the epoch.
///
/// Milliseconds rather than seconds because `recent` orders by this column and
/// two captures in the same second are ordinary — say something, correct
/// yourself, say it again.
fn to_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        // A clock before 1970 is a broken clock, not a memory from the past.
        .unwrap_or(0)
}

fn from_millis(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_round_trips_to_the_millisecond() {
        // The library view orders by this and shows it as a date, so a
        // conversion that loses precision would reorder two captures made in
        // the same second.
        let time = UNIX_EPOCH + Duration::from_millis(1_755_000_123_456);
        assert_eq!(from_millis(to_millis(time)), time);
    }

    #[test]
    fn a_clock_before_the_epoch_becomes_the_epoch_rather_than_a_panic() {
        assert_eq!(to_millis(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert_eq!(from_millis(-1), UNIX_EPOCH);
    }

    #[test]
    fn a_rating_outside_one_to_five_reads_back_as_unrated() {
        // Not reachable through `record`, which only ever writes what the UI
        // produced — but reachable through an older build or a hand-edited
        // file, and clamping a stored 9 to 5 would show the user a judgement
        // they never made as their own.
        assert_eq!(rating_in_range(0), None);
        assert_eq!(rating_in_range(6), None);
        assert_eq!(rating_in_range(-1), None);
        for value in 1..=5 {
            assert_eq!(rating_in_range(value), Some(value as u8));
        }
    }

    #[test]
    fn every_index_state_survives_the_round_trip_through_the_column() {
        for state in [
            IndexState::Indexing,
            IndexState::Indexed,
            IndexState::Forgotten,
        ] {
            assert_eq!(index_state(state.as_str()), Some(state));
        }
        assert_eq!(index_state("indexed-ish"), None);
    }
}
