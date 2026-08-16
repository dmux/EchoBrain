//! Does whisper actually transcribe Portuguese speech through this pipeline?
//!
//! Ignored by default: it needs the ~570 MB model (`make model`) and takes a
//! few seconds. Run it with `make verify-audio`.
//!
//! # Why synthetic speech rather than the microphone
//!
//! macOS hands an unpermitted process **silence**, not an error. A microphone
//! test therefore passes trivially and proves nothing — and worse, whisper
//! hallucinates plausible phrases over silence, so the output looks like a
//! success. Observed directly while building this: five seconds of silence
//! transcribed as "E aí", confidently.
//!
//! `say` gives real speech with known content and no permission prompt, so the
//! assertion is about the recogniser rather than about the machine's settings.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

use echobrain_audio::{DEFAULT_MODEL, WhisperTranscriber, model, to_whisper_input};
use echobrain_core::Transcriber;

/// Render text to 16 kHz mono f32 with the system voice.
fn speak(text: &str, to: &Path) -> bool {
    Command::new("say")
        .args(["--data-format=LEF32@16000", "--file-format=WAVE", "-o"])
        .arg(to)
        .arg(text)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The float samples out of a canonical RIFF/WAVE file.
///
/// Chunks are walked rather than assuming a 44-byte header: `say` inserts a
/// `FLLR` padding chunk, so the data does not start where a minimal writer
/// would put it.
fn samples_of(wav: &[u8]) -> Vec<f32> {
    let mut cursor = 12; // past "RIFF" + size + "WAVE"
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
        cursor = body + size;
    }
    Vec::new()
}

#[test]
#[ignore = "needs the speech model; run `make verify-audio`"]
fn portuguese_speech_is_transcribed_rather_than_hallucinated() {
    let model = model::require_model(DEFAULT_MODEL).expect("run `make model` first");

    let spoken = "Decidimos usar índice vetorial no DynamoDB por causa do custo por byte.";
    let wav = std::env::temp_dir().join("echobrain-transcription-test.wav");
    assert!(speak(spoken, &wav), "`say` must produce the fixture");

    let bytes = std::fs::read(&wav).expect("the fixture is readable");
    let raw = samples_of(&bytes);
    assert!(!raw.is_empty(), "the WAV must contain samples");

    // `say` already emits 16 kHz mono, so this exercises the pass-through path.
    // The resampling path has its own unit tests.
    let samples = to_whisper_input(&raw, 16_000, 1).expect("already in the right format");

    let transcriber =
        WhisperTranscriber::load(&model, Some("pt".to_string())).expect("the model loads");

    let transcript = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(transcriber.transcribe(&samples))
        .expect("speech transcribes");

    let text = transcript.text.to_lowercase();
    println!("transcrito: {}", transcript.text);

    // Not an exact match: recognisers punctuate and case differently, and
    // asserting the whole sentence makes the test brittle for no gain. These
    // are the content words that decide whether recall would find this memory.
    for expected in ["dynamodb", "vetorial", "custo"] {
        assert!(
            text.contains(expected),
            "expected {expected:?} in the transcript, got: {}",
            transcript.text
        );
    }

    let _ = std::fs::remove_file(&wav);
}
