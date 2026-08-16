//! EchoBrain's domain: capturing speech and turning it into semantic memory.
//!
//! # What is not here
//!
//! No Tauri, no cpal, no whisper, no SQLite, no AWS SDK. The dependency list in
//! `Cargo.toml` is short enough to read, and that is the point — the boundary is
//! enforced by the compiler, so an accidental import fails the build rather than
//! quietly eroding it.
//!
//! # Why there is no `MemoryService` here
//!
//! Because it already exists. `agent_memory_core::MemoryService` is the primary
//! port of the sibling `aws-dynamodb-search-vector-rust-example` project, and
//! `agent_memory_client::RemoteMemoryService` is a working implementation over
//! HTTP + SigV4. EchoBrain is a third **driving adapter** on that port,
//! alongside the deployed Lambda and the MCP server — which is why this project
//! contains no backend, no embedding code and no DynamoDB.
//!
//! `agent_memory_core` is itself dependency-free of AWS, so depending on it
//! costs this crate nothing architecturally.

pub mod classify;
pub mod error;
pub mod intent;
pub mod model;
pub mod ports;
pub mod usecase;
pub mod voice;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use classify::CueClassifier;
pub use error::{BoxError, EchoError};
pub use intent::{CueIntent, Intent, IntentClassifier};
pub use model::{
    Answer, AudioFormatError, Endpoint, Frame, Hit, IndexState, JournalEntry, REQUIRED_SAMPLE_RATE,
    Samples, Similarity, Transcript, caller_placeholder,
};
pub use ports::{
    Answerer, AudioCapture, AudioMonitor, Discard, Endpointing, IntentJudge, Journal,
    KindClassifier, SentenceSink, Speaker, Transcriber, VoiceIdentity,
};
pub use usecase::{
    Action, AnswerQuestion, AnswerRequest, AnswerStyle, DEFAULT_FOLLOW_UP, DEFAULT_MIN_SIMILARITY,
    Event, IngestRequest, IngestUtterance, Ingested, Phase, Reconciler, SearchMemories,
    SearchRequest, Turn,
};
pub use voice::{EnrolmentError, UnknownVoice, VoiceEmbedding, VoiceProfile};

// Re-exported so adapters and the app do not need a direct dependency on the
// sibling crate just to name the types crossing this boundary.
pub use agent_memory_core::{
    Cursor, ListQuery, Memory, MemoryError, MemoryFilter, MemoryId, MemoryKind, MemoryPage,
    MemoryService, PageSize, ScoredMemory, TopK, UpdateCommand, UserId,
};
