//! The hexagon's boundary.
//!
//! The **primary port is not defined here** — it is
//! [`agent_memory_core::MemoryService`], owned by the sibling repository. That
//! is the whole reason EchoBrain has no backend of its own: the deployed
//! Lambda and this desktop app are two driving adapters over one port, and the
//! HTTP client that reaches it is already written.
//!
//! What this module declares is what a *voice client* additionally needs from
//! the outside world. Every one is a driven port, and every one is satisfied by
//! a macOS adapter in `echobrain-audio` and by a fake in [`crate::testing`] —
//! which is what keeps the use cases free of both `cfg(target_os)` and a
//! microphone.

use async_trait::async_trait;

use agent_memory_core::{MemoryId, MemoryKind};

use crate::error::EchoError;
use crate::model::{Answer, Endpoint, Frame, Hit, JournalEntry, Samples, Transcript};
use crate::voice::VoiceEmbedding;

/// Speech to text.
///
/// Today this is whisper.cpp running locally over Metal. A hosted recogniser
/// would share no code with it and have wildly different latency — precisely
/// the asymmetry a port exists to absorb, and the reason the use cases never
/// learn which one answered.
#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, samples: &Samples) -> Result<Transcript, EchoError>;
}

/// Microphone capture.
///
/// Implementations are responsible for resampling to
/// [`crate::model::REQUIRED_SAMPLE_RATE`] — [`Samples`] will refuse anything
/// else, so the obligation is checked rather than documented.
#[async_trait]
pub trait AudioCapture: Send + Sync {
    /// Begin capturing. Idempotent: starting an already-running capture is not
    /// an error, because the hotkey can repeat.
    async fn start(&self) -> Result<(), EchoError>;

    /// Stop and return everything captured since [`Self::start`].
    async fn stop(&self) -> Result<Samples, EchoError>;

    /// Current input level on `0.0..=1.0`, for the waveform. Cheap enough to
    /// poll at frame rate; returns 0 when not capturing.
    fn level(&self) -> f32;
}

/// The microphone as a continuous stream, rather than one utterance at a time.
///
/// # Why this is separate from [`AudioCapture`]
///
/// Batch capture is the right shape for the hotkey: press, speak, release, get
/// one utterance. It is the wrong shape for anything that has to *decide* when
/// speech started and stopped, because that decision needs the audio while it
/// is arriving.
///
/// Keeping them apart means the hotkey path pays none of the monitoring cost —
/// no frames allocated, no channel, no detector running — which matters when
/// the alternative is holding the microphone open all day.
#[async_trait]
pub trait AudioMonitor: Send + Sync {
    /// Open the device and start delivering frames. Idempotent.
    ///
    /// This is where the microphone light comes on and stays on, so callers are
    /// expected to treat it as a mode the user turned on rather than something
    /// to do speculatively.
    async fn start_monitoring(&self) -> Result<(), EchoError>;

    /// Frames as they arrive, ~20 ms each, mono, at the device's rate.
    ///
    /// Dropping the receiver is how a listener unsubscribes. A slow listener
    /// has its frames **dropped**, never buffered without bound and never
    /// allowed to block: the sending side is a realtime audio callback, where
    /// waiting produces an audible glitch.
    fn frames(&self) -> std::sync::mpsc::Receiver<Frame>;

    /// Stop delivering frames and close the device if nothing else needs it.
    fn stop_monitoring(&self);

    fn is_monitoring(&self) -> bool;
}

/// Deciding when speech started and stopped, from frames as they arrive.
///
/// A port rather than a concrete type because the detector is a real
/// dependency with a real model behind it, and the conversation loop must be
/// testable by handing it scripted boundaries instead of synthesised audio.
pub trait Endpointing: Send {
    /// Feed one frame. Returns whatever boundaries it crossed — usually none.
    fn push(&mut self, frame: &Frame) -> Vec<Endpoint>;

    /// Forget accumulated state between turns, so a new utterance does not
    /// inherit the tail of the last one.
    fn reset(&mut self);
}

/// Turning an utterance into a point in speaker space.
///
/// # Why this is a port with no default implementation
///
/// The model behind it is a real download and a real inference stack, and the
/// feature it powers is off until someone enrols. Everything that *decides*
/// anything — the averaging, the threshold, what happens to a voice that does
/// not match — is in [`crate::voice`] and tested without it.
///
/// Note what is absent: no `verify`, no `is_owner`. This port only measures.
/// Deciding is the domain's job, and keeping the decision out of the adapter is
/// what stops "probably them" from quietly becoming an authorisation check.
pub trait VoiceIdentity: Send + Sync {
    fn embed(&self, samples: &Samples) -> Result<VoiceEmbedding, EchoError>;

    /// Dimensions the model produces, so a saved profile from a different one
    /// can be recognised instead of compared against.
    fn dimensions(&self) -> usize;
}

/// Where an answer's sentences go as they are produced.
///
/// A model emits tokens for several seconds. Waiting for the last one before
/// speaking the first word makes the app feel broken, so the adapter hands over
/// each sentence as it closes and the caller starts speaking it immediately.
///
/// Not async, and not fallible: this is called from inside a token loop, and a
/// consumer that cannot keep up must drop rather than stall the stream.
pub trait SentenceSink: Send {
    fn sentence(&mut self, sentence: &str);
}

/// A sink that discards everything, for callers that only want the finished
/// [`Answer`] — the smoke test, the library view, anything not speaking.
#[derive(Debug, Clone, Copy, Default)]
pub struct Discard;

impl SentenceSink for Discard {
    fn sentence(&mut self, _sentence: &str) {}
}

/// Composes an answer from memories that were already recalled.
///
/// # The one rule this port exists to enforce
///
/// The implementation is given the question and the hits, and **nothing else**
/// — no conversation history, no tools, no ability to look anything up. If the
/// hits do not answer the question, the contract is to say so rather than to
/// fill the gap. Everything about the shape of this trait is meant to make the
/// grounded case the only easy one to implement.
///
/// This is the first thing in EchoBrain that costs tokens. It is optional for
/// exactly that reason: without it, recall still works and still returns the
/// memories — what is lost is the sentence tying them together.
///
/// [`AnswerStyle`] varies the *form* of the answer and nothing else. An
/// implementation that relaxed a grounding rule for the written style would
/// defeat the paragraph above at the exact moment the answer got long enough to
/// hide an invention in.
///
/// [`AnswerStyle`]: crate::AnswerStyle
#[async_trait]
pub trait Answerer: Send + Sync {
    async fn answer(
        &self,
        question: &str,
        hits: &[Hit],
        style: crate::AnswerStyle,
        sink: &mut dyn SentenceSink,
    ) -> Result<Answer, EchoError>;
}

/// Decides between storing and asking for an utterance the cue router could
/// not classify.
///
/// # Why this is a second port and not a better [`crate::IntentClassifier`]
///
/// Two reasons, and the first is mechanical: this one is `async`, because the
/// only implementation worth having makes a network call. Routing every
/// utterance through it would make the cue path — which decides in
/// microseconds and is right whenever the user said "grava" or asked a
/// question outright — wait on a model.
///
/// The second is cost. Answering is the one feature in this app that spends
/// tokens, and it was kept behind an explicitly configured model for that
/// reason. Judging intent spends them earlier, on utterances that may never
/// reach the answering layer at all, so it stays just as optional: absent, the
/// cue router's default stands and the app behaves exactly as it did before.
#[async_trait]
pub trait IntentJudge: Send + Sync {
    async fn judge(&self, text: &str) -> Result<crate::Intent, EchoError>;
}

/// Text to speech.
///
/// Two adapters already sit behind it: the macOS system voices through `say`,
/// and Polly over the network. They share no code and have very different
/// startup latency — the same asymmetry [`Transcriber`] exists to absorb.
///
/// # Why `stop` is on the port rather than left to the adapter
///
/// Barge-in — talking over the answer to interrupt it — is what separates a
/// conversation from a phone menu, and the conversation use case has to be able
/// to trigger it without knowing which platform is speaking. A caller invokes
/// it the moment the microphone hears speech, so it must be safe to call when
/// nothing is speaking at all.
#[async_trait]
pub trait Speaker: Send + Sync {
    /// Speak, resolving when the audio has finished playing.
    ///
    /// Concurrent calls queue rather than overlap: an answer arrives as a
    /// stream of sentences, and two voices talking at once is not a feature.
    async fn speak(&self, text: &str) -> Result<(), EchoError>;

    /// Silence whatever is speaking and drop anything queued behind it.
    /// Idempotent — barge-in fires without first checking whether it needs to.
    fn stop(&self);

    fn is_speaking(&self) -> bool;
}

/// The local write journal.
///
/// See [`JournalEntry`] for why this exists and why it must never be presented
/// as a complete inventory.
#[async_trait]
pub trait Journal: Send + Sync {
    /// Record a memory this device just wrote, as [`crate::model::IndexState::Indexing`].
    async fn record(&self, entry: &JournalEntry) -> Result<(), EchoError>;

    /// Most recent entries first, tombstones excluded.
    async fn recent(&self, limit: usize) -> Result<Vec<JournalEntry>, EchoError>;

    /// Entries still awaiting confirmation that the vector index caught up.
    async fn awaiting_index(&self) -> Result<Vec<JournalEntry>, EchoError>;

    /// Promote to [`crate::model::IndexState::Indexed`]. Idempotent.
    async fn mark_indexed(&self, memory_id: &MemoryId) -> Result<(), EchoError>;

    /// Tombstone rather than delete: the row is the only local evidence the
    /// memory ever existed, and the server has no list endpoint to rebuild from.
    async fn tombstone(&self, memory_id: &MemoryId) -> Result<(), EchoError>;
}

/// Guesses which of the memory domain's three kinds a piece of speech is.
///
/// Deliberately not an LLM. This project has no inference budget, so the
/// implementation is a heuristic and is wrong sometimes — which is why the UI
/// shows the choice as an editable chip rather than a settled fact.
pub trait KindClassifier: Send + Sync {
    fn classify(&self, text: &str) -> MemoryKind;
}
