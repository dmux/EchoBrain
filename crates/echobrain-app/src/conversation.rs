//! Driving the conversation machine with a real microphone.
//!
//! The decisions are in [`echobrain_core::Turn`], which has no I/O and is
//! tested without one. What is here is the wiring: a thread that owns the frame
//! stream and the detector, a task that runs the slow parts, and the events the
//! frontend draws.
//!
//! # Why two threads rather than one loop
//!
//! Frames arrive on a `std` channel every 20 ms and must be consumed steadily —
//! the sender is a realtime audio callback that drops rather than waits. The
//! work a turn triggers takes seconds: whisper, a vector search, a model, a
//! voice. Doing both in one place means either blocking the runtime or missing
//! audio, and missing audio is invisible until someone's first word is gone.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use echobrain_audio::{Endpointer, to_whisper_input};
use echobrain_core::{
    Action, AnswerRequest, AnswerStyle, Discard, Endpoint, Endpointing, Event, Frame, Phase,
    Samples, Turn, UnknownVoice,
};
use tauri::{AppHandle, Emitter, Runtime};

use crate::commands::AppState;

/// The event the frontend listens for, mirroring `hotkey.rs`.
pub const STATE_EVENT: &str = "echobrain://conversation-state";

/// How much audio from before the detector fired is kept.
///
/// The endpointer needs ~50 ms of voiced frames before it will say so, and the
/// detector looks back over a window of its own. Without a pre-roll every
/// utterance starts after the first phoneme, and "grava" reaches whisper as
/// "ava".
const PRE_ROLL: Duration = Duration::from_millis(400);

/// Cap on one utterance, so a detector that latches on cannot buffer the
/// afternoon into memory.
const MAX_UTTERANCE: Duration = Duration::from_secs(60);

/// Below this an "utterance" is a door, a keyboard or a breath. Sending it to
/// whisper wastes a second of Metal and reliably produces a confident
/// hallucination — the failure `verify-audio` exists to remember.
const MIN_UTTERANCE: Duration = Duration::from_millis(250);

/// What the audio thread reports upward.
enum Heard {
    Started,
    /// An utterance, already resampled for the recogniser.
    Ended(Box<Samples>),
    /// Speech ended but produced nothing usable.
    Discarded,
}

/// What the frontend is shown.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    pub phase: String,
    /// Reported honestly, including while speaking: barge-in only works because
    /// the microphone never closed, and a UI that showed it shut would be
    /// describing a microphone that is on.
    pub microphone_open: bool,
    /// The last thing understood, so the UI can show what it heard while the
    /// answer is still being composed.
    pub heard: Option<String>,
    pub error: Option<String>,
}

/// A running conversation. Dropping or stopping it closes the microphone.
#[derive(Debug)]
pub struct Conversation {
    running: Arc<AtomicBool>,
}

impl Conversation {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start a conversation.
pub fn start<R: Runtime>(
    app: AppHandle<R>,
    state: Arc<AppState>,
    hangover: Duration,
    follow_up: Duration,
) -> Result<Conversation, echobrain_core::EchoError> {
    let Some(monitor) = state.monitor.clone() else {
        return Err(echobrain_core::EchoError::audio(std::io::Error::other(
            "este dispositivo não entrega áudio contínuo",
        )));
    };

    // Subscribing before opening: frames delivered between the two calls would
    // otherwise be dropped, and those are the start of the first word.
    let frames = monitor.frames();
    tauri::async_runtime::block_on(monitor.start_monitoring())?;

    let running = Arc::new(AtomicBool::new(true));
    let (heard_tx, heard_rx) = tokio::sync::mpsc::unbounded_channel::<Heard>();

    spawn_audio_thread(frames, heard_tx, Arc::clone(&running), hangover);
    spawn_turn_task(app, state, heard_rx, Arc::clone(&running), follow_up);

    Ok(Conversation { running })
}

/// Detect boundaries and accumulate utterances. Nothing slow happens here.
fn spawn_audio_thread(
    frames: std::sync::mpsc::Receiver<Frame>,
    heard: tokio::sync::mpsc::UnboundedSender<Heard>,
    running: Arc<AtomicBool>,
    hangover: Duration,
) {
    std::thread::Builder::new()
        .name("echobrain-conversation".into())
        .spawn(move || {
            let mut endpointer = Endpointer::new(hangover);
            let mut utterance: Vec<f32> = Vec::new();
            let mut collecting = false;

            // Frames from before the detector fired.
            let mut pre_roll: std::collections::VecDeque<f32> = std::collections::VecDeque::new();

            while running.load(Ordering::Relaxed) {
                // A timeout rather than a blocking receive: it is how this
                // notices `running` went false with no frames arriving.
                let frame = match frames.recv_timeout(Duration::from_millis(200)) {
                    Ok(frame) => frame,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };

                let rate = frame.sample_rate;
                if rate == 0 {
                    continue;
                }
                let pre_roll_limit = (PRE_ROLL.as_secs_f32() * rate as f32) as usize;
                let max_samples = (MAX_UTTERANCE.as_secs_f32() * rate as f32) as usize;

                if collecting {
                    utterance.extend_from_slice(&frame.samples);
                    if utterance.len() > max_samples {
                        collecting = false;
                        endpointer.reset();
                        let _ = heard.send(prepare(std::mem::take(&mut utterance), rate));
                    }
                } else {
                    pre_roll.extend(frame.samples.iter().copied());
                    while pre_roll.len() > pre_roll_limit {
                        pre_roll.pop_front();
                    }
                }

                for endpoint in endpointer.push(&frame) {
                    match endpoint {
                        Endpoint::SpeechStarted => {
                            collecting = true;
                            utterance.clear();
                            utterance.extend(pre_roll.drain(..));
                            let _ = heard.send(Heard::Started);
                        }
                        Endpoint::SpeechEnded => {
                            collecting = false;
                            let _ = heard.send(prepare(std::mem::take(&mut utterance), rate));
                        }
                    }
                }
            }
        })
        .ok();
}

/// Turn accumulated device-rate audio into what the recogniser accepts.
fn prepare(samples: Vec<f32>, rate: u32) -> Heard {
    if rate == 0 {
        return Heard::Discarded;
    }
    let seconds = samples.len() as f32 / rate as f32;
    if seconds < MIN_UTTERANCE.as_secs_f32() {
        return Heard::Discarded;
    }
    // Already mono — the monitor averages channels in the audio callback.
    match to_whisper_input(&samples, rate, 1) {
        Ok(prepared) => Heard::Ended(Box::new(prepared)),
        Err(error) => {
            tracing::warn!(%error, "an utterance could not be prepared for the recogniser");
            Heard::Discarded
        }
    }
}

/// Run the machine and everything slow it asks for.
fn spawn_turn_task<R: Runtime>(
    app: AppHandle<R>,
    state: Arc<AppState>,
    mut heard: tokio::sync::mpsc::UnboundedReceiver<Heard>,
    running: Arc<AtomicBool>,
    follow_up: Duration,
) {
    tauri::async_runtime::spawn(async move {
        let mut turn = Turn::new(follow_up);
        let mut deadline: Option<tokio::time::Instant> = None;
        let mut last_heard: Option<String> = None;
        let mut last_error: Option<String> = None;

        drive(&app, &state, &mut turn, Event::Opened, &mut deadline).await;
        publish(&app, &turn, &last_heard, &last_error);

        while running.load(Ordering::Relaxed) {
            let event = tokio::select! {
                incoming = heard.recv() => match incoming {
                    Some(Heard::Started) => Event::SpeechStarted,
                    Some(Heard::Discarded) => {
                        // Ended, and worth nothing. Both halves are reported so
                        // the machine leaves `Hearing`.
                        drive(&app, &state, &mut turn, Event::SpeechEnded, &mut deadline).await;
                        Event::NotUnderstood
                    }
                    Some(Heard::Ended(samples)) => {
                        drive(&app, &state, &mut turn, Event::SpeechEnded, &mut deadline).await;
                        last_error = None;
                        publish(&app, &turn, &last_heard, &last_error);
                        match understand(&state, *samples).await {
                            Ok(Some(event)) => {
                                if let Event::Understood { text, .. } = &event {
                                    last_heard = Some(text.clone());
                                }
                                event
                            }
                            Ok(None) => Event::NotUnderstood,
                            Err(message) => {
                                last_error = Some(message.clone());
                                Event::Failed { message }
                            }
                        }
                    }
                    None => break,
                },
                () = sleep_until(deadline) => {
                    deadline = None;
                    Event::FollowUpExpired
                }
            };

            drive(&app, &state, &mut turn, event, &mut deadline).await;
            publish(&app, &turn, &last_heard, &last_error);

            if turn.phase() == Phase::Idle {
                break;
            }
        }

        if let Some(monitor) = &state.monitor {
            monitor.stop_monitoring();
        }
        if let Some(speaker) = &state.speaker {
            speaker.stop();
        }
        running.store(false, Ordering::Relaxed);

        turn = Turn::new(follow_up);
        publish(&app, &turn, &None, &None);
    });
}

/// Feed one event and run everything it asks for, including the events those
/// actions produce in turn.
async fn drive<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<AppState>,
    turn: &mut Turn,
    event: Event,
    deadline: &mut Option<tokio::time::Instant>,
) {
    let mut queue = turn.handle(event);

    // Executing an action produces the next event — asking produces `Handled`,
    // speaking produces `FinishedSpeaking` — so this drains rather than
    // iterating once.
    while !queue.is_empty() {
        let mut next = Vec::new();
        for action in std::mem::take(&mut queue) {
            if let Some(event) = perform(app, state, action, deadline).await {
                next.push(event);
            }
        }
        for event in next {
            queue.extend(turn.handle(event));
        }
    }
}

/// Carry out one action, returning whatever it caused.
async fn perform<R: Runtime>(
    _app: &AppHandle<R>,
    state: &Arc<AppState>,
    action: Action,
    deadline: &mut Option<tokio::time::Instant>,
) -> Option<Event> {
    match action {
        // The microphone is opened by `start` and closed by the task's exit;
        // the machine's view of it is what drives the UI.
        Action::StartListening => None,
        Action::StopListening => None,
        // Utterance boundaries are handled by the audio thread, which owns the
        // samples. The machine still models them so the phases are right.
        Action::BeginUtterance | Action::ProcessUtterance => None,

        Action::StopSpeaking => {
            if let Some(speaker) = &state.speaker {
                speaker.stop();
            }
            None
        }

        Action::Speak { text } => {
            match &state.speaker {
                Some(speaker) => {
                    if let Err(error) = speaker.speak(&text).await {
                        tracing::warn!(%error, "the turn could not be spoken");
                    }
                }
                // No voice installed: the text still reached the UI through the
                // state event, so the turn continues rather than stalling.
                None => tracing::info!("no voice installed; the answer was not spoken"),
            }
            Some(Event::FinishedSpeaking)
        }

        Action::ArmFollowUp { after } => {
            *deadline = Some(tokio::time::Instant::now() + after);
            None
        }

        Action::Capture { text } => Some(
            match state
                .aws()
                .ingest
                .execute(echobrain_core::IngestRequest {
                    text,
                    kind: None,
                    ttl: None,
                    github_repo: None,
                })
                .await
            {
                Ok(outcome) => Event::Handled {
                    spoken: Some(confirmation(outcome.entry.kind)),
                },
                Err(error) => Event::Failed {
                    message: format!("Não consegui guardar: {error}"),
                },
            },
        ),

        Action::Ask { text } => {
            let Some(answering) = state.aws().answer.clone() else {
                return Some(Event::Failed {
                    message: "As respostas por voz não estão configuradas.".into(),
                });
            };

            // Spoken sentence by sentence as the model produces them, so the
            // first word arrives seconds before the last.
            let floor = state
                .settings
                .read()
                .map(|settings| settings.min_similarity)
                .unwrap_or(echobrain_core::DEFAULT_MIN_SIMILARITY);

            let mut sink = Discard;
            Some(
                match answering
                    .execute(
                        AnswerRequest {
                            question: text,
                            min_similarity: Some(floor),
                            // Always spoken here: this whole module exists to
                            // hold a conversation out loud.
                            style: AnswerStyle::Spoken,
                        },
                        &mut sink,
                    )
                    .await
                {
                    Ok(answer) => Event::Handled {
                        spoken: Some(answer.text),
                    },
                    Err(error) => Event::Failed {
                        message: format!("Não consegui responder: {error}"),
                    },
                },
            )
        }
    }
}

/// Transcribe and route one utterance.
///
/// # Where the speaker filter goes, and where it does not
///
/// Here, before the recogniser, so a voice that is not the owner's costs no
/// whisper pass and cannot become a memory. This is the *continuous* path: the
/// microphone is open, the room is audible, and the television is otherwise a
/// prolific author.
///
/// It is **not** on the hotkey path, and that is not an oversight. Pressing
/// ⌥Space is an unambiguous statement of intent, and an app that refused to
/// obey it because a cold made someone sound different would be broken in the
/// least forgivable way. A filter that can only cost you a passing remark is
/// worth having; one that can refuse a deliberate command is not.
async fn understand(state: &Arc<AppState>, samples: Samples) -> Result<Option<Event>, String> {
    if !sounds_like_the_owner(state, &samples) {
        tracing::debug!("an utterance was dropped: it did not match the enrolled voice");
        return Ok(None);
    }

    let Some(transcriber) = state.transcriber.clone() else {
        return Err("O modelo de voz não está instalado.".into());
    };

    let transcript = match transcriber.transcribe(&samples).await {
        Ok(transcript) => transcript,
        // Silence is an ordinary outcome of a detector firing on a noise, not
        // an error worth showing.
        Err(echobrain_core::EchoError::EmptyUtterance) => return Ok(None),
        Err(error) => return Err(format!("Não consegui entender: {error}")),
    };

    let text = transcript.text.trim().to_string();
    if text.is_empty() {
        return Ok(None);
    }

    let intent = route(state, &text).await;
    Ok(Some(Event::Understood { text, intent }))
}

/// Cue first, model only for what the cue router declined.
///
/// The cue path is right whenever the speaker gave one and costs nothing, so it
/// keeps its priority. What changes is the ambiguous middle: on the hotkey it
/// falls to `Record`, which is safe because every hotkey utterance was
/// deliberate. Here the microphone is open and most of what it hears is
/// ambiguous, so that default was storing the room.
async fn route(state: &Arc<AppState>, text: &str) -> echobrain_core::Intent {
    if let Some(cue) = state.cue.cue(text) {
        return cue;
    }

    let Some(judge) = state.aws().intent_judge.clone() else {
        // No model configured: the app behaves exactly as it did before this
        // existed, which is a supported way to run and not a degraded one.
        return state.intent.classify(text);
    };

    match judge.judge(text).await {
        Ok(intent) => {
            tracing::debug!(%intent, "the model routed an utterance the cues could not");
            intent
        }
        // A router that is merely unreachable must not change what the app
        // does with speech, so this falls back to the documented default
        // rather than guessing in the other direction.
        Err(error) => {
            tracing::warn!(%error, "the intent judge failed; falling back to the cue default");
            state.intent.classify(text)
        }
    }
}

/// Whether this utterance passes the speaker filter.
///
/// True whenever the filter cannot apply — no model installed, no profile
/// enrolled — or when the user has turned it off. Both are normal states, and
/// in them the app behaves exactly as it did before this feature existed.
///
/// # Why enrolling is treated as asking for the filter
///
/// This used to default to allowing every voice even after enrolment, on the
/// grounds that filtering should be something the user turns on deliberately
/// after seeing their own score. In practice enrolment *is* that deliberate
/// act — nobody records three samples of their voice to then have the
/// television keep writing memories — and the setting that would have enabled
/// it was reachable only by hand-editing `config.toml`. So a profile now means
/// the filter applies, and the settings screen carries the switch to say
/// otherwise.
fn sounds_like_the_owner(state: &Arc<AppState>, samples: &Samples) -> bool {
    let settings = match state.settings.read() {
        Ok(settings) => settings.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    // Still `!= Ignore` rather than `== Allow`: a value neither variant
    // recognises must not silently start discarding speech.
    if settings.unknown_voice != UnknownVoice::Ignore.as_str() {
        return true;
    }

    let Some(identity) = &state.voice_identity else {
        return true;
    };
    let profile = match state.voice_profile.read() {
        Ok(profile) => profile.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let Some(profile) = profile else {
        return true;
    };

    match identity.embed(samples) {
        Ok(embedding) => {
            let score = profile.similarity(&embedding);
            tracing::debug!(score, threshold = settings.voice_threshold, "speaker check");
            profile.accepts(&embedding, settings.voice_threshold)
        }
        // A failing model must not silence the user. Erring towards hearing
        // them is the only safe direction: the cost is a stray memory, and the
        // alternative is an app that has stopped listening without saying so.
        Err(error) => {
            tracing::warn!(%error, "the speaker check failed; treating the voice as the owner's");
            true
        }
    }
}

/// What the app says after storing something.
///
/// # Why this is a function with a test rather than a `format!` in place
///
/// The microphone stays open while the app speaks — that is what makes
/// barge-in possible — so this sentence is heard back by the detector,
/// transcribed, and routed like any other utterance. It used to open with
/// "Guardado", which matches the `guarda` cue in `RECORD_COMMANDS`: the
/// confirmation was read as an order to store itself, stored, confirmed again,
/// and the app talked in a circle until it was stopped.
///
/// The speaker filter also breaks that loop, but only once a voice is enrolled
/// and the filter is on. Phrasing costs nothing and holds either way, so the
/// rule is worth stating and keeping: nothing the app says may open with a cue
/// the router acts on.
fn confirmation(kind: echobrain_core::MemoryKind) -> String {
    format!("Pronto — {}.", kind_in_portuguese(kind))
}

fn kind_in_portuguese(kind: echobrain_core::MemoryKind) -> &'static str {
    match kind {
        echobrain_core::MemoryKind::Fact => "fato",
        echobrain_core::MemoryKind::Preference => "preferência",
        echobrain_core::MemoryKind::Episode => "episódio",
        echobrain_core::MemoryKind::Snippet => "código",
    }
}

/// `None` never fires, which is how "no timer armed" is expressed in a
/// `select!` without inventing a second branch.
async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn publish<R: Runtime>(
    app: &AppHandle<R>,
    turn: &Turn,
    heard: &Option<String>,
    error: &Option<String>,
) {
    let payload = ConversationState {
        phase: turn.phase().as_str().to_string(),
        microphone_open: turn.phase().microphone_open(),
        heard: heard.clone(),
        error: error.clone(),
    };
    if let Err(error) = app.emit(STATE_EVENT, payload) {
        tracing::warn!(%error, "could not deliver the conversation state");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echobrain_core::{CueIntent, Intent, IntentClassifier, MemoryKind};

    /// The feedback loop, as a rule the code cannot drift back into.
    ///
    /// Every sentence the app speaks is heard back by its own open microphone.
    /// If one of them routes to `Record`, storing it produces another
    /// confirmation, which is stored in turn — the app converses with itself
    /// until the user stops it. This happened: "Guardado como episódio" opens
    /// with the `guarda` cue.
    #[test]
    fn nothing_the_app_says_fires_a_record_cue() {
        let router = CueIntent::new();

        for kind in [
            MemoryKind::Fact,
            MemoryKind::Preference,
            MemoryKind::Episode,
        ] {
            let spoken = confirmation(kind);
            assert_ne!(
                router.cue(&spoken),
                Some(Intent::Record),
                "the app says {spoken:?} and hears it back; matching a record cue \
                 makes storing it unconditional, and storing it says the same \
                 sentence again"
            );
        }
    }

    /// What removing the cue does and does not buy.
    ///
    /// It stops the confirmation from being an *unconditional* order to store.
    /// It does not make the sentence safe on its own: with no cue at all it
    /// reaches the ambiguous middle, where the cue router's default is still
    /// `Record`. Closing the loop needs one of the two layers above it — the
    /// speaker filter rejecting the app's own voice, or the model routing a
    /// confirmation to `Ask` — and this test exists so that dependency is
    /// written down rather than assumed.
    #[test]
    fn the_confirmation_still_needs_the_filter_or_the_model_behind_it() {
        assert_eq!(
            CueIntent::new().classify(&confirmation(MemoryKind::Episode)),
            Intent::Record,
            "the cue-only default has not changed, and is why the other two \
             layers are not optional"
        );
    }

    /// The phrasing that caused it, kept as the counter-example so the reason
    /// survives even if the wording changes again.
    #[test]
    fn the_old_confirmation_really_was_a_record_command() {
        assert_eq!(
            CueIntent::new().classify("Guardado como episódio."),
            Intent::Record
        );
    }
}
