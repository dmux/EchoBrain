//! Turning something said into a stored memory.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use agent_memory_core::{MemoryKind, MemoryService, RememberCommand};

use crate::error::EchoError;
use crate::model::{IndexState, JournalEntry, caller_placeholder};
use crate::ports::{Journal, KindClassifier};

/// What the caller asks to be remembered.
#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub text: String,
    /// Set when the user corrected the classifier's guess in the UI. `None`
    /// means "you decide".
    pub kind: Option<MemoryKind>,
    /// Enforced by DynamoDB TTL, which also removes the vector index entry.
    pub ttl: Option<Duration>,
    pub github_repo: Option<String>,
}

/// The outcome of a capture.
#[derive(Debug, Clone)]
pub struct Ingested {
    pub entry: JournalEntry,
    /// The memory is durably stored whatever this says — it reports only that
    /// the *local* journal write failed, so the capture will be missing from
    /// the library on this device.
    ///
    /// It is deliberately not an error. Failing the whole operation here would
    /// tell the user their capture was lost and invite them to repeat
    /// themselves, which would pay for a second embedding and store a duplicate
    /// of something already saved.
    pub journal_failed: Option<String>,
}

/// Ports are type-erased rather than generic for the same reason the sibling
/// repository's MCP server erases them: the driving adapter cannot be generic.
/// Tauri commands resolve their state by concrete type, so a
/// `IngestUtterance<M, K, J>` would have to have its parameters spelled out at
/// every call site and could not be swapped for a fake at runtime.
pub struct IngestUtterance {
    memory: Arc<dyn MemoryService>,
    classifier: Arc<dyn KindClassifier>,
    journal: Arc<dyn Journal>,
    /// Provenance written to every memory, e.g. `"echobrain-macos"`. Lets a
    /// later audit tell dictated captures from ones an agent wrote over MCP.
    source: String,
}

impl fmt::Debug for IngestUtterance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IngestUtterance")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl IngestUtterance {
    pub fn new(
        memory: Arc<dyn MemoryService>,
        classifier: Arc<dyn KindClassifier>,
        journal: Arc<dyn Journal>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            memory,
            classifier,
            journal,
            source: source.into(),
        }
    }

    pub async fn execute(&self, request: IngestRequest) -> Result<Ingested, EchoError> {
        let text = request.text.trim();
        if text.is_empty() {
            // Pressing the hotkey and saying nothing. Rejected before the
            // network call, so silence never costs an embedding.
            return Err(EchoError::EmptyUtterance);
        }

        let kind = request
            .kind
            .unwrap_or_else(|| self.classifier.classify(text));

        let memory = self
            .memory
            .remember(RememberCommand {
                // Ignored by the server, which derives the namespace from the
                // verified SigV4 principal. See `caller_placeholder`.
                user_id: caller_placeholder()?,
                kind,
                text: text.to_string(),
                ttl: request.ttl,
                source: Some(self.source.clone()),
                github_login: None,
                github_repo: request.github_repo,
                rating: None,
                active: true,
            })
            .await?;

        let entry = JournalEntry {
            memory_id: memory.memory_id,
            kind: memory.kind,
            text: memory.text,
            created_at: memory.created_at,
            expires_at: memory.expires_at,
            source: memory.source.unwrap_or_else(|| self.source.clone()),
            github_login: memory.github_login,
            github_repo: memory.github_repo,
            rating: memory.rating,
            active: memory.active,
            // Written, but the vector index has not caught up yet and will not
            // for up to ~90 seconds.
            state: IndexState::Indexing,
        };

        let journal_failed = match self.journal.record(&entry).await {
            Ok(()) => None,
            Err(error) => Some(error.to_string()),
        };

        Ok(Ingested {
            entry,
            journal_failed,
        })
    }
}
