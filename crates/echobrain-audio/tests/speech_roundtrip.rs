//! Proves the app can speak, by having it listen to itself.
//!
//! # Why a round trip rather than "did `say` exit 0"
//!
//! An exit code proves a process ran. It does not prove a voice was found, that
//! the words came out in Portuguese, or that the text reached the synthesiser
//! unmangled — and every one of those has a silent failure mode. `say -v` on a
//! voice that was uninstalled fails the whole utterance; the `[[…]]` command
//! syntax swallows text without complaining.
//!
//! So the assertion runs the same direction the user experiences: render with
//! the configured voice, transcribe with the model the app already ships, and
//! check the words survived. Same reasoning as `transcription.rs`, which exists
//! because macOS hands an unpermitted process silence rather than an error.
//!
//! Ignored by default because it needs the ~570 MB speech model. Run it with
//! `make verify-tts`.

#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

use echobrain_audio::{DEFAULT_MODEL, SystemSpeaker, WhisperTranscriber, model, to_whisper_input};
use echobrain_core::Transcriber;

/// Render text to 16 kHz mono f32 with a named voice, the way `SystemSpeaker`
/// would render it to the speakers.
fn render(text: &str, voice: &str, to: &Path) -> bool {
    Command::new("/usr/bin/say")
        .args(["--data-format=LEF32@16000", "--file-format=WAVE", "-v"])
        .arg(voice)
        .arg("-o")
        .arg(to)
        .arg("--")
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
                .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                .collect();
        }
        // Chunks are word-aligned.
        cursor = body + size + (size % 2);
    }
    Vec::new()
}

fn normalise(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect()
}

#[test]
#[ignore = "needs the speech model; run `make verify-tts`"]
fn the_configured_voice_speaks_portuguese_that_transcribes_back() {
    let voice = SystemSpeaker::resolve_voice(None)
        .expect("a pt-BR voice must be installed for speech to work at all");
    eprintln!("voz: {voice}");

    let path = model::require_model(DEFAULT_MODEL)
        .expect("the speech model must be installed — run `make model`");
    let transcriber =
        WhisperTranscriber::load(&path, Some("pt".to_string())).expect("the model loads");

    // Deliberately the kind of sentence the app really says: an answer built
    // from a memory, with the accents and the proper noun that a wrong voice or
    // a wrong language hint would mangle.
    let spoken = "Você descartou o OpenSearch por causa do custo por byte.";

    let wav = std::env::temp_dir().join("echobrain-verify-tts.wav");
    assert!(
        render(spoken, &voice, &wav),
        "`say -v {voice}` failed — the configured voice cannot render"
    );

    let bytes = std::fs::read(&wav).expect("the rendered file is readable");
    let samples = samples_of(&bytes);
    assert!(
        !samples.is_empty(),
        "the voice produced no audio at all — a silent success is the failure \
         mode this whole test exists to catch"
    );

    let input = to_whisper_input(&samples, 16_000, 1).expect("already at whisper's rate");
    let transcript = tokio::runtime::Runtime::new()
        .expect("a runtime")
        .block_on(transcriber.transcribe(&input))
        .expect("the audio transcribes");

    eprintln!("falado:     {spoken}");
    eprintln!("transcrito: {}", transcript.text);

    let heard = normalise(&transcript.text);
    for word in ["descartou", "opensearch", "custo", "byte"] {
        assert!(
            heard.contains(word),
            "the voice said something, but `{word}` did not survive the round \
             trip — heard {:?}",
            transcript.text
        );
    }
}

#[test]
#[ignore = "needs the speech model; run `make verify-tts`"]
fn inline_speech_commands_are_read_aloud_rather_than_executed() {
    // `say` treats `[[…]]` as embedded commands: `[[slnc 4000]]` is a four
    // second pause, `[[volm 0]]` is silence. A memory is arbitrary dictated
    // text, so a transcript containing double brackets must be *spoken*, not
    // obeyed. `SystemSpeaker::sanitise` defangs them; this proves the audio
    // that comes out still carries the words.
    let voice = SystemSpeaker::resolve_voice(None).expect("a pt-BR voice is installed");
    let path = model::require_model(DEFAULT_MODEL).expect("the speech model is installed");
    let transcriber =
        WhisperTranscriber::load(&path, Some("pt".to_string())).expect("the model loads");

    // What `SystemSpeaker` hands to `say` after sanitising.
    let sanitised = "O relatório citava (volm 0) como exemplo de comando.";

    let wav = std::env::temp_dir().join("echobrain-verify-tts-brackets.wav");
    assert!(render(sanitised, &voice, &wav), "the voice renders");

    let bytes = std::fs::read(&wav).expect("readable");
    let samples = samples_of(&bytes);
    let input = to_whisper_input(&samples, 16_000, 1).expect("already at whisper's rate");
    let transcript = tokio::runtime::Runtime::new()
        .expect("a runtime")
        .block_on(transcriber.transcribe(&input))
        .expect("transcribes");

    eprintln!("transcrito: {}", transcript.text);
    let heard = normalise(&transcript.text);
    assert!(
        heard.contains("relat") && heard.contains("exemplo"),
        "the sentence around the brackets must survive — heard {:?}",
        transcript.text
    );
}
