//! Telling whether the person speaking is the person who set this up.
//!
//! # What this is, stated plainly
//!
//! A speaker embedding is a vector summarising how a voice sounds. Two clips of
//! the same person land close together; two people land apart. Comparing a live
//! utterance against an enrolled average answers *is this probably them* with a
//! number.
//!
//! # What this is not
//!
//! **It is not a lock.** A recording of you passes it. Nothing here should ever
//! gate access to anything, and the two things it is wired to do reflect that:
//!
//! * **Filter** — with the microphone open all day, the television and the
//!   person at the next desk otherwise become memories. Dropping what does not
//!   match is worth doing, and the cost of a mistake is one lost utterance.
//! * **Attribute** — stamp who spoke onto the memory, next to the `source` that
//!   already says which device wrote it.
//!
//! The hotkey is **never** filtered. Pressing a key is an unambiguous statement
//! of intent, and an app that refused to obey it because a cold made someone
//! sound different would be broken in the least forgivable way.

use std::time::SystemTime;

/// A voice, as a point in the model's space.
///
/// Always unit-length, so [`VoiceProfile::similarity`] is a dot product and the
/// comparison cannot be skewed by how loud someone was.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoiceEmbedding(pub(crate) Vec<f32>);

impl VoiceEmbedding {
    /// Normalise on construction rather than trusting the caller.
    ///
    /// Returns `None` for a zero or non-finite vector — silence through an
    /// embedding model, which would otherwise normalise to `NaN` and compare
    /// as similar to everything.
    pub fn new(values: Vec<f32>) -> Option<Self> {
        if values.is_empty() || values.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm <= f32::EPSILON {
            return None;
        }
        Some(Self(values.into_iter().map(|v| v / norm).collect()))
    }

    pub fn dimensions(&self) -> usize {
        self.0.len()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }
}

/// What to do with a voice that does not match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownVoice {
    /// Do nothing. The default, and the honest one until a profile has been
    /// enrolled and its threshold checked against the user's own voice.
    #[default]
    Allow,
    /// Drop the utterance before it costs a transcription.
    Ignore,
}

impl UnknownVoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ignore => "ignore",
        }
    }
}

/// The enrolled voice: an average of several utterances.
///
/// Averaging rather than keeping one sample because a single recording carries
/// that day's cold, that room's echo and that sentence's vowels. Three to five
/// short phrases average most of that out.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoiceProfile {
    centroid: VoiceEmbedding,
    /// How many utterances went into it, shown so a two-sample profile can be
    /// recognised as the weak thing it is.
    pub enrolled_from: usize,
    pub enrolled_at: SystemTime,
}

impl VoiceProfile {
    /// Fewer than this and the average is one bad day away from useless.
    pub const MIN_ENROLMENT: usize = 3;

    /// Default acceptance threshold, on `0.0..=1.0`.
    ///
    /// Deliberately shown as a number in the settings screen rather than hidden
    /// behind a word. The right value depends on the microphone, the room and
    /// how different the other voices around are, and the only way to choose it
    /// is to see your own score.
    pub const DEFAULT_THRESHOLD: f32 = 0.45;

    /// Build a profile from enrolment samples.
    ///
    /// Rejects fewer than [`Self::MIN_ENROLMENT`], and rejects a mix of
    /// dimensions — which means the model changed under a saved profile, and
    /// comparing across that would produce numbers that mean nothing.
    pub fn enrol(samples: &[VoiceEmbedding], at: SystemTime) -> Result<Self, EnrolmentError> {
        if samples.len() < Self::MIN_ENROLMENT {
            return Err(EnrolmentError::TooFew {
                got: samples.len(),
                needed: Self::MIN_ENROLMENT,
            });
        }
        let dimensions = samples[0].dimensions();
        if samples.iter().any(|s| s.dimensions() != dimensions) {
            return Err(EnrolmentError::MixedDimensions);
        }

        let mut sum = vec![0.0f32; dimensions];
        for sample in samples {
            for (total, value) in sum.iter_mut().zip(sample.as_slice()) {
                *total += value;
            }
        }

        // Averaging opposing vectors can cancel to zero — samples that are not
        // the same person at all. Better to refuse than to enrol a centroid
        // that matches nobody.
        let centroid = VoiceEmbedding::new(sum).ok_or(EnrolmentError::Inconsistent)?;

        Ok(Self {
            centroid,
            enrolled_from: samples.len(),
            enrolled_at: at,
        })
    }

    /// Cosine similarity against the enrolled voice, on `0.0..=1.0`.
    ///
    /// Both vectors are unit-length, so this is a dot product. Negative
    /// similarity is clamped away: "less alike than unrelated" is not a
    /// distinction anyone can act on, and showing a negative percentage would
    /// invite reading it as a score.
    pub fn similarity(&self, other: &VoiceEmbedding) -> f32 {
        if other.dimensions() != self.centroid.dimensions() {
            return 0.0;
        }
        self.centroid
            .as_slice()
            .iter()
            .zip(other.as_slice())
            .map(|(a, b)| a * b)
            .sum::<f32>()
            .clamp(0.0, 1.0)
    }

    /// Whether an utterance should be treated as the enrolled speaker.
    pub fn accepts(&self, other: &VoiceEmbedding, threshold: f32) -> bool {
        self.similarity(other) >= threshold.clamp(0.0, 1.0)
    }

    pub fn dimensions(&self) -> usize {
        self.centroid.dimensions()
    }

    pub fn centroid(&self) -> &VoiceEmbedding {
        &self.centroid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnrolmentError {
    #[error("matrícula precisa de pelo menos {needed} amostras, recebeu {got}")]
    TooFew { got: usize, needed: usize },

    #[error("as amostras vieram de modelos diferentes")]
    MixedDimensions,

    #[error("as amostras não soam como a mesma pessoa")]
    Inconsistent,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedding(values: &[f32]) -> VoiceEmbedding {
        VoiceEmbedding::new(values.to_vec()).expect("a usable vector")
    }

    fn enrol(samples: &[VoiceEmbedding]) -> VoiceProfile {
        VoiceProfile::enrol(samples, SystemTime::UNIX_EPOCH).expect("enrols")
    }

    #[test]
    fn an_embedding_is_normalised_so_loudness_cannot_skew_the_comparison() {
        let quiet = embedding(&[1.0, 0.0, 0.0]);
        let loud = embedding(&[50.0, 0.0, 0.0]);
        assert_eq!(quiet, loud, "the same voice at two volumes is one point");
    }

    #[test]
    fn silence_through_the_model_is_rejected_rather_than_normalised_to_nan() {
        // A zero vector divided by its zero norm is NaN in every lane, and NaN
        // compares as neither similar nor dissimilar — which in a threshold
        // check reads as "not them" sometimes and "them" other times.
        assert!(VoiceEmbedding::new(vec![0.0; 8]).is_none());
        assert!(VoiceEmbedding::new(vec![f32::NAN, 1.0]).is_none());
        assert!(VoiceEmbedding::new(Vec::new()).is_none());
    }

    #[test]
    fn a_profile_needs_more_than_one_sample() {
        // One recording carries that day's cold and that room's echo.
        let one = vec![embedding(&[1.0, 0.0])];
        assert_eq!(
            VoiceProfile::enrol(&one, SystemTime::UNIX_EPOCH),
            Err(EnrolmentError::TooFew {
                got: 1,
                needed: VoiceProfile::MIN_ENROLMENT
            })
        );
    }

    #[test]
    fn the_same_voice_scores_far_above_a_different_one() {
        let profile = enrol(&[
            embedding(&[1.0, 0.1, 0.0]),
            embedding(&[0.98, 0.15, 0.02]),
            embedding(&[1.0, 0.05, 0.01]),
        ]);

        let same = embedding(&[0.99, 0.12, 0.0]);
        let different = embedding(&[0.0, 0.2, 1.0]);

        assert!(profile.similarity(&same) > 0.95, "same voice");
        assert!(profile.similarity(&different) < 0.4, "different voice");
        assert!(profile.accepts(&same, VoiceProfile::DEFAULT_THRESHOLD));
        assert!(!profile.accepts(&different, VoiceProfile::DEFAULT_THRESHOLD));
    }

    #[test]
    fn a_profile_from_two_different_people_is_refused_rather_than_averaged() {
        // Opposing vectors cancel. The resulting centroid would match nobody,
        // and every later comparison would quietly score near zero.
        let samples = vec![
            embedding(&[1.0, 0.0]),
            embedding(&[-1.0, 0.0]),
            embedding(&[1.0, 0.0]),
            embedding(&[-1.0, 0.0]),
        ];
        assert_eq!(
            VoiceProfile::enrol(&samples, SystemTime::UNIX_EPOCH),
            Err(EnrolmentError::Inconsistent)
        );
    }

    #[test]
    fn a_profile_from_a_different_model_scores_zero_rather_than_garbage() {
        // Upgrading the embedding model changes the dimension. Comparing across
        // it must not produce a number that looks like a score.
        let profile = enrol(&[
            embedding(&[1.0, 0.0, 0.0]),
            embedding(&[1.0, 0.1, 0.0]),
            embedding(&[0.9, 0.0, 0.1]),
        ]);
        assert_eq!(profile.similarity(&embedding(&[1.0, 0.0])), 0.0);
        assert!(!profile.accepts(&embedding(&[1.0, 0.0]), 0.0001));
    }

    #[test]
    fn similarity_never_goes_negative() {
        let profile = enrol(&[
            embedding(&[1.0, 0.0]),
            embedding(&[1.0, 0.05]),
            embedding(&[0.98, 0.0]),
        ]);
        // Diametrically opposite: a true cosine of -1.
        assert_eq!(profile.similarity(&embedding(&[-1.0, 0.0])), 0.0);
    }

    #[test]
    fn mixed_dimensions_are_refused_at_enrolment_too() {
        let samples = vec![
            embedding(&[1.0, 0.0, 0.0]),
            embedding(&[1.0, 0.0]),
            embedding(&[1.0, 0.0, 0.0]),
        ];
        assert_eq!(
            VoiceProfile::enrol(&samples, SystemTime::UNIX_EPOCH),
            Err(EnrolmentError::MixedDimensions)
        );
    }

    #[test]
    fn the_default_is_to_allow_unknown_voices() {
        // Filtering must be something the user turned on after seeing their own
        // score, never something that silently starts discarding speech.
        assert_eq!(UnknownVoice::default(), UnknownVoice::Allow);
        assert_eq!(UnknownVoice::Allow.as_str(), "allow");
    }

    #[test]
    fn the_threshold_is_clamped_so_a_slider_cannot_produce_nonsense() {
        let profile = enrol(&[
            embedding(&[1.0, 0.0]),
            embedding(&[1.0, 0.02]),
            embedding(&[0.99, 0.0]),
        ]);
        let same = embedding(&[1.0, 0.01]);
        assert!(profile.accepts(&same, -5.0), "a floor below zero accepts");
        assert!(!profile.accepts(&same, 9.0), "a ceiling above one rejects");
    }
}
