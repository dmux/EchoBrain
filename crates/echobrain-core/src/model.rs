//! Types EchoBrain adds on top of the memory domain.
//!
//! The memory layer already owns [`Memory`], [`MemoryKind`], [`Distance`] and
//! the identifiers. What lives here is what a *voice capture client* needs and
//! the memory layer has no opinion about: audio in the one format a recogniser
//! accepts, and similarity in the one direction a person reads.

use std::time::{Duration, SystemTime};

use agent_memory_core::{Distance, MemoryError, MemoryId, MemoryKind, UserId};

/// The sample rate every speech recogniser in this project expects.
///
/// Whisper resamples internally if given anything else, badly and silently.
/// Making it a constant that [`Samples`] enforces means a capture adapter that
/// forgets to resample fails at construction rather than producing transcripts
/// that are subtly wrong.
pub const REQUIRED_SAMPLE_RATE: u32 = 16_000;

/// Mono PCM audio at [`REQUIRED_SAMPLE_RATE`], the only shape [`crate::ports::Transcriber`] accepts.
#[derive(Debug, Clone, PartialEq)]
pub struct Samples(Vec<f32>);

impl Samples {
    /// `rate` is taken and checked rather than assumed, because the capture
    /// device picks it — 48 kHz on most Macs — and the resampling step between
    /// device and here is exactly the one easy to leave out.
    pub fn new(data: Vec<f32>, rate: u32) -> Result<Self, AudioFormatError> {
        if rate != REQUIRED_SAMPLE_RATE {
            return Err(AudioFormatError::WrongSampleRate {
                expected: REQUIRED_SAMPLE_RATE,
                actual: rate,
            });
        }
        if data.is_empty() {
            return Err(AudioFormatError::Empty);
        }
        if let Some((index, value)) = data.iter().enumerate().find(|(_, v)| !v.is_finite()) {
            return Err(AudioFormatError::NonFiniteSample {
                index,
                value: *value,
            });
        }
        Ok(Self(data))
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Duration of the clip. Used to decide whether a hotkey press was a real
    /// utterance or a slip.
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.0.len() as f64 / f64::from(REQUIRED_SAMPLE_RATE))
    }
}

// No `Eq`: `NonFiniteSample` carries the offending `f32`, and NaN is exactly
// the value that variant exists to report.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum AudioFormatError {
    #[error("audio must be {expected} Hz mono, got {actual} Hz — resample before constructing")]
    WrongSampleRate { expected: u32, actual: u32 },

    #[error("the capture produced no samples")]
    Empty,

    #[error("sample {index} is not finite ({value})")]
    NonFiniteSample { index: usize, value: f32 },
}

/// What a recogniser produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    /// BCP-47 tag when the recogniser reports one. Whisper detects it, and is
    /// the only recogniser wired up today — a hosted one would more likely be
    /// told the language up front, so this stays optional.
    pub language: Option<String>,
}

/// How close a hit is, on `0.0..=1.0` where 1 is identical.
///
/// [`Distance`] is the memory domain's ranking type and it is correct: a COSINE
/// index returns 0 for identical and 2 for opposite, so ascending order really
/// is best-first. It is also exactly backwards from what someone reading a list
/// expects, and a bar filled in proportion to distance would draw the best match
/// as the emptiest one.
///
/// Converting once, here, is what keeps that inversion out of every view. No
/// component receives a `Distance`, so no component can render it by mistake.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Similarity(f32);

impl Similarity {
    /// Clamped because the metric's 0..=2 range is a property of COSINE, and a
    /// different index type could hand back something outside it.
    pub fn from_distance(distance: Distance) -> Self {
        Self((1.0 - distance.get() / 2.0).clamp(0.0, 1.0))
    }

    pub fn get(self) -> f32 {
        self.0
    }

    /// For display. Rounded once, here, so two views cannot disagree.
    pub fn percent(self) -> u8 {
        (self.0 * 100.0).round() as u8
    }
}

/// A search result, as the UI consumes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub memory: agent_memory_core::Memory,
    pub similarity: Similarity,
}

/// A block of microphone audio, mono, at whatever rate the device runs.
///
/// # Why this is not [`Samples`]
///
/// [`Samples`] is resampled to 16 kHz because that is what whisper wants, and
/// it validates that on construction. A monitoring frame has not earned that
/// yet: it arrives every ~20 ms straight from the audio callback, and
/// resampling each one would mean running an FFT thirty times a second to feed
/// a voice-activity detector that works perfectly well at 48 kHz.
///
/// So frames stay at the device rate and carry it, and the conversion happens
/// once — when an utterance has ended and there is something worth
/// transcribing.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Frame {
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f32(self.samples.len() as f32 / self.sample_rate as f32)
    }
}

/// A boundary the endpointer noticed in the microphone stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// Someone started talking. Fired once per utterance.
    SpeechStarted,
    /// The utterance ended, after the configured silence.
    SpeechEnded,
}

/// A spoken answer, and the memories it was built from.
///
/// # Why `cited` is not optional
///
/// This is the one type in the domain whose text nobody dictated — a model
/// wrote it. Recall used to be self-verifying: what came back was, word for
/// word, what went in. A composed answer is not, and it arrives by voice, where
/// there is nothing to skim.
///
/// So the memories that produced it travel *with* it, and every surface that
/// renders an [`Answer`] renders these too. Making the field an `Option`, or
/// letting a caller drop it, would turn a claim the user can check into one
/// they have to trust.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// What is spoken and shown, in the user's language.
    pub text: String,

    /// The memories the answer drew on, best-first. Empty only when
    /// `grounded` is false.
    pub cited: Vec<Hit>,

    /// Whether any memory actually supported the answer.
    ///
    /// False means `text` is the honest admission rather than a composed
    /// answer. The distinction matters to the UI: "não encontrei nada" is not a
    /// result with zero sources, it is a different outcome.
    pub grounded: bool,

    /// Whether a model was called to produce this.
    ///
    /// # Why this is separate from `grounded`
    ///
    /// There are two ways to end up with nothing to say, and they cost
    /// different amounts:
    ///
    /// * **Nothing cleared the similarity floor.** No model was called and no
    ///   token was spent — [`AnswerQuestion`](crate::AnswerQuestion) refuses
    ///   before the network.
    /// * **The memories cleared the floor and did not answer the question.**
    ///   The model was called, read them, and correctly declined. That is a
    ///   billed call.
    ///
    /// Both produce `grounded: false` and an empty `cited`, so without this
    /// flag they are indistinguishable — and anything reporting cost would have
    /// to guess. It guessed wrong the first time this ran for real.
    pub consulted: bool,
}

impl Answer {
    /// What the app says when recall finds nothing. Deliberately one fixed
    /// sentence: a model asked to improvise here will eventually improvise an
    /// answer instead.
    pub const NOTHING_FOUND: &'static str = "Não encontrei nada sobre isso nas suas memórias.";

    /// Nothing cleared the floor, so no model was consulted and nothing was
    /// spent.
    pub fn nothing_found() -> Self {
        Self {
            text: Self::NOTHING_FOUND.to_string(),
            cited: Vec::new(),
            grounded: false,
            consulted: false,
        }
    }

    /// The model was consulted, read the memories and declined to answer from
    /// them. Costs the same as any other question.
    pub fn declined() -> Self {
        Self {
            consulted: true,
            ..Self::nothing_found()
        }
    }
}

/// Whether a written memory is known to be searchable yet.
///
/// DynamoDB vector search is eventually consistent: the sibling repository's
/// smoke test budgets 90 seconds between a write and the write turning up in a
/// search. A UI that hides a memory until the index catches up looks broken, so
/// captures are shown immediately and carry this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// Written, not yet observed in a search.
    Indexing,
    /// Observed in a search at least once. Never goes back.
    Indexed,
    /// Deleted from this device. Kept as a tombstone because the journal is
    /// append-only and the server has no list endpoint to reconcile against.
    Forgotten,
}

impl IndexState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Indexed => "indexed",
            Self::Forgotten => "forgotten",
        }
    }
}

/// One row of the local write journal.
///
/// The journal exists because the memory API has no list endpoint — discovery
/// is semantic-only, so a memory whose existence you have forgotten is
/// unreachable. It is a **cache, never an inventory**: it holds what *this
/// device* wrote, so memories written by another client of the same namespace —
/// the MCP server, an agent through the Lambda — are absent from it, and
/// server-side TTL expiries happen without notice. Any view built on it has to
/// say so rather than imply completeness.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub memory_id: MemoryId,
    pub kind: MemoryKind,
    pub text: String,
    pub created_at: SystemTime,
    pub expires_at: Option<SystemTime>,
    pub source: String,
    pub github_login: Option<String>,
    pub github_repo: Option<String>,
    /// 1..=5 once the user has judged it, `None` while they have not. Mirrored
    /// from the server rather than owned here: rating a memory on another client
    /// must not be silently reverted by this one's copy of the row.
    pub rating: Option<u8>,
    /// Deactivated memories stay stored and stay listed — that is the point of
    /// the flag over a delete — but the server keeps them out of recall.
    pub active: bool,
    pub state: IndexState,
}

/// The `user_id` every [`agent_memory_core::MemoryService`] call must carry,
/// and that the deployed API ignores.
///
/// The server derives the real namespace from the SigV4 principal it verified,
/// so nothing chosen here can address another user's memories. The trait still
/// requires a value, and this is the honest one: named for what it is instead
/// of dressed up as an identity. Mirrors `caller_placeholder` in the sibling
/// repository's MCP server.
pub fn caller_placeholder() -> Result<UserId, MemoryError> {
    UserId::new("caller")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_inverts_distance_so_the_best_match_reads_highest() {
        let identical = Similarity::from_distance(Distance::new(0.0).expect("finite"));
        let orthogonal = Similarity::from_distance(Distance::new(1.0).expect("finite"));
        let opposite = Similarity::from_distance(Distance::new(2.0).expect("finite"));

        assert_eq!(identical.percent(), 100);
        assert_eq!(orthogonal.percent(), 50);
        assert_eq!(opposite.percent(), 0);
        assert!(
            identical > opposite,
            "similarity must order the opposite way round from distance"
        );
    }

    #[test]
    fn similarity_clamps_metrics_that_leave_the_cosine_range() {
        // DOT_PRODUCT indexes can score negative. The type must not produce a
        // percentage above 100 or below 0 if the index type ever changes.
        let below = Similarity::from_distance(Distance::new(-3.0).expect("finite"));
        let above = Similarity::from_distance(Distance::new(9.0).expect("finite"));
        assert_eq!(below.get(), 1.0);
        assert_eq!(above.get(), 0.0);
    }

    #[test]
    fn samples_reject_the_device_rate_instead_of_silently_mis_transcribing() {
        let error = Samples::new(vec![0.0; 128], 48_000).expect_err("48 kHz must be refused");
        assert_eq!(
            error,
            AudioFormatError::WrongSampleRate {
                expected: 16_000,
                actual: 48_000
            }
        );
        assert!(Samples::new(vec![0.0; 128], 16_000).is_ok());
    }

    #[test]
    fn samples_reject_empty_and_non_finite_input() {
        assert_eq!(
            Samples::new(Vec::new(), 16_000).expect_err("empty"),
            AudioFormatError::Empty
        );
        assert!(matches!(
            Samples::new(vec![0.0, f32::NAN], 16_000).expect_err("NaN"),
            AudioFormatError::NonFiniteSample { index: 1, .. }
        ));
    }

    #[test]
    fn duration_is_derived_from_the_fixed_rate() {
        let one_second = Samples::new(vec![0.0; 16_000], 16_000).expect("valid");
        assert_eq!(one_second.duration().as_millis(), 1000);
    }
}
