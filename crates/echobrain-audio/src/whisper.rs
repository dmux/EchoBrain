//! Local speech recognition, via whisper.cpp over Metal.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use echobrain_core::{EchoError, Samples, Transcriber, Transcript};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// The model this app is tuned for.
///
/// `large-v3-turbo` quantised to q5_0 is the smallest model that transcribes
/// accented pt-BR reliably; `base` and `small` drop enough words to make the
/// stored memory misleading, which is worse than no memory at all. On Metal it
/// still runs comfortably faster than real time.
pub const DEFAULT_MODEL: &str = "ggml-large-v3-turbo-q5_0.bin";

#[derive(Debug, thiserror::Error)]
pub enum WhisperError {
    #[error("the model file {0} is missing — download it before transcribing")]
    ModelMissing(PathBuf),

    #[error("whisper could not load the model")]
    Load(#[source] whisper_rs::WhisperError),

    #[error("whisper failed while transcribing")]
    Run(#[source] whisper_rs::WhisperError),
}

/// A loaded whisper model.
///
/// Loading costs hundreds of milliseconds and a good deal of memory, so the
/// context is built once and shared. `WhisperContext` is `Send + Sync`; each
/// transcription creates its own state.
pub struct WhisperTranscriber {
    /// Shared so a transcription can move onto a blocking thread without
    /// borrowing `self` across the await.
    context: Arc<WhisperContext>,
    /// BCP-47 hint. `None` lets whisper detect, which is slower and, on short
    /// utterances, often wrong — a pt-BR sentence detected as Spanish is
    /// transcribed as garbled Spanish rather than flagged.
    language: Option<String>,
}

impl std::fmt::Debug for WhisperTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperTranscriber")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl WhisperTranscriber {
    pub fn load(model: &Path, language: Option<String>) -> Result<Self, WhisperError> {
        if !model.exists() {
            return Err(WhisperError::ModelMissing(model.to_path_buf()));
        }

        let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
            .map_err(WhisperError::Load)?;

        Ok(Self {
            context: Arc::new(context),
            language,
        })
    }
}

#[async_trait]
impl Transcriber for WhisperTranscriber {
    async fn transcribe(&self, samples: &Samples) -> Result<Transcript, EchoError> {
        // A whisper pass is seconds of solid CPU and GPU work. Running it
        // directly would stall every other task on the runtime — including the
        // IPC handler that is supposed to keep the waveform moving.
        let context = Arc::clone(&self.context);
        let language = self.language.clone();
        let audio = samples.as_slice().to_vec();

        tokio::task::spawn_blocking(move || run(&context, language, &audio))
            .await
            .map_err(|error| EchoError::transcription(Box::new(error)))?
    }
}

fn run(
    context: &WhisperContext,
    language: Option<String>,
    audio: &[f32],
) -> Result<Transcript, EchoError> {
    // `Samples` can only exist at 16 kHz mono, so there is no format check
    // to forget here — the type already made the mistake unrepresentable.
    let mut state = context
        .create_state()
        .map_err(|error| EchoError::transcription(WhisperError::Run(error)))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    if let Some(language) = &language {
        params.set_language(Some(language));
    }
    // This is a batch call on a finished recording, so progress output is
    // noise; whisper prints to stdout otherwise.
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, audio)
        .map_err(|error| EchoError::transcription(WhisperError::Run(error)))?;

    let mut text = String::new();
    for index in 0..state.full_n_segments() {
        let Some(segment) = state.get_segment(index) else {
            continue;
        };
        // Lossy: a model can emit a partial multi-byte sequence at a segment
        // edge, and losing one character beats losing the whole utterance to a
        // decode error.
        let piece = segment
            .to_str_lossy()
            .map_err(|error| EchoError::transcription(WhisperError::Run(error)))?;
        text.push_str(&piece);
    }

    let text = text.trim().to_string();
    if text.is_empty() {
        // Silence. A normal outcome of holding the hotkey and saying nothing,
        // not a failure of the recogniser.
        return Err(EchoError::EmptyUtterance);
    }

    Ok(Transcript { text, language })
}
