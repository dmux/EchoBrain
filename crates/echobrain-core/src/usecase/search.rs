//! Searching stored memories by meaning.

use std::fmt;
use std::sync::Arc;

use agent_memory_core::{Distance, MemoryKind, MemoryService, RecallQuery, TopK};

use crate::error::EchoError;
use crate::model::{Hit, Similarity, caller_placeholder};

/// A search as the UI expresses it.
///
/// Note the threshold is a *minimum similarity*, not a maximum distance. The
/// memory domain's `RecallQuery` takes the latter, correctly, because that is
/// the direction its index works in. Asking a view layer to invert it is asking
/// it to get it backwards eventually, so the inversion happens here — once.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub text: String,
    pub top_k: TopK,
    pub kind: Option<MemoryKind>,
    /// Drop hits weaker than this, on `0.0..=1.0`.
    pub min_similarity: Option<f32>,
}

impl SearchRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            top_k: TopK::DEFAULT,
            kind: None,
            min_similarity: None,
        }
    }
}

pub struct SearchMemories {
    memory: Arc<dyn MemoryService>,
}

impl fmt::Debug for SearchMemories {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchMemories").finish_non_exhaustive()
    }
}

impl SearchMemories {
    pub fn new(memory: Arc<dyn MemoryService>) -> Self {
        Self { memory }
    }

    pub async fn execute(&self, request: SearchRequest) -> Result<Vec<Hit>, EchoError> {
        let text = request.text.trim();
        if text.is_empty() {
            // An empty query would embed whitespace and rank the whole
            // namespace against it. Cheaper and more honest to return nothing.
            return Ok(Vec::new());
        }

        // similarity = 1 - distance/2, so distance = (1 - similarity) * 2.
        let max_distance = request
            .min_similarity
            .map(|similarity| Distance::new((1.0 - similarity.clamp(0.0, 1.0)) * 2.0))
            .transpose()?;

        let hits = self
            .memory
            .recall(RecallQuery {
                user_id: caller_placeholder()?,
                text: text.to_string(),
                top_k: request.top_k,
                kind: request.kind,
                max_distance,
            })
            .await?;

        // Already ascending by distance, and the domain's `LocalMemoryService`
        // re-sorts defensively, so this preserves best-first ordering.
        Ok(hits
            .into_iter()
            .map(|hit| Hit {
                similarity: Similarity::from_distance(hit.distance),
                memory: hit.memory,
            })
            .collect())
    }
}
