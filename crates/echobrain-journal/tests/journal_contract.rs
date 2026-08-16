//! The [`Journal`] contract, exercised against the SQLite adapter.
//!
//! `InMemoryJournal` in `echobrain_core::testing` is what every use-case test
//! runs against, so these two implementations disagreeing would mean the suite
//! is green about behaviour the app does not have. Each case here is a rule the
//! fake already obeys, restated against a real database file.
//!
//! They run on a temporary file rather than `:memory:` wherever persistence is
//! the thing under test — an in-memory database cannot fail the one test that
//! `InMemoryJournal` failed, which is surviving a quit.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use echobrain_core::{IndexState, Journal, JournalEntry, MemoryId, MemoryKind};
use echobrain_journal::SqliteJournal;

/// A path in the OS temporary directory, unique per test, removed on drop.
///
/// No `tempfile` dependency for five tests: the whole need is a name nothing
/// else uses and a cleanup that runs on failure too.
struct TempDatabase(std::path::PathBuf);

impl TempDatabase {
    fn new(name: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_nanos();
        Self(std::env::temp_dir().join(format!("echobrain-{name}-{unique}.sqlite3")))
    }

    fn open(&self) -> SqliteJournal {
        SqliteJournal::open(&self.0).expect("the journal opens")
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        // WAL leaves two siblings behind. Best-effort: a leftover file in the
        // temporary directory must never fail a passing test.
        let _ = std::fs::remove_file(&self.0);
        for suffix in ["-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

fn entry(id: &str, seconds: u64, state: IndexState) -> JournalEntry {
    JournalEntry {
        memory_id: MemoryId::new(id).expect("a valid id"),
        kind: MemoryKind::Fact,
        text: format!("memória {id}"),
        created_at: UNIX_EPOCH + Duration::from_secs(seconds),
        expires_at: None,
        source: "echobrain-macos".to_string(),
        github_login: None,
        github_repo: None,
        rating: None,
        active: true,
        state,
    }
}

#[tokio::test]
async fn what_was_recorded_is_still_there_after_a_reopen() {
    // The reason this crate exists. `InMemoryJournal` passes every other case
    // in this file and fails this one, which is what an empty library view at
    // every launch looked like.
    let database = TempDatabase::new("reopen");

    database
        .open()
        .record(&entry("mem-1", 1_700_000_000, IndexState::Indexing))
        .await
        .expect("recorded");

    let reopened = database.open().recent(10).await.expect("read back");

    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].memory_id.as_str(), "mem-1");
    assert_eq!(reopened[0].state, IndexState::Indexing);
}

#[tokio::test]
async fn a_row_survives_the_round_trip_field_for_field() {
    let database = TempDatabase::new("roundtrip");
    let mut written = entry("mem-full", 1_700_000_001, IndexState::Indexed);
    written.expires_at = Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    written.kind = MemoryKind::Preference;
    written.github_login = Some("rafaelsales".to_string());
    written.github_repo = Some("dmux/EchoBrain".to_string());
    written.rating = Some(4);
    written.active = false;

    let journal = database.open();
    journal.record(&written).await.expect("recorded");
    let read_back = journal.recent(10).await.expect("read back");

    // Field for field rather than field by field: a column added to the struct
    // and forgotten in the INSERT is exactly the bug this catches.
    assert_eq!(read_back, vec![written]);
}

#[tokio::test]
async fn recent_hides_tombstones_and_returns_the_newest_first() {
    let database = TempDatabase::new("recent");
    let journal = database.open();

    for (id, seconds) in [("mem-old", 1_000), ("mem-new", 2_000), ("mem-mid", 1_500)] {
        journal
            .record(&entry(id, seconds, IndexState::Indexed))
            .await
            .expect("recorded");
    }
    journal
        .tombstone(&MemoryId::new("mem-mid").expect("a valid id"))
        .await
        .expect("tombstoned");

    let ids: Vec<_> = journal
        .recent(10)
        .await
        .expect("read back")
        .into_iter()
        .map(|row| row.memory_id.into_inner())
        .collect();

    assert_eq!(ids, vec!["mem-new".to_string(), "mem-old".to_string()]);
}

#[tokio::test]
async fn recent_honours_the_limit() {
    let database = TempDatabase::new("limit");
    let journal = database.open();

    for seconds in 0..5u64 {
        journal
            .record(&entry(
                &format!("mem-{seconds}"),
                1_000 + seconds,
                IndexState::Indexed,
            ))
            .await
            .expect("recorded");
    }

    assert_eq!(journal.recent(2).await.expect("read back").len(), 2);
}

#[tokio::test]
async fn awaiting_index_returns_only_what_is_still_indexing() {
    let database = TempDatabase::new("awaiting");
    let journal = database.open();

    journal
        .record(&entry("mem-waiting", 1_000, IndexState::Indexing))
        .await
        .expect("recorded");
    journal
        .record(&entry("mem-done", 1_001, IndexState::Indexed))
        .await
        .expect("recorded");
    journal
        .record(&entry("mem-gone", 1_002, IndexState::Forgotten))
        .await
        .expect("recorded");

    let waiting = journal.awaiting_index().await.expect("read back");

    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].memory_id.as_str(), "mem-waiting");
}

#[tokio::test]
async fn marking_indexed_is_idempotent_and_never_resurrects_a_tombstone() {
    let database = TempDatabase::new("mark");
    let journal = database.open();
    let waiting = MemoryId::new("mem-waiting").expect("a valid id");
    let gone = MemoryId::new("mem-gone").expect("a valid id");

    journal
        .record(&entry("mem-waiting", 1_000, IndexState::Indexing))
        .await
        .expect("recorded");
    journal
        .record(&entry("mem-gone", 1_001, IndexState::Forgotten))
        .await
        .expect("recorded");

    journal.mark_indexed(&waiting).await.expect("marked");
    journal.mark_indexed(&waiting).await.expect("marked twice");
    // The memory reappearing in the index says nothing about the user having
    // un-deleted it.
    journal.mark_indexed(&gone).await.expect("no-op");

    let states: Vec<_> = journal
        .awaiting_index()
        .await
        .expect("read back")
        .into_iter()
        .collect();
    assert!(states.is_empty(), "nothing is left waiting");

    let visible = journal.recent(10).await.expect("read back");
    assert_eq!(visible.len(), 1, "the tombstone stays hidden");
    assert_eq!(visible[0].state, IndexState::Indexed);
}

#[tokio::test]
async fn recording_the_same_id_twice_updates_it_without_resurrecting_it() {
    let database = TempDatabase::new("conflict");
    let journal = database.open();
    let id = MemoryId::new("mem-1").expect("a valid id");

    journal
        .record(&entry("mem-1", 1_000, IndexState::Indexing))
        .await
        .expect("recorded");
    journal.tombstone(&id).await.expect("tombstoned");

    // The server assigned the id, so a second record of it is the same memory
    // arriving again — not a reason to un-delete it.
    let mut again = entry("mem-1", 1_000, IndexState::Indexing);
    again.text = "texto corrigido".to_string();
    journal.record(&again).await.expect("re-recorded");

    assert!(
        journal.recent(10).await.expect("read back").is_empty(),
        "the tombstone survives a re-record"
    );
}

#[tokio::test]
async fn tombstoning_something_this_device_never_wrote_is_not_an_error() {
    // The journal is a cache: the memory may have been written by the MCP
    // server or by an agent through the Lambda, and forgetting it remotely
    // still has to succeed here.
    let database = TempDatabase::new("unknown");

    database
        .open()
        .tombstone(&MemoryId::new("mem-elsewhere").expect("a valid id"))
        .await
        .expect("tombstoning an absent row succeeds");
}
