//! The answering adapter: turning recalled memories into a spoken sentence.
//!
//! One port lives here, [`echobrain_core::Answerer`], reached through Amazon
//! Bedrock.
//!
//! # Why Bedrock rather than the Anthropic API directly
//!
//! Not capability — the same models are on both. It is that EchoBrain has no
//! API keys anywhere: `config.toml` holds an endpoint, a region and a profile,
//! and every credential comes from the SigV4 chain the app already builds for
//! the memory API. Bedrock signs with that same `SdkConfig`, so answering costs
//! zero new secrets. An `ANTHROPIC_API_KEY` in the config file would be the
//! first one, and it would have to be managed, rotated and kept out of logs
//! forever after.
//!
//! # What this crate is careful about
//!
//! It is the only place in EchoBrain that spends tokens, and the only place
//! that produces text nobody dictated. The prompt is closed over the memories
//! it is handed, the model is told what to say when they do not answer the
//! question, and the memories travel back attached to the answer so a caller
//! cannot show the sentence without its sources.

pub mod bedrock;
pub mod intent;
pub mod prompt;
pub mod sentences;

pub use bedrock::{BedrockAnswerer, BedrockError, DEFAULT_MAX_TOKENS, DEFAULT_MODEL_ID};
pub use intent::BedrockIntentJudge;
pub use sentences::Sentences;
