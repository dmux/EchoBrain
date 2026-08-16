//! Driven adapters for the audio side of the hexagon.
//!
//! Three ports live here, all declared in `echobrain-core`:
//!
//! * [`ports::AudioCapture`](echobrain_core::AudioCapture) — the microphone, via `cpal`.
//! * [`ports::Transcriber`](echobrain_core::Transcriber) — whisper.cpp over Metal.
//! * [`ports::Speaker`](echobrain_core::Speaker) — the macOS system voices, via `say`.
//!
//! Nothing here knows about Tauri, AWS or the memory API. The use cases in
//! `echobrain-core` see only the traits, which is what lets a recogniser be
//! swapped — for a hosted one, or for a fake in a test — without the domain
//! noticing.

pub mod capture;
pub mod identity;
pub mod model;
pub mod polly;
pub mod resample;
pub mod vad;

#[cfg(target_os = "macos")]
pub mod speech;
#[cfg(target_os = "macos")]
pub mod whisper;

pub use capture::{CaptureError, MicrophoneCapture};
pub use identity::WeSpeakerIdentity;
pub use model::{ModelError, model_path};
pub use polly::{DEFAULT_POLLY_VOICE, PollyError, PollySpeaker};
pub use resample::{ResampleError, to_whisper_input};
pub use vad::Endpointer;

#[cfg(target_os = "macos")]
pub use speech::{DEFAULT_RATE, DEFAULT_VOICE, SpeechError, SystemSpeaker, Voice};

#[cfg(target_os = "macos")]
pub use whisper::{DEFAULT_MODEL, WhisperError, WhisperTranscriber};
