//! Use-case tests against the memory domain's own in-memory implementation.
//!
//! `LocalMemoryService` wired to `InMemoryRepository` + `StubEmbedder` is a
//! real `MemoryService` — the same trait the deployed API is reached through —
//! so these exercise the actual code path with no network, no AWS account and
//! no cost. That is the property the crate split buys.

use std::sync::Arc;
use std::time::Duration;

use agent_memory_core::testing::{FixedClock, InMemoryRepository, SeqIdGenerator, StubEmbedder};
use agent_memory_core::{LocalMemoryService, MemoryKind, TopK};

use echobrain_core::testing::{FailingJournal, FixedClassifier, InMemoryJournal};
use echobrain_core::{
    CueClassifier, EchoError, IndexState, IngestRequest, IngestUtterance, Journal, MemoryId,
    SearchMemories, SearchRequest,
};

/// Titan Text Embeddings V2 produces 1024 floats, and the deployed vector index
/// was created with that dimension.
const DIMENSIONS: usize = 1024;

type Service = LocalMemoryService<InMemoryRepository, StubEmbedder, FixedClock, SeqIdGenerator>;

fn service() -> Arc<Service> {
    Arc::new(LocalMemoryService::new(
        InMemoryRepository::default(),
        StubEmbedder::new(DIMENSIONS),
        FixedClock::new(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        SeqIdGenerator::default(),
    ))
}

fn ingest_with(service: Arc<Service>, journal: Arc<InMemoryJournal>) -> IngestUtterance {
    IngestUtterance::new(
        service,
        Arc::new(CueClassifier::new()),
        journal,
        "echobrain-test",
    )
}

#[tokio::test]
async fn a_capture_is_stored_and_journalled_as_still_indexing() {
    let ingest = ingest_with(service(), Arc::new(InMemoryJournal::new()));

    let outcome = ingest
        .execute(IngestRequest {
            text: "  eu prefiro arquitetura hexagonal  ".to_string(),
            kind: None,
            ttl: None,
            github_repo: None,
        })
        .await
        .expect("the capture succeeds");

    assert_eq!(
        outcome.entry.text, "eu prefiro arquitetura hexagonal",
        "surrounding whitespace must not reach the store"
    );
    assert_eq!(outcome.entry.kind, MemoryKind::Preference);
    assert_eq!(outcome.entry.source, "echobrain-test");
    assert_eq!(
        outcome.entry.state,
        IndexState::Indexing,
        "a fresh write is not searchable yet and must not claim to be"
    );
    assert!(outcome.journal_failed.is_none());
}

#[tokio::test]
async fn an_explicit_kind_overrides_the_classifier() {
    // The UI chip: the guess is correctable, and the correction wins.
    let ingest = IngestUtterance::new(
        service(),
        Arc::new(FixedClassifier(MemoryKind::Episode)),
        Arc::new(InMemoryJournal::new()),
        "echobrain-test",
    );

    let outcome = ingest
        .execute(IngestRequest {
            text: "moro em Lisboa".to_string(),
            kind: Some(MemoryKind::Fact),
            ttl: None,
            github_repo: None,
        })
        .await
        .expect("the capture succeeds");

    assert_eq!(outcome.entry.kind, MemoryKind::Fact);
}

#[tokio::test]
async fn silence_is_rejected_before_it_costs_an_embedding() {
    let service = service();
    let ingest = ingest_with(Arc::clone(&service), Arc::new(InMemoryJournal::new()));

    let error = ingest
        .execute(IngestRequest {
            text: "   \n  ".to_string(),
            kind: None,
            ttl: None,
            github_repo: None,
        })
        .await
        .expect_err("whitespace is not an utterance");

    assert!(matches!(error, EchoError::EmptyUtterance));
    assert!(
        error.is_invalid_input(),
        "silence is a user outcome, not a system failure"
    );
    assert_eq!(
        service.embedder().calls(),
        0,
        "nothing may be embedded for an empty utterance — embeddings are billed"
    );
}

#[tokio::test]
async fn a_journal_failure_does_not_lose_a_capture_that_was_already_stored() {
    // The memory is durably written by this point. Reporting failure would
    // invite the user to repeat themselves and pay for a second embedding.
    let ingest = IngestUtterance::new(
        service(),
        Arc::new(CueClassifier::new()),
        Arc::new(FailingJournal),
        "echobrain-test",
    );

    let outcome = ingest
        .execute(IngestRequest {
            text: "decidimos por DynamoDB".to_string(),
            kind: None,
            ttl: None,
            github_repo: None,
        })
        .await
        .expect("the capture still succeeds");

    assert!(
        outcome.journal_failed.is_some(),
        "the local failure must be reported, not swallowed"
    );
    assert_eq!(outcome.entry.text, "decidimos por DynamoDB");
}

#[tokio::test]
async fn a_ttl_produces_an_expiry() {
    let ingest = ingest_with(service(), Arc::new(InMemoryJournal::new()));

    let outcome = ingest
        .execute(IngestRequest {
            text: "lembrete temporário".to_string(),
            kind: None,
            ttl: Some(Duration::from_secs(3600)),
            github_repo: None,
        })
        .await
        .expect("the capture succeeds");

    assert!(
        outcome.entry.expires_at.is_some(),
        "a TTL must reach the stored memory, since that is what DynamoDB TTL enforces"
    );
}

#[tokio::test]
async fn search_returns_similarity_rather_than_distance() {
    let service = service();
    let ingest = ingest_with(Arc::clone(&service), Arc::new(InMemoryJournal::new()));

    ingest
        .execute(IngestRequest {
            text: "eu prefiro café coado".to_string(),
            kind: None,
            ttl: None,
            github_repo: None,
        })
        .await
        .expect("stored");

    let search =
        SearchMemories::new(Arc::clone(&service) as Arc<dyn echobrain_core::MemoryService>);
    let hits = search
        .execute(SearchRequest::new("que café eu gosto?"))
        .await
        .expect("search succeeds");

    assert_eq!(hits.len(), 1);
    let similarity = hits[0].similarity.get();
    assert!(
        (0.0..=1.0).contains(&similarity),
        "similarity must be normalised to 0..=1, got {similarity}"
    );
}

#[tokio::test]
async fn an_empty_query_returns_nothing_instead_of_ranking_the_whole_namespace() {
    let service = service();
    let search =
        SearchMemories::new(Arc::clone(&service) as Arc<dyn echobrain_core::MemoryService>);

    let hits = search
        .execute(SearchRequest::new("   "))
        .await
        .expect("an empty query is not an error");

    assert!(hits.is_empty());
    assert_eq!(
        service.embedder().calls(),
        0,
        "an empty query must not be embedded either"
    );
}

#[tokio::test]
async fn search_honours_top_k_and_orders_best_first() {
    let service = service();
    let ingest = ingest_with(Arc::clone(&service), Arc::new(InMemoryJournal::new()));

    for text in ["primeira memória", "segunda memória", "terceira memória"] {
        ingest
            .execute(IngestRequest {
                text: text.to_string(),
                kind: None,
                ttl: None,
                github_repo: None,
            })
            .await
            .expect("stored");
    }

    let search =
        SearchMemories::new(Arc::clone(&service) as Arc<dyn echobrain_core::MemoryService>);
    let hits = search
        .execute(SearchRequest {
            text: "memória".to_string(),
            top_k: TopK::new(2).expect("in range"),
            kind: None,
            min_similarity: None,
        })
        .await
        .expect("search succeeds");

    assert_eq!(hits.len(), 2, "top_k must be respected");
    assert!(
        hits[0].similarity >= hits[1].similarity,
        "results must be ordered best-first once converted to similarity"
    );
}

#[tokio::test]
async fn the_journal_lists_captures_and_tombstones_forgotten_ones() {
    let journal = Arc::new(InMemoryJournal::new());
    let ingest = ingest_with(service(), Arc::clone(&journal));

    let outcome = ingest
        .execute(IngestRequest {
            text: "algo memorável".to_string(),
            kind: None,
            ttl: None,
            github_repo: None,
        })
        .await
        .expect("stored");

    assert_eq!(journal.recent(10).await.expect("lists").len(), 1);
    assert_eq!(journal.awaiting_index().await.expect("lists").len(), 1);

    journal
        .mark_indexed(&outcome.entry.memory_id)
        .await
        .expect("promotes");
    assert!(
        journal.awaiting_index().await.expect("lists").is_empty(),
        "a promoted entry stops awaiting the index"
    );

    journal
        .tombstone(&outcome.entry.memory_id)
        .await
        .expect("tombstones");
    assert!(
        journal.recent(10).await.expect("lists").is_empty(),
        "a forgotten memory leaves the library view"
    );

    // Tombstoning something absent is not an error: the journal is a cache, and
    // the memory may have been written from another device.
    journal
        .tombstone(&MemoryId::new("mem-unknown").expect("valid"))
        .await
        .expect("tombstoning an unknown id is a no-op");
}
