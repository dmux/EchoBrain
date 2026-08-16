//! Getting microphone audio into the one format a recogniser accepts.
//!
//! A capture device picks its own rate — 48 kHz on every Mac tested — and its
//! own channel count. Whisper wants **16 kHz mono f32**, and given anything
//! else it resamples internally, badly and without saying so: the transcript
//! comes back plausible and wrong, which is the worst failure mode available.
//!
//! [`echobrain_core::Samples`] refuses any other rate, so the obligation to
//! convert is checked by the compiler-adjacent type rather than left to a
//! comment. This module is what satisfies it.

use echobrain_core::{REQUIRED_SAMPLE_RATE, Samples};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("the capture reported {0} channels; at least one is required")]
    NoChannels(u16),

    #[error("the capture produced no samples")]
    Empty,

    #[error("could not build a resampler from {from} Hz to {to} Hz")]
    Construct {
        from: u32,
        to: u32,
        #[source]
        source: rubato::ResamplerConstructionError,
    },

    #[error("resampling failed")]
    Process(#[source] rubato::ResampleError),

    #[error(transparent)]
    Format(#[from] echobrain_core::AudioFormatError),
}

/// Chunk size fed to the resampler.
///
/// Only affects internal FFT sizing, not the result. 1024 keeps the transform
/// cheap while still amortising per-call overhead over a useful span.
const CHUNK: usize = 1024;

/// Interleaved device audio to [`Samples`].
///
/// `input` is interleaved by channel, as `cpal` delivers it. Multi-channel
/// input is averaged to mono rather than having one channel picked: on a
/// laptop the second channel is often the quieter one, and choosing wrong
/// halves the signal.
pub fn to_whisper_input(
    input: &[f32],
    sample_rate: u32,
    channels: u16,
) -> Result<Samples, ResampleError> {
    if channels == 0 {
        return Err(ResampleError::NoChannels(channels));
    }
    if input.is_empty() {
        return Err(ResampleError::Empty);
    }

    let mono = downmix(input, channels);

    let resampled = if sample_rate == REQUIRED_SAMPLE_RATE {
        // Already right. Skipping the resampler avoids its edge transients.
        mono
    } else {
        resample(&mono, sample_rate)?
    };

    Ok(Samples::new(resampled, REQUIRED_SAMPLE_RATE)?)
}

/// Average interleaved channels into one.
fn downmix(input: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return input.to_vec();
    }

    let channels = usize::from(channels);
    let scale = 1.0 / channels as f32;
    input
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() * scale)
        .collect()
}

fn resample(mono: &[f32], from: u32) -> Result<Vec<f32>, ResampleError> {
    let mut resampler = Fft::<f32>::new(
        from as usize,
        REQUIRED_SAMPLE_RATE as usize,
        CHUNK,
        1, // one channel: the downmix already happened
        FixedSync::Input,
    )
    .map_err(|source| ResampleError::Construct {
        from,
        to: REQUIRED_SAMPLE_RATE,
        source,
    })?;

    // `process_all_into_buffer` handles a clip of arbitrary length: it chunks
    // internally and trims the resampler's own start-up delay, which is what
    // makes doing this by hand easy to get subtly wrong.
    let input = vec![mono.to_vec()];
    let input =
        SequentialSliceOfVecs::new(&input, 1, mono.len()).map_err(|_| ResampleError::Empty)?;

    let capacity = resampler.process_all_needed_output_len(mono.len());
    let mut output = vec![vec![0.0f32; capacity]];
    let mut sink = SequentialSliceOfVecs::new_mut(&mut output, 1, capacity)
        .map_err(|_| ResampleError::Empty)?;

    let (_read, written) = resampler
        .process_all_into_buffer(&input, &mut sink, mono.len(), None)
        .map_err(ResampleError::Process)?;

    let mut channel = output.remove(0);
    channel.truncate(written);
    Ok(channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `hz`, sampled at `rate`, for `seconds`.
    fn tone(hz: f32, rate: u32, seconds: f32) -> Vec<f32> {
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .map(|i| (i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    }

    #[test]
    fn forty_eight_kilohertz_becomes_sixteen() {
        // The case that actually happens: every Mac tested captures at 48 kHz.
        let input = tone(440.0, 48_000, 1.0);
        let samples = to_whisper_input(&input, 48_000, 1).expect("resamples");

        // Exactly 16 000 is not guaranteed — the resampler works in chunks and
        // the tail is padded — but it must be close, and never the input length.
        let produced = samples.len();
        assert!(
            (15_000..=17_000).contains(&produced),
            "one second at 16 kHz should be about 16 000 samples, got {produced}"
        );
    }

    #[test]
    fn a_device_already_at_sixteen_kilohertz_is_passed_through_untouched() {
        let input = tone(440.0, 16_000, 0.5);
        let samples = to_whisper_input(&input, 16_000, 1).expect("passes through");
        assert_eq!(
            samples.len(),
            input.len(),
            "no resampling means no length change and no edge transients"
        );
        assert_eq!(samples.as_slice(), input.as_slice());
    }

    #[test]
    fn stereo_is_averaged_rather_than_having_a_channel_picked() {
        // Left loud, right silent. Picking a channel gives either 1.0 or 0.0;
        // averaging gives 0.5, which is what a downmix should produce.
        let interleaved: Vec<f32> = (0..32).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let mono = downmix(&interleaved, 2);

        assert_eq!(mono.len(), 32, "two channels halve the frame count");
        assert!(mono.iter().all(|&s| (s - 0.5).abs() < f32::EPSILON));
    }

    #[test]
    fn the_result_is_always_a_valid_samples_value() {
        // `Samples::new` rejects non-finite values, so this asserts the
        // resampler never produces NaN from ordinary input.
        let input = tone(1000.0, 44_100, 0.25);
        let samples = to_whisper_input(&input, 44_100, 1).expect("resamples");
        assert!(samples.as_slice().iter().all(|s| s.is_finite()));
        assert!(!samples.is_empty());
    }

    #[test]
    fn empty_and_channelless_input_are_rejected_rather_than_producing_silence() {
        assert!(matches!(
            to_whisper_input(&[], 48_000, 1),
            Err(ResampleError::Empty)
        ));
        assert!(matches!(
            to_whisper_input(&[0.1, 0.2], 48_000, 0),
            Err(ResampleError::NoChannels(0))
        ));
    }

    #[test]
    fn resampling_preserves_the_signal_rather_than_returning_silence() {
        // A cheap sanity check that the output is the tone and not zeros: RMS
        // of a 0.5-amplitude sine is about 0.354.
        let input = tone(440.0, 48_000, 1.0);
        let samples = to_whisper_input(&input, 48_000, 1).expect("resamples");

        let rms =
            (samples.as_slice().iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        assert!(
            rms > 0.2,
            "the resampled tone should retain its energy, got RMS {rms}"
        );
    }
}
