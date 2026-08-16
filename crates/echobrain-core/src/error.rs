//! One error type for the whole hexagon.

use agent_memory_core::MemoryError;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EchoError {
    /// A failure from the memory layer, passed through unchanged so that
    /// [`MemoryError::is_invalid_input`] still discriminates user error from
    /// something worth retrying.
    #[error(transparent)]
    Memory(#[from] MemoryError),

    /// Whatever sits behind [`crate::Transcriber`] — whisper.cpp, today.
    #[error("could not transcribe the audio")]
    Transcription(#[source] BoxError),

    #[error("the local journal failed")]
    Journal(#[source] BoxError),

    #[error("could not capture audio")]
    Audio(#[source] BoxError),

    /// The system voice or Polly, whichever is behind [`crate::Speaker`].
    #[error("could not speak")]
    Speech(#[source] BoxError),

    /// Composing an answer from the recalled memories failed.
    #[error("could not compose an answer")]
    Answering(#[source] BoxError),

    /// Silence, or a recogniser that returned nothing. Not a failure of the
    /// system — a normal outcome of pressing the hotkey and saying nothing —
    /// so callers are expected to handle it without surfacing an error.
    #[error("nothing was said")]
    EmptyUtterance,
}

impl EchoError {
    pub fn transcription(source: impl Into<BoxError>) -> Self {
        Self::Transcription(source.into())
    }

    pub fn journal(source: impl Into<BoxError>) -> Self {
        Self::Journal(source.into())
    }

    pub fn audio(source: impl Into<BoxError>) -> Self {
        Self::Audio(source.into())
    }

    pub fn speech(source: impl Into<BoxError>) -> Self {
        Self::Speech(source.into())
    }

    pub fn answering(source: impl Into<BoxError>) -> Self {
        Self::Answering(source.into())
    }

    /// Whether the caller supplied something invalid, as opposed to the system
    /// failing. Mirrors the memory layer's own discriminator so a UI can decide
    /// between "fix your input" and "try again" without inspecting variants.
    pub fn is_invalid_input(&self) -> bool {
        match self {
            Self::Memory(error) => error.is_invalid_input(),
            Self::EmptyUtterance => true,
            _ => false,
        }
    }
}
