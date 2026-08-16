//! Proves the endpointer finds the edges of real speech, not just of numbers.
//!
//! The unit tests in `vad.rs` drive the state machine with scores chosen by
//! hand — they pin the run lengths and the resampling arithmetic, and they
//! would keep passing if the detector stopped recognising Portuguese entirely.
//!
//! This one uses speech from `say`, sandwiched between known silences, and
//! asserts the boundaries land where the silence ends. Same reason
//! `transcription.rs` exists: macOS hands an unpermitted process silence rather
//! than an error, so a test that only ever sees silence must not be able to
//! pass.
//!
//! Run with `make verify-vad`.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use echobrain_audio::Endpointer;
use echobrain_core::{Endpoint, Endpointing, Frame};

/// What the microphone delivers per callback at 48 kHz — 20 ms.
const BLOCK: usize = 960;
const RATE: u32 = 48_000;

fn render(text: &str, to: &Path) -> bool {
    Command::new("/usr/bin/say")
        .args(["--data-format=LEF32@48000", "--file-format=WAVE", "-v"])
        .arg("Luciana")
        .arg("-o")
        .arg(to)
        .arg("--")
        .arg(text)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Float samples out of a RIFF/WAVE file, walking chunks because `say` inserts
/// an `FLLR` padding chunk before the data.
fn samples_of(wav: &[u8]) -> Vec<f32> {
    let mut cursor = 12;
    while cursor + 8 <= wav.len() {
        let id = &wav[cursor..cursor + 4];
        let size = u32::from_le_bytes([
            wav[cursor + 4],
            wav[cursor + 5],
            wav[cursor + 6],
            wav[cursor + 7],
        ]) as usize;
        let body = cursor + 8;
        if id == b"data" {
            let end = (body + size).min(wav.len());
            return wav[body..end]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
        }
        cursor = body + size + (size % 2);
    }
    Vec::new()
}

/// Feed a signal in 20 ms blocks, returning `(event, seconds)` for each
/// boundary crossed.
fn run(signal: &[f32]) -> Vec<(Endpoint, f32)> {
    let mut endpointer = Endpointer::new(Duration::from_millis(800));
    let mut found = Vec::new();

    for (index, block) in signal.chunks(BLOCK).enumerate() {
        let at = (index * BLOCK) as f32 / RATE as f32;
        for event in endpointer.push(&Frame {
            samples: block.to_vec(),
            sample_rate: RATE,
        }) {
            found.push((event, at));
        }
    }
    found
}

/// `name` scopes the temporary file to one test.
///
/// Cargo runs tests in the same binary on parallel threads, so a shared path
/// means one test rendering over the file another is reading — which showed up
/// as an utterance that appeared to run for twelve seconds and passed cleanly
/// when run alone. The most expensive kind of flake: it looks like a detector
/// bug.
fn speech(name: &str) -> Vec<f32> {
    let wav = std::env::temp_dir().join(format!("echobrain-verify-vad-{name}.wav"));
    assert!(
        render(
            "Decidimos usar índice vetorial no DynamoDB por causa do custo por byte.",
            &wav
        ),
        "`say` must render for this test to mean anything"
    );
    let bytes = std::fs::read(&wav).expect("readable");
    let samples = samples_of(&bytes);
    assert!(
        samples.len() > RATE as usize,
        "expected more than a second of speech, got {} samples",
        samples.len()
    );
    samples
}

#[test]
#[ignore = "needs `say`; run `make verify-vad`"]
fn real_speech_between_silences_is_bounded_where_the_silence_ends() {
    let speech = speech("bounds");
    let lead = RATE as usize; // 1 s
    let tail = RATE as usize * 2; // 2 s, comfortably past the 800 ms hangover

    let mut signal = vec![0.0f32; lead];
    signal.extend_from_slice(&speech);
    signal.extend(std::iter::repeat_n(0.0f32, tail));

    let events = run(&signal);
    eprintln!("eventos: {events:?}");

    let started = events
        .iter()
        .find(|(event, _)| *event == Endpoint::SpeechStarted)
        .map(|(_, at)| *at)
        .expect("speech must be detected — if this fails the detector is not hearing Portuguese");
    let ended = events
        .iter()
        .find(|(event, _)| *event == Endpoint::SpeechEnded)
        .map(|(_, at)| *at)
        .expect("the utterance must end once the silence runs long enough");

    // Onset within a beat of where the silence stops. Generous on the late side
    // because "de-" is a quiet consonant, and strict on the early side because
    // firing during the lead silence would mean the detector is not listening.
    assert!(
        (0.9..=1.4).contains(&started),
        "speech should start near 1.0 s, started at {started:.2}s"
    );

    let speech_ends = 1.0 + speech.len() as f32 / RATE as f32;
    let hangover = ended - speech_ends;
    assert!(
        (0.6..=1.6).contains(&hangover),
        "the turn should end ~0.8 s after the speech stops; \
         speech ended at {speech_ends:.2}s, turn ended at {ended:.2}s ({hangover:.2}s later)"
    );

    assert_eq!(
        events.len(),
        2,
        "one utterance is one start and one end, not a stutter: {events:?}"
    );
}

#[test]
#[ignore = "needs `say`; run `make verify-vad`"]
fn a_pause_inside_one_sentence_does_not_split_it() {
    // The hangover exists for this: someone thinking mid-sentence must not have
    // their turn ended and half a thought sent to the recogniser.
    let first = speech("pause");
    let gap = (RATE as f32 * 0.4) as usize; // 400 ms, under the 800 ms hangover

    let mut signal = vec![0.0f32; RATE as usize];
    signal.extend_from_slice(&first);
    signal.extend(std::iter::repeat_n(0.0f32, gap));
    signal.extend_from_slice(&first);
    signal.extend(std::iter::repeat_n(0.0f32, RATE as usize * 2));

    let events = run(&signal);
    eprintln!("eventos: {events:?}");

    let starts = events
        .iter()
        .filter(|(event, _)| *event == Endpoint::SpeechStarted)
        .count();
    assert_eq!(
        starts, 1,
        "a 400 ms pause must not start a second utterance: {events:?}"
    );
}

#[test]
#[ignore = "needs `say`; run `make verify-vad`"]
fn silence_alone_produces_nothing() {
    // The failure this whole file guards against: a process without microphone
    // permission receives silence, and a detector that fires on it would make
    // the conversation loop talk to an empty room.
    let events = run(&vec![0.0f32; RATE as usize * 5]);
    assert!(
        events.is_empty(),
        "five seconds of silence produced {events:?}"
    );
}
