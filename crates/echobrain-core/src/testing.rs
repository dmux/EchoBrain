//! In-memory port implementations, for tests and for the phase-0 app.
//!
//! Mirrors `agent_memory_core::testing`: an implementation that needs nothing
//! but `std` is not infrastructure, so it belongs beside the ports rather than
//! in an adapter crate. Feature-gated so a release build cannot pick one up by
//! accident.

use std::sync::Mutex;

use async_trait::async_trait;

use agent_memory_core::{MemoryId, MemoryKind};

use crate::error::EchoError;
use crate::model::{IndexState, JournalEntry, Samples, Transcript};
use crate::ports::{Journal, KindClassifier, Transcriber};

/// A [`Journal`] that lives as long as the process.
///
/// Useful in tests, and honest as the phase-0 stand-in: it has exactly the
/// semantics of the SQLite adapter minus durability, so swapping it out later
/// changes one line in the composition root and nothing else.
#[derive(Debug, Default)]
pub struct InMemoryJournal {
    entries: Mutex<Vec<JournalEntry>>,
}

impl InMemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rows().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn rows(&self) -> Vec<JournalEntry> {
        match self.entries.lock() {
            Ok(guard) => guard.clone(),
            // A poisoned lock means another thread panicked mid-write. The
            // journal is a cache, so degrading to "empty" beats propagating a
            // panic into a capture that already succeeded remotely.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn with_rows<T>(&self, f: impl FnOnce(&mut Vec<JournalEntry>) -> T) -> T {
        let mut guard = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }
}

#[async_trait]
impl Journal for InMemoryJournal {
    async fn record(&self, entry: &JournalEntry) -> Result<(), EchoError> {
        self.with_rows(|rows| rows.push(entry.clone()));
        Ok(())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<JournalEntry>, EchoError> {
        let mut rows = self.rows();
        rows.retain(|row| row.state != IndexState::Forgotten);
        rows.sort_by_key(|row| std::cmp::Reverse(row.created_at));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn awaiting_index(&self) -> Result<Vec<JournalEntry>, EchoError> {
        let mut rows = self.rows();
        rows.retain(|row| row.state == IndexState::Indexing);
        Ok(rows)
    }

    async fn mark_indexed(&self, memory_id: &MemoryId) -> Result<(), EchoError> {
        self.with_rows(|rows| {
            for row in rows.iter_mut() {
                // A tombstoned row stays tombstoned: the memory being visible
                // in the index says nothing about the user having un-deleted it.
                if &row.memory_id == memory_id && row.state == IndexState::Indexing {
                    row.state = IndexState::Indexed;
                }
            }
        });
        Ok(())
    }

    async fn tombstone(&self, memory_id: &MemoryId) -> Result<(), EchoError> {
        self.with_rows(|rows| {
            for row in rows.iter_mut() {
                if &row.memory_id == memory_id {
                    row.state = IndexState::Forgotten;
                }
            }
        });
        Ok(())
    }
}

/// A [`Journal`] that fails every write, to exercise the path where a capture
/// succeeds remotely but cannot be recorded locally.
#[derive(Debug, Default)]
pub struct FailingJournal;

#[async_trait]
impl Journal for FailingJournal {
    async fn record(&self, _entry: &JournalEntry) -> Result<(), EchoError> {
        Err(EchoError::journal("disk on fire"))
    }

    async fn recent(&self, _limit: usize) -> Result<Vec<JournalEntry>, EchoError> {
        Err(EchoError::journal("disk on fire"))
    }

    async fn awaiting_index(&self) -> Result<Vec<JournalEntry>, EchoError> {
        Err(EchoError::journal("disk on fire"))
    }

    async fn mark_indexed(&self, _memory_id: &MemoryId) -> Result<(), EchoError> {
        Err(EchoError::journal("disk on fire"))
    }

    async fn tombstone(&self, _memory_id: &MemoryId) -> Result<(), EchoError> {
        Err(EchoError::journal("disk on fire"))
    }
}

/// A [`KindClassifier`] that always answers the same thing.
#[derive(Debug, Clone, Copy)]
pub struct FixedClassifier(pub MemoryKind);

impl KindClassifier for FixedClassifier {
    fn classify(&self, _text: &str) -> MemoryKind {
        self.0
    }
}

/// A [`Transcriber`] that returns canned text, so a capture flow can be tested
/// without audio hardware or a model file.
#[derive(Debug, Clone)]
pub struct CannedTranscriber {
    pub text: String,
}

impl CannedTranscriber {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[async_trait]
impl Transcriber for CannedTranscriber {
    async fn transcribe(&self, _samples: &Samples) -> Result<Transcript, EchoError> {
        Ok(Transcript {
            text: self.text.clone(),
            language: Some("pt-BR".to_string()),
        })
    }
}
