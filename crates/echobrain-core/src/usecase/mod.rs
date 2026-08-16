//! Use cases: what EchoBrain does, expressed only in terms of ports.

pub mod answer;
pub mod ingest;
pub mod reconcile;
pub mod search;
pub mod turn;

pub use answer::{AnswerQuestion, AnswerRequest, AnswerStyle, DEFAULT_MIN_SIMILARITY};
pub use ingest::{IngestRequest, IngestUtterance, Ingested};
pub use reconcile::Reconciler;
pub use search::{SearchMemories, SearchRequest};
pub use turn::{Action, DEFAULT_FOLLOW_UP, Event, Phase, Turn};
