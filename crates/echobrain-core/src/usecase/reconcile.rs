//! Closing the gap between "written" and "searchable".

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use agent_memory_core::{MemoryId, MemoryService};

use crate::Journal;
use crate::error::EchoError;
use crate::model::caller_placeholder;

/// How long a memory may sit in `Indexing` before its absence is worth a call.
///
/// The sibling project documents 90 seconds as the ceiling for the vector index
/// to catch up. Both observed runs indexed in under a second, so this is a
/// bound and not an expectation — which is exactly why the probe waits for it
/// rather than polling from the start: below this, "not found yet" is the
/// normal answer and paying for it says nothing.
pub const INDEXING_BUDGET: Duration = Duration::from_secs(90);

/// What one pass of [`Reconciler::probe_overdue`] did.
///
/// Returned rather than logged because this crate has no logger and is not
/// getting one — see the dependency list in `Cargo.toml`. The adapter that
/// drives the timer is the layer that knows how this machine reports things.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Confirmed present in the index and settled in the journal.
    pub promoted: usize,
    /// Stored, past the budget, and still not in the index. Each of these was
    /// asked about exactly once and is worth a line in the log.
    pub still_missing: Vec<MemoryId>,
    /// Probes that failed outright. Says nothing about the memories themselves,
    /// and each will be retried on the next pass.
    pub failed: usize,
}

impl Reconciled {
    /// Whether this pass found anything worth mentioning.
    pub fn is_quiet(&self) -> bool {
        self.promoted == 0 && self.still_missing.is_empty() && self.failed == 0
    }
}

/// Promotes journal rows from `Indexing` to `Indexed`.
///
/// # Why this exists at all
///
/// A capture is stored remotely the moment `remember` returns, but the vector
/// index behind recall is eventually consistent — so for a short window the
/// memory is saved and unfindable. The journal records that window as
/// `Indexing`, and something has to end it, or the library view labels a
/// perfectly searchable memory "indexando" until the file is deleted.
///
/// # Why it mostly does not call anything
///
/// The cheap signal is the one the user generates anyway: a memory that comes
/// back from a search is, by definition, in the index. [`Self::observed`] takes
/// the hits every search already produced and settles them for free.
/// [`Self::probe_overdue`] is the fallback for the memory nobody happens to
/// search for, and it is deliberately the second-choice path — it costs a
/// request per memory and answers a question a future search would have
/// answered for nothing.
#[derive(Clone)]
pub struct Reconciler {
    journal: Arc<dyn Journal>,
    memory: Arc<dyn MemoryService>,
    /// Ids already probed, so a memory the API does not have is asked about
    /// once rather than every 30 seconds for as long as the app is open.
    ///
    /// In memory rather than in the journal: forgetting on restart is the
    /// correct behaviour, since a relaunch is also the cheapest moment to
    /// discover the index caught up while the app was closed.
    probed: Arc<Mutex<HashSet<MemoryId>>>,
}

impl std::fmt::Debug for Reconciler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reconciler").finish_non_exhaustive()
    }
}

impl Reconciler {
    pub fn new(journal: Arc<dyn Journal>, memory: Arc<dyn MemoryService>) -> Self {
        Self {
            journal,
            memory,
            probed: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Settle every id that just came back from a search.
    ///
    /// Infallible by construction: this runs on the search path, and a journal
    /// write failing is not a reason to fail the search the user asked for. The
    /// cost of dropping it is one row that stays labelled `indexing` until the
    /// next search returns it again.
    pub async fn observed(&self, memory_ids: &[MemoryId]) {
        for memory_id in memory_ids {
            let _ = self.journal.mark_indexed(memory_id).await;
        }
    }

    /// Ask the API about memories that have been `Indexing` past the budget.
    pub async fn probe_overdue(&self) -> Result<Reconciled, EchoError> {
        let mut report = Reconciled::default();

        let awaiting = self.journal.awaiting_index().await?;
        if awaiting.is_empty() {
            // The common case, and the reason the caller can run this on a
            // timer: no rows waiting means no user, no network and no cost.
            return Ok(report);
        }

        let now = SystemTime::now();
        let user_id = caller_placeholder()?;

        for entry in awaiting {
            // A `created_at` in the future is a clock that moved, not a memory
            // from the future — treated as brand new, so it waits its turn.
            let age = now
                .duration_since(entry.created_at)
                .unwrap_or(Duration::ZERO);
            if age < INDEXING_BUDGET {
                continue;
            }

            if !self.claim(&entry.memory_id) {
                continue;
            }

            match self.memory.get(&user_id, &entry.memory_id).await {
                Ok(Some(_)) => {
                    // A failure here leaves the row waiting for the next pass,
                    // which is the same place a failed probe leaves it.
                    if self.journal.mark_indexed(&entry.memory_id).await.is_ok() {
                        report.promoted += 1;
                    }
                }
                // Stored, past its budget, and still not there. Reported once —
                // but the row stays `Indexing` rather than being tombstoned:
                // the journal is a cache, and deleting the user's only local
                // record of a capture on the strength of one lookup would
                // destroy more than it fixes.
                Ok(None) => report.still_missing.push(entry.memory_id),
                // A failed request says nothing about the memory, so the claim
                // is released and the next pass tries again.
                Err(_) => {
                    report.failed += 1;
                    self.release(&entry.memory_id);
                }
            }
        }

        Ok(report)
    }

    /// `true` if this call is the first to take responsibility for probing `id`.
    fn claim(&self, memory_id: &MemoryId) -> bool {
        self.with_probed(|probed| probed.insert(memory_id.clone()))
    }

    fn release(&self, memory_id: &MemoryId) {
        self.with_probed(|probed| probed.remove(memory_id));
    }

    /// A poisoned lock is recovered rather than propagated, for the same reason
    /// the journal degrades instead of failing: this set is an optimisation
    /// against duplicate requests, and losing it costs a redundant lookup —
    /// while panicking here would take down a background task that is only
    /// tidying up.
    fn with_probed<T>(&self, f: impl FnOnce(&mut HashSet<MemoryId>) -> T) -> T {
        let mut guard = match self.probed.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        f(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent_memory_core::{
        Memory, MemoryError, MemoryKind, RecallQuery, RememberCommand, ScoredMemory, UpdateCommand,
        UserId,
    };
    use async_trait::async_trait;

    use crate::model::{IndexState, JournalEntry};
    use crate::testing::InMemoryJournal;

    /// A [`MemoryService`] that answers `get` however the test says, and counts
    /// how many times it was asked.
    ///
    /// The count is the point: "probed once" is the rule that keeps a memory the
    /// index never gets from generating a request every thirty seconds for as
    /// long as the app is open.
    struct ProbeSpy {
        answer: Result<bool, ()>,
        gets: AtomicUsize,
    }

    impl ProbeSpy {
        fn found() -> Arc<Self> {
            Arc::new(Self {
                answer: Ok(true),
                gets: AtomicUsize::new(0),
            })
        }

        fn missing() -> Arc<Self> {
            Arc::new(Self {
                answer: Ok(false),
                gets: AtomicUsize::new(0),
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                answer: Err(()),
                gets: AtomicUsize::new(0),
            })
        }

        fn gets(&self) -> usize {
            self.gets.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MemoryService for ProbeSpy {
        async fn get(
            &self,
            _user_id: &UserId,
            memory_id: &MemoryId,
        ) -> Result<Option<Memory>, MemoryError> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            match self.answer {
                Ok(true) => Ok(Some(Memory {
                    user_id: caller_placeholder().expect("a valid placeholder"),
                    memory_id: memory_id.clone(),
                    text: "qualquer coisa".to_string(),
                    kind: MemoryKind::Fact,
                    created_at: SystemTime::UNIX_EPOCH,
                    expires_at: None,
                    source: None,
                    github_login: None,
                    github_repo: None,
                    rating: None,
                    active: true,
                })),
                Ok(false) => Ok(None),
                Err(()) => Err(MemoryError::repository(Box::new(std::io::Error::other(
                    "a rede caiu",
                )))),
            }
        }

        async fn remember(&self, _command: RememberCommand) -> Result<Memory, MemoryError> {
            unreachable!("the reconciler never writes")
        }

        async fn recall(&self, _query: RecallQuery) -> Result<Vec<ScoredMemory>, MemoryError> {
            unreachable!("the reconciler never searches")
        }

        async fn forget(
            &self,
            _user_id: &UserId,
            _memory_id: &MemoryId,
        ) -> Result<(), MemoryError> {
            unreachable!("the reconciler never deletes")
        }

        async fn list(
            &self,
            _user_id: &UserId,
            _query: &agent_memory_core::ListQuery,
        ) -> Result<agent_memory_core::MemoryPage, MemoryError> {
            unreachable!("the reconciler reads the journal, not the API")
        }

        async fn update(&self, _command: UpdateCommand) -> Result<Memory, MemoryError> {
            unreachable!("the reconciler never edits")
        }
    }

    fn waiting_since(age: Duration) -> JournalEntry {
        JournalEntry {
            memory_id: MemoryId::new("mem-1").expect("a valid id"),
            kind: MemoryKind::Fact,
            text: "prefiro arquitetura hexagonal".to_string(),
            created_at: SystemTime::now() - age,
            expires_at: None,
            source: "echobrain-macos".to_string(),
            github_login: None,
            github_repo: None,
            rating: None,
            active: true,
            state: IndexState::Indexing,
        }
    }

    async fn journal_with(entry: &JournalEntry) -> Arc<InMemoryJournal> {
        let journal = Arc::new(InMemoryJournal::new());
        journal.record(entry).await.expect("recorded");
        journal
    }

    async fn state_of(journal: &Arc<InMemoryJournal>) -> IndexState {
        journal.recent(10).await.expect("read back")[0].state
    }

    #[tokio::test]
    async fn a_memory_that_came_back_from_a_search_is_settled_without_a_request() {
        let entry = waiting_since(Duration::ZERO);
        let journal = journal_with(&entry).await;
        let api = ProbeSpy::missing();
        let reconciler = Reconciler::new(journal.clone(), api.clone());

        reconciler
            .observed(std::slice::from_ref(&entry.memory_id))
            .await;

        assert_eq!(state_of(&journal).await, IndexState::Indexed);
        assert_eq!(api.gets(), 0, "the search already proved it is indexed");
    }

    #[tokio::test]
    async fn nothing_is_probed_before_the_consistency_budget_has_passed() {
        // Below the budget "not found" is the ordinary answer, so asking buys
        // nothing and still costs a request.
        let journal = journal_with(&waiting_since(INDEXING_BUDGET / 2)).await;
        let api = ProbeSpy::found();
        let reconciler = Reconciler::new(journal.clone(), api.clone());

        let _ = reconciler.probe_overdue().await.expect("probed");

        assert_eq!(api.gets(), 0);
        assert_eq!(state_of(&journal).await, IndexState::Indexing);
    }

    #[tokio::test]
    async fn an_overdue_memory_the_api_has_is_promoted() {
        let journal = journal_with(&waiting_since(INDEXING_BUDGET * 2)).await;
        let api = ProbeSpy::found();
        let reconciler = Reconciler::new(journal.clone(), api.clone());

        let _ = reconciler.probe_overdue().await.expect("probed");

        assert_eq!(api.gets(), 1);
        assert_eq!(state_of(&journal).await, IndexState::Indexed);
    }

    #[tokio::test]
    async fn a_memory_the_api_does_not_have_is_asked_about_once_and_kept() {
        let journal = journal_with(&waiting_since(INDEXING_BUDGET * 2)).await;
        let api = ProbeSpy::missing();
        let reconciler = Reconciler::new(journal.clone(), api.clone());

        let first = reconciler.probe_overdue().await.expect("probed");
        let second = reconciler.probe_overdue().await.expect("probed again");

        assert_eq!(api.gets(), 1, "one answer is enough to stop asking");
        assert_eq!(
            first.still_missing,
            vec![MemoryId::new("mem-1").expect("a valid id")],
            "the caller is told once, so it can log it once"
        );
        assert!(second.is_quiet(), "and not told again");
        assert_eq!(
            state_of(&journal).await,
            IndexState::Indexing,
            "one lookup is not enough to destroy the only local record"
        );
    }

    #[tokio::test]
    async fn a_failed_probe_is_retried_because_it_said_nothing_about_the_memory() {
        let journal = journal_with(&waiting_since(INDEXING_BUDGET * 2)).await;
        let api = ProbeSpy::failing();
        let reconciler = Reconciler::new(journal.clone(), api.clone());

        let first = reconciler.probe_overdue().await.expect("the tick survives");
        reconciler.probe_overdue().await.expect("the tick survives");

        assert_eq!(api.gets(), 2);
        assert_eq!(first.failed, 1);
        assert!(
            first.still_missing.is_empty(),
            "a failed request is not evidence the memory is missing"
        );
    }

    #[tokio::test]
    async fn an_empty_journal_costs_nothing() {
        let journal = Arc::new(InMemoryJournal::new());
        let api = ProbeSpy::found();

        let report = Reconciler::new(journal, api.clone())
            .probe_overdue()
            .await
            .expect("probed");

        assert_eq!(api.gets(), 0);
        assert!(report.is_quiet());
    }
}
