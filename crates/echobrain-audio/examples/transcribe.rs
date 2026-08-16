//! Record from the microphone and print the transcript. No Tauri, no AWS.
//!
//! ```bash
//! make model      # once: downloads ~570 MB
//! make transcribe
//! ```
//!
//! This exists to isolate failures. When dictation misbehaves in the app the
//! cause is whisper, the resampler, the microphone permission or the IPC layer,
//! and those look identical from the UI. Here only the first three are in play.
//!
//! **macOS will attach the microphone permission to the terminal**, not to
//! EchoBrain, because that is the process asking. Granting it here says nothing
//! about the bundled app, which prompts separately on first use.

use std::time::{Duration, Instant};

use echobrain_audio::{DEFAULT_MODEL, MicrophoneCapture, WhisperTranscriber, model};
use echobrain_core::{AudioCapture, Transcriber};

const RECORD_FOR: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = model::require_model(DEFAULT_MODEL)?;
    println!("→ carregando o modelo de {}", path.display());

    let started = Instant::now();
    let transcriber = WhisperTranscriber::load(&path, Some("pt".to_string()))?;
    println!("✓ modelo carregado em {:?}", started.elapsed());

    let microphone = MicrophoneCapture::new();
    microphone.start().await?;
    println!("→ gravando por {RECORD_FOR:?}… fale agora");

    // Print the level so a dead microphone is obvious before transcription
    // rather than after, when it looks like a whisper problem.
    let deadline = Instant::now() + RECORD_FOR;
    let mut peak = 0.0f32;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let level = microphone.level();
        peak = peak.max(level);
        let bars = (level * 40.0) as usize;
        println!("  {:<40} {level:.3}", "▮".repeat(bars.min(40)));
    }

    let samples = microphone.stop().await?;
    println!(
        "✓ capturado {:?} de áudio a 16 kHz mono (pico {peak:.3})",
        samples.duration()
    );

    if peak < 0.001 {
        println!(
            "⚠ o nível ficou em zero o tempo todo — o microfone provavelmente \
             não tem permissão para este processo"
        );
    }

    let started = Instant::now();
    let transcript = transcriber.transcribe(&samples).await?;
    println!("✓ transcrito em {:?}", started.elapsed());
    println!("\n  {}\n", transcript.text);

    Ok(())
}
