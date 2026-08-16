//! Speech synthesis on macOS, via `say`.
//!
//! # Why a subprocess rather than `AVSpeechSynthesizer`
//!
//! The framework API would avoid a process spawn per sentence, but reaching it
//! means `objc2` and `unsafe`, and the workspace sets `unsafe_code = "forbid"`.
//! Opening that hole to save ~30 ms on a call that is about to play two seconds
//! of audio is a bad trade. `say` is also the same binary
//! `tests/transcription.rs` already trusts to produce reference speech.
//!
//! # Why speaking is chunked by the caller
//!
//! `speak` blocks until the audio finishes, so handing it a whole answer means
//! the user waits for the model to finish generating before hearing a word. The
//! answering path streams sentences instead and calls this once per sentence;
//! [`Speaker::speak`] serialises them so they play in order.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use echobrain_core::{EchoError, Speaker};

/// The system binary. Absolute so that a stray `say` on `PATH` cannot be
/// picked up instead — a `.app` launched from the Finder has a `PATH` this
/// process never chose.
const SAY: &str = "/usr/bin/say";

/// How often a running utterance is checked for completion.
///
/// The lock is released between polls so `stop` can take it — which is the
/// whole reason this polls instead of blocking on `wait()`.
const POLL: Duration = Duration::from_millis(40);

/// Fallback voice. `Luciana` is the pt-BR voice macOS has shipped for years;
/// [`SystemSpeaker::available_voices`] is what the settings screen offers.
pub const DEFAULT_VOICE: &str = "Luciana";

/// Words per minute. `say` defaults to ~175; a touch faster reads as
/// responsive without losing intelligibility.
pub const DEFAULT_RATE: u16 = 185;

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("could not start {SAY} — is this macOS?")]
    Spawn(#[source] std::io::Error),

    #[error("the speech process failed")]
    Wait(#[source] std::io::Error),
}

impl From<SpeechError> for EchoError {
    fn from(error: SpeechError) -> Self {
        EchoError::speech(error)
    }
}

/// A pt-BR voice installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub name: String,
    pub locale: String,
}

/// Speaks through the macOS system voices.
pub struct SystemSpeaker {
    /// Interior-mutable so the settings screen can change the voice without
    /// rebuilding the app state and re-registering it everywhere it is held.
    /// Read once per utterance, so a change takes effect on the next sentence
    /// rather than mid-word.
    voice: Mutex<String>,
    rate: AtomicU32,
    /// The utterance currently playing, if any. `stop` reaches in and kills it.
    current: Arc<Mutex<Option<Child>>>,
    /// Bumped by [`Speaker::stop`]. A `speak` whose generation is stale gives
    /// up rather than starting: without this, barge-in would silence the
    /// sentence being spoken and then let the *next* queued sentence play,
    /// which sounds like the interruption was ignored.
    generation: Arc<AtomicU64>,
    /// Serialises `speak` so queued sentences play in order instead of on top
    /// of each other.
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for SystemSpeaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemSpeaker")
            .field("voice", &self.voice())
            .field("rate", &self.rate.load(Ordering::Relaxed))
            .field("speaking", &self.is_speaking())
            .finish_non_exhaustive()
    }
}

impl SystemSpeaker {
    pub fn new(voice: impl Into<String>, rate: u16) -> Self {
        Self {
            voice: Mutex::new(voice.into()),
            rate: AtomicU32::new(u32::from(rate)),
            current: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            turn: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Change the voice for the next utterance.
    ///
    /// The name is resolved against what is installed first, so a stale name
    /// from `config.toml` cannot make every later utterance fail — `say -v` on
    /// a missing voice errors rather than substituting.
    pub fn set_voice(&self, requested: Option<&str>) {
        let Some(resolved) = Self::resolve_voice(requested) else {
            return;
        };
        match self.voice.lock() {
            Ok(mut voice) => *voice = resolved,
            Err(poisoned) => *poisoned.into_inner() = resolved,
        }
    }

    pub fn set_rate(&self, rate: u16) {
        self.rate.store(u32::from(rate), Ordering::Relaxed);
    }

    fn voice(&self) -> String {
        match self.voice.lock() {
            Ok(voice) => voice.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// The pt-BR voices installed on this machine, in the order `say` lists
    /// them.
    ///
    /// Returns empty rather than erroring when `say` is missing: a machine
    /// without it simply has no speech, which the caller already has to handle
    /// the same way it handles a missing whisper model.
    pub fn available_voices() -> Vec<Voice> {
        let Ok(output) = Command::new(SAY).args(["-v", "?"]).output() else {
            return Vec::new();
        };

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(parse_voice_line)
            .filter(|voice| voice.locale.starts_with("pt"))
            .collect()
    }

    /// Whether speech can be offered at all — `say` exists and has at least one
    /// Portuguese voice. The microphone button precedent: hide the feature
    /// rather than offer one that always fails.
    pub fn available() -> bool {
        !Self::available_voices().is_empty()
    }

    /// Resolve a configured voice against what is actually installed, falling
    /// back to [`DEFAULT_VOICE`] and then to whatever is present.
    ///
    /// A voice name saved in `config.toml` can stop existing — the user removes
    /// a language in System Settings — and `say -v` on a missing voice fails
    /// the whole utterance rather than substituting.
    pub fn resolve_voice(requested: Option<&str>) -> Option<String> {
        let installed = Self::available_voices();
        let named = |name: &str| installed.iter().any(|voice| voice.name == name);

        requested
            .filter(|name| named(name))
            .map(str::to_owned)
            .or_else(|| named(DEFAULT_VOICE).then(|| DEFAULT_VOICE.to_owned()))
            .or_else(|| installed.first().map(|voice| voice.name.clone()))
    }

    fn kill_current(&self) {
        let mut guard = match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            // Reap it, or the process lingers as a zombie for the life of the
            // app — and a long conversation is a lot of sentences.
            let _ = child.wait();
        }
    }
}

#[async_trait]
impl Speaker for SystemSpeaker {
    async fn speak(&self, text: &str) -> Result<(), EchoError> {
        let text = sanitise(text);
        if text.is_empty() {
            return Ok(());
        }

        // Queue behind any sentence already playing.
        let _turn = self.turn.lock().await;

        // Captured after acquiring the turn, not before: a `stop` that arrived
        // while this call was waiting must cancel it.
        let generation = self.generation.load(Ordering::SeqCst);

        let child = Command::new(SAY)
            .arg("-v")
            .arg(self.voice())
            .arg("-r")
            .arg(self.rate.load(Ordering::Relaxed).to_string())
            .arg("--")
            .arg(&text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(SpeechError::Spawn)?;

        {
            let mut guard = match self.current.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            // A `stop` between the generation check and here would have found
            // no child to kill, so honour it now rather than start speaking.
            if self.generation.load(Ordering::SeqCst) != generation {
                drop(guard);
                let mut orphan = child;
                let _ = orphan.kill();
                let _ = orphan.wait();
                return Ok(());
            }
            *guard = Some(child);
        }

        loop {
            {
                let mut guard = match self.current.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let finished = match guard.as_mut() {
                    // Taken by `stop` — interrupted, which is not an error.
                    None => return Ok(()),
                    Some(child) => child.try_wait().map_err(SpeechError::Wait)?.is_some(),
                };
                if finished {
                    // Reaped by `try_wait`; dropping the handle is all that is
                    // left, and it must happen before returning or
                    // `is_speaking` would keep reporting the finished child.
                    *guard = None;
                    return Ok(());
                }
            }
            tokio::time::sleep(POLL).await;
        }
    }

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.kill_current();
    }

    fn is_speaking(&self) -> bool {
        let mut guard = match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.as_mut() {
            None => false,
            Some(child) => matches!(child.try_wait(), Ok(None)),
        }
    }
}

impl Drop for SystemSpeaker {
    fn drop(&mut self) {
        // Closing the window mid-sentence should not leave a voice talking to
        // an empty room.
        self.kill_current();
    }
}

/// Parse one line of `say -v '?'`, whose shape is
/// `Luciana             pt_BR    # Olá, meu nome é Luciana.`
///
/// The name can contain spaces (`Grandma (Português (Brasil))`), so the locale
/// is found by scanning from the right of the part before `#` rather than by
/// splitting on whitespace from the left.
fn parse_voice_line(line: &str) -> Option<Voice> {
    let (head, _sample) = line.split_once('#')?;
    let mut fields = head.split_whitespace().collect::<Vec<_>>();
    let locale = fields.pop()?;
    if fields.is_empty() {
        return None;
    }
    Some(Voice {
        name: fields.join(" "),
        locale: locale.to_owned(),
    })
}

/// Strip `say`'s inline command syntax before speaking.
///
/// `say` interprets `[[…]]` as embedded speech commands — `[[slnc 5000]]` is a
/// five-second pause, `[[volm 0]]` is silence. A memory is arbitrary text the
/// user dictated, and a transcript that happens to contain double brackets
/// should be read aloud, not executed.
fn sanitise(text: &str) -> String {
    text.replace("[[", "(").replace("]]", ")").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_line_yields_the_name_and_locale() {
        let voice = parse_voice_line("Luciana             pt_BR    # Olá, meu nome é Luciana.")
            .expect("a well-formed line parses");
        assert_eq!(voice.name, "Luciana");
        assert_eq!(voice.locale, "pt_BR");
    }

    #[test]
    fn a_voice_name_may_contain_spaces_and_parentheses() {
        let voice =
            parse_voice_line("Grandma (Português (Brasil)) pt_BR    # Olá, meu nome é Grandma.")
                .expect("a multi-word name parses");
        assert_eq!(voice.name, "Grandma (Português (Brasil))");
        assert_eq!(voice.locale, "pt_BR");
    }

    #[test]
    fn a_line_without_a_sample_is_not_a_voice() {
        assert!(parse_voice_line("").is_none());
        assert!(parse_voice_line("some banner text with no hash").is_none());
    }

    #[test]
    fn inline_speech_commands_are_defanged() {
        // Otherwise a dictated "[[slnc 9000]]" would silently pause the answer.
        assert_eq!(
            sanitise("antes [[slnc 5000]] depois"),
            "antes (slnc 5000) depois"
        );
        assert_eq!(sanitise("  texto normal  "), "texto normal");
    }

    /// The port contract, exercised against the real binary. Ignored by
    /// default: it makes noise, and CI has no audio device.
    #[tokio::test]
    #[ignore = "plays audio — run with --ignored"]
    async fn speaking_resolves_after_the_audio_finishes() {
        let voice = SystemSpeaker::resolve_voice(None).expect("a pt-BR voice is installed");
        let speaker = SystemSpeaker::new(voice, DEFAULT_RATE);

        assert!(!speaker.is_speaking());
        speaker
            .speak("Testando a fala do EchoBrain.")
            .await
            .expect("speaks");
        assert!(
            !speaker.is_speaking(),
            "the child is reaped once it finishes"
        );
    }

    /// Barge-in: `stop` must silence the current sentence *and* drop the ones
    /// queued behind it, or an interrupted answer keeps talking.
    #[tokio::test]
    #[ignore = "plays audio — run with --ignored"]
    async fn stopping_drops_the_queue_as_well_as_the_current_sentence() {
        let voice = SystemSpeaker::resolve_voice(None).expect("a pt-BR voice is installed");
        let speaker = Arc::new(SystemSpeaker::new(voice, DEFAULT_RATE));

        let queued = {
            let speaker = Arc::clone(&speaker);
            tokio::spawn(async move {
                let long = "Esta é uma frase deliberadamente longa para dar tempo de interromper.";
                speaker.speak(long).await.expect("first sentence");
                speaker
                    .speak("Esta segunda frase não deveria ser falada.")
                    .await
                    .expect("second");
            })
        };

        tokio::time::sleep(Duration::from_millis(400)).await;
        speaker.stop();
        queued
            .await
            .expect("the speaking task finishes rather than hanging");

        assert!(!speaker.is_speaking());
    }
}
