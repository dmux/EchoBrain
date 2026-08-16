//! Deciding when someone started and stopped talking.
//!
//! # What this is for
//!
//! The hotkey never needed this: the user says when they are done by pressing
//! the key again. A conversation has no second press — the app has to notice
//! the silence itself, and notice it fast enough that the pause before an
//! answer feels like listening rather than lag.
//!
//! # Two rates, on purpose
//!
//! [`earshot`] is a neural detector that wants 16 kHz in frames of exactly 256
//! samples. The microphone runs at whatever the device chose, usually 48 kHz.
//!
//! Rather than resample everything properly, the detector path uses linear
//! interpolation — cheap, running thirty times a second, and feeding something
//! that only has to answer *is this speech*. The audio that reaches whisper
//! still goes through `rubato` unchanged: a recogniser is worth an FFT, a
//! gate is not.
//!
//! # Why the run lengths are asymmetric
//!
//! Speech is declared after ~50 ms of voiced frames and silence after ~800 ms.
//! A short onset means the first syllable is not clipped; a long hangover means
//! the natural pause in "então… a gente decidiu" does not end the turn. The
//! cost of being wrong runs the same direction as everywhere else in this app:
//! cutting someone off is worse than waiting a beat too long.

use earshot::Detector;
use echobrain_core::{Endpoint, Endpointing, Frame};

/// What the detector consumes: 16 kHz, exactly this many samples.
const DETECTOR_FRAME: usize = 256;

/// The rate the detector was trained at.
const DETECTOR_RATE: f64 = 16_000.0;

/// One detector frame, in seconds.
const FRAME_SECONDS: f32 = DETECTOR_FRAME as f32 / DETECTOR_RATE as f32;

/// Above this score a frame counts as voice. `earshot` documents 0.5 as the
/// general threshold; a touch higher trades a little onset sensitivity for
/// noticeably fewer false starts from keyboard noise and room tone.
const VOICE_THRESHOLD: f32 = 0.55;

/// Consecutive voiced frames before speech is declared. ~48 ms.
const ONSET_FRAMES: u32 = 3;

/// Streaming endpointer over device-rate frames.
pub struct Endpointer {
    detector: Box<Detector>,
    source_rate: u32,
    /// Device-rate samples not yet consumed by the resampler.
    carry: Vec<f32>,
    /// Fractional read position into `carry`.
    position: f64,
    /// 16 kHz samples awaiting a full detector frame.
    pending: Vec<f32>,

    speaking: bool,
    voiced_run: u32,
    silent_run: u32,
    hangover_frames: u32,
}

impl std::fmt::Debug for Endpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpointer")
            .field("speaking", &self.speaking)
            .field("source_rate", &self.source_rate)
            .finish_non_exhaustive()
    }
}

impl Endpointer {
    /// `hangover` is how much silence ends an utterance.
    pub fn new(hangover: std::time::Duration) -> Self {
        Self {
            detector: Detector::default_boxed(),
            source_rate: 0,
            carry: Vec::new(),
            position: 0.0,
            pending: Vec::with_capacity(DETECTOR_FRAME * 2),
            speaking: false,
            voiced_run: 0,
            silent_run: 0,
            hangover_frames: (hangover.as_secs_f32() / FRAME_SECONDS).ceil().max(1.0) as u32,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// Forget everything, keeping the configuration. Called between turns so a
    /// new utterance does not inherit the tail of the last one.
    fn reset_state(&mut self) {
        self.detector.reset();
        self.carry.clear();
        self.pending.clear();
        self.position = 0.0;
        self.speaking = false;
        self.voiced_run = 0;
        self.silent_run = 0;
    }

    /// Feed one microphone frame. Returns whatever boundaries it crossed —
    /// usually none, at most one in practice.
    fn push_frame(&mut self, frame: &Frame) -> Vec<Endpoint> {
        if frame.sample_rate == 0 || frame.samples.is_empty() {
            return Vec::new();
        }

        // A device change mid-stream would otherwise resample against the old
        // rate and read as continuous silence.
        if frame.sample_rate != self.source_rate {
            self.reset_state();
            self.source_rate = frame.sample_rate;
        }

        self.carry.extend_from_slice(&frame.samples);
        self.resample_into_pending();

        let mut events = Vec::new();
        while self.pending.len() >= DETECTOR_FRAME {
            let chunk: Vec<f32> = self.pending.drain(..DETECTOR_FRAME).collect();
            // The detector's contract is samples in [-1, 1]; a hot input would
            // otherwise trip its debug assertion and skew the features.
            let clamped: Vec<f32> = chunk.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
            let score = self.detector.predict_f32(&clamped);
            if let Some(event) = self.observe(score) {
                events.push(event);
            }
        }
        events
    }

    /// Advance the state machine by one scored frame.
    fn observe(&mut self, score: f32) -> Option<Endpoint> {
        // A negative score is `earshot` rejecting the frame shape, which cannot
        // happen here — but treating it as speech would latch the machine on.
        let voiced = score >= VOICE_THRESHOLD;

        if voiced {
            self.voiced_run += 1;
            self.silent_run = 0;
        } else {
            self.silent_run += 1;
            self.voiced_run = 0;
        }

        if !self.speaking && self.voiced_run >= ONSET_FRAMES {
            self.speaking = true;
            return Some(Endpoint::SpeechStarted);
        }
        if self.speaking && self.silent_run >= self.hangover_frames {
            self.speaking = false;
            return Some(Endpoint::SpeechEnded);
        }
        None
    }

    /// Linear-interpolate `carry` down to 16 kHz, appending to `pending`.
    fn resample_into_pending(&mut self) {
        if self.source_rate == 0 {
            return;
        }
        let ratio = f64::from(self.source_rate) / DETECTOR_RATE;

        while self.position + 1.0 < self.carry.len() as f64 {
            let index = self.position as usize;
            let fraction = (self.position - index as f64) as f32;
            let sample = self.carry[index] * (1.0 - fraction) + self.carry[index + 1] * fraction;
            self.pending.push(sample);
            self.position += ratio;
        }

        // Drop what the read head has passed, so `carry` does not grow without
        // bound over a long conversation.
        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            let consumed = consumed.min(self.carry.len());
            self.carry.drain(..consumed);
            self.position -= consumed as f64;
        }
    }
}

impl Endpointing for Endpointer {
    fn push(&mut self, frame: &Frame) -> Vec<Endpoint> {
        self.push_frame(frame)
    }

    fn reset(&mut self) {
        self.reset_state();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    fn endpointer() -> Endpointer {
        Endpointer::new(Duration::from_millis(800))
    }

    fn frame(samples: Vec<f32>, rate: u32) -> Frame {
        Frame {
            samples,
            sample_rate: rate,
        }
    }

    #[test]
    fn silence_never_starts_an_utterance() {
        let mut endpointer = endpointer();
        for _ in 0..100 {
            let events = endpointer.push(&frame(vec![0.0; 960], 48_000));
            assert!(events.is_empty(), "silence must not trip the detector");
        }
        assert!(!endpointer.is_speaking());
    }

    #[test]
    fn the_resampler_lands_on_the_expected_rate() {
        // 48 kHz in, 16 kHz out: one second of input must produce about one
        // second of detector frames. Getting this wrong scales every timing in
        // the state machine — the hangover would be three times what it says.
        let mut endpointer = endpointer();
        endpointer.source_rate = 48_000;
        endpointer.carry = vec![0.0; 48_000];
        endpointer.resample_into_pending();

        let produced = endpointer.pending.len();
        assert!(
            (15_900..=16_100).contains(&produced),
            "expected ~16000 samples at 16 kHz, produced {produced}"
        );
    }

    #[test]
    fn a_non_integer_ratio_still_resamples() {
        // 44.1 kHz is not a multiple of 16 kHz, which is why this interpolates
        // rather than decimating by an integer step.
        let mut endpointer = endpointer();
        endpointer.source_rate = 44_100;
        endpointer.carry = vec![0.0; 44_100];
        endpointer.resample_into_pending();

        let produced = endpointer.pending.len();
        assert!(
            (15_900..=16_100).contains(&produced),
            "expected ~16000 samples, produced {produced}"
        );
    }

    #[test]
    fn the_carry_does_not_grow_without_bound() {
        // A conversation runs for minutes. Holding every sample would be a leak
        // that only shows up in the sessions that matter most.
        let mut endpointer = endpointer();
        for _ in 0..500 {
            endpointer.push(&frame(vec![0.0; 960], 48_000));
        }
        assert!(
            endpointer.carry.len() < 4_000,
            "carry grew to {} samples",
            endpointer.carry.len()
        );
        assert!(endpointer.pending.len() < DETECTOR_FRAME);
    }

    #[test]
    fn the_state_machine_needs_a_run_before_declaring_speech() {
        let mut endpointer = endpointer();

        // One loud frame is a door slam, not a sentence.
        assert_eq!(endpointer.observe(0.9), None);
        assert_eq!(endpointer.observe(0.9), None);
        assert_eq!(endpointer.observe(0.9), Some(Endpoint::SpeechStarted));
        assert!(endpointer.is_speaking());
    }

    #[test]
    fn a_short_pause_does_not_end_the_turn() {
        let mut endpointer = endpointer();
        for _ in 0..ONSET_FRAMES {
            endpointer.observe(0.9);
        }
        assert!(endpointer.is_speaking());

        // ~300 ms of silence: the beat in "então… a gente decidiu".
        for _ in 0..19 {
            assert_eq!(endpointer.observe(0.1), None);
        }
        assert!(
            endpointer.is_speaking(),
            "a natural pause must not end the utterance"
        );

        // Speaking again clears the count entirely.
        endpointer.observe(0.9);
        for _ in 0..19 {
            assert_eq!(endpointer.observe(0.1), None);
        }
        assert!(endpointer.is_speaking());
    }

    #[test]
    fn the_hangover_ends_the_turn_at_the_configured_silence() {
        let mut endpointer = Endpointer::new(Duration::from_millis(800));
        for _ in 0..ONSET_FRAMES {
            endpointer.observe(0.9);
        }

        // 800 ms / 16 ms = 50 frames.
        let mut ended = None;
        for index in 0..80 {
            if let Some(event) = endpointer.observe(0.0) {
                ended = Some(index + 1);
                assert_eq!(event, Endpoint::SpeechEnded);
                break;
            }
        }

        assert_eq!(
            ended,
            Some(50),
            "the hangover must match what was configured"
        );
        assert!(!endpointer.is_speaking());
    }

    #[test]
    fn a_device_change_resets_rather_than_reading_as_silence() {
        let mut endpointer = endpointer();
        endpointer.push(&frame(vec![0.0; 960], 48_000));
        for _ in 0..ONSET_FRAMES {
            endpointer.observe(0.9);
        }
        assert!(endpointer.is_speaking());

        // Switching to AirPods mid-conversation changes the rate. Resampling
        // against the old one would produce garbage that scores as silence.
        endpointer.push(&frame(vec![0.0; 320], 16_000));
        assert!(
            !endpointer.is_speaking(),
            "a rate change resets the machine"
        );
        assert_eq!(endpointer.source_rate, 16_000);
    }

    #[test]
    fn an_empty_or_rateless_frame_is_ignored() {
        let mut endpointer = endpointer();
        assert!(endpointer.push(&frame(Vec::new(), 48_000)).is_empty());
        assert!(endpointer.push(&frame(vec![0.1; 100], 0)).is_empty());
    }
}
