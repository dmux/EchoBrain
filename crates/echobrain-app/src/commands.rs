//! The IPC surface. Four commands, matching `TauriGateway` in the frontend.
//!
//! Each one is a thin translation: parse the wire types, call a use case, map
//! the result back. No decisions are made here — a decision taken in this file
//! would be a decision no test can reach without a webview, and one the
//! `FakeGateway` in the frontend could not mirror.

use std::sync::Arc;
use std::time::Duration;

use echobrain_core::{
    AnswerRequest, AnswerStyle, AudioCapture, AudioMonitor, IngestRequest, IntentClassifier,
    Journal, MemoryId, MemoryKind, SearchRequest, SentenceSink, Speaker, TopK, Transcriber,
    UnknownVoice, VoiceIdentity, VoiceProfile,
};
use std::sync::{Mutex, RwLock};

use tauri::State;

use crate::config::{Config, VoiceConfig};
use crate::dto::{
    AnswerDto, CaptureResultDto, ConnectionDto, ConnectionSettingsDto, DictationDto, HitDto,
    IpcError, JournalEntryDto, MemoryDto, MemoryFilterDto, MemoryPageDto, UpdateMemoryDto,
    VoiceDto, VoiceProfileDto, VoiceSettingsDto,
};

/// Everything the commands need, wired once in `lib.rs`.
pub struct AppState {
    /// Everything that depends on which AWS account the app is talking to.
    ///
    /// Behind a lock because [`set_connection`] replaces it wholesale. Read it
    /// with [`AppState::aws`], which takes a snapshot: holding the guard across
    /// an `await` would let a save deadlock behind an in-flight search.
    pub aws: Arc<RwLock<Arc<crate::aws::AwsStack>>>,

    /// Outside [`crate::aws::AwsStack`] deliberately: it records what *this
    /// device* captured, so it has to survive every account change.
    pub journal: Arc<dyn Journal>,
    pub capture: Arc<dyn AudioCapture>,
    /// `None` when the speech model is not installed.
    ///
    /// Dictation is the one feature with a 570 MB prerequisite, so its absence
    /// must not stop the app starting or block typing. The UI asks
    /// [`dictation_available`] and hides the microphone rather than offering a
    /// button that always fails.
    pub transcriber: Option<Arc<dyn Transcriber>>,

    /// `None` when no Portuguese system voice is installed.
    ///
    /// Same precedent as the microphone: the feature disappears rather than
    /// failing loudly.
    pub speaker: Option<Arc<dyn Speaker>>,

    /// Routes an utterance between storing and asking. Always present — it is
    /// a heuristic with no prerequisites.
    pub intent: Arc<dyn IntentClassifier>,

    /// The same router, concretely, so the conversation loop can ask whether a
    /// cue fired at all instead of only receiving the default it applies when
    /// none did.
    pub cue: Arc<echobrain_core::CueIntent>,

    /// The microphone as a continuous stream, for conversation mode.
    ///
    /// The same device `capture` uses — one open stream serves both — but
    /// reached through the port that delivers frames rather than utterances.
    pub monitor: Option<Arc<dyn AudioMonitor>>,

    /// Measures how much an utterance sounds like the enrolled voice.
    ///
    /// `None` until the embedding model is installed. Absent is the normal
    /// state and everything keeps working: the filter simply never fires.
    pub voice_identity: Option<Arc<dyn VoiceIdentity>>,

    /// The enrolled voice, if one has been recorded.
    pub voice_profile: Arc<RwLock<Option<VoiceProfile>>>,

    /// The conversation in progress, if any.
    ///
    /// At most one: a second would open a second detector on the same frames
    /// and answer everything twice.
    pub conversation: Arc<Mutex<Option<crate::conversation::Conversation>>>,

    /// The concrete speaker, when there is one, so the settings screen can
    /// change the voice live. `speaker` above is the same object behind the
    /// port; this is the platform-specific handle the trait deliberately does
    /// not expose.
    pub system_speaker: Option<Arc<echobrain_audio::SystemSpeaker>>,
    pub polly_speaker: Arc<echobrain_audio::PollySpeaker>,

    /// The live `[voice]` settings. Read per question and per utterance, so a
    /// change here takes effect on the next one without a restart.
    pub settings: Arc<RwLock<VoiceConfig>>,

    /// The whole config, held so `set_settings` can write it back without
    /// re-reading and clobbering fields it does not own.
    pub config: Arc<RwLock<Config>>,
}

impl AppState {
    /// The AWS-backed services as they are *right now*.
    ///
    /// Returns a snapshot rather than a guard on purpose. Every caller is an
    /// `async fn` that then awaits a network call, and a guard held across that
    /// await would block [`set_connection`] for the length of a search — or, if
    /// the same task took it twice, deadlock outright. A snapshot means an
    /// operation already in flight finishes against the account it started on,
    /// which is also the only coherent answer.
    pub fn aws(&self) -> Arc<crate::aws::AwsStack> {
        Arc::clone(&read(&self.aws))
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

#[tauri::command]
pub async fn search_memories(
    state: State<'_, Arc<AppState>>,
    query: String,
    kind: Option<String>,
    top_k: Option<u32>,
    min_similarity: Option<f32>,
) -> Result<Vec<HitDto>, IpcError> {
    let request = SearchRequest {
        text: query,
        // Validated here rather than in the adapter so an out-of-range value is
        // rejected before it costs an embedding.
        top_k: match top_k {
            Some(requested) => TopK::new(requested).map_err(|error| IpcError {
                message: error.to_string(),
                invalid_input: true,
            })?,
            None => TopK::DEFAULT,
        },
        kind: parse_kind(kind)?,
        min_similarity,
    };

    let aws = state.aws();
    let hits = aws.search.execute(request).await?;

    let memory_ids: Vec<_> = hits
        .iter()
        .map(|hit| hit.memory.memory_id.clone())
        .collect();
    aws.reconciler.observed(&memory_ids).await;

    Ok(hits.into_iter().map(HitDto::from).collect())
}

#[tauri::command]
pub async fn capture_memory(
    state: State<'_, Arc<AppState>>,
    text: String,
    kind: Option<String>,
    ttl_seconds: Option<u64>,
) -> Result<CaptureResultDto, IpcError> {
    let outcome = state
        .aws()
        .ingest
        .execute(IngestRequest {
            text,
            kind: parse_kind(kind)?,
            ttl: ttl_seconds.map(Duration::from_secs),
            github_repo: None,
        })
        .await?;

    if let Some(reason) = &outcome.journal_failed {
        tracing::warn!(
            reason,
            memory_id = %outcome.entry.memory_id,
            "the memory was stored remotely but the local journal write failed"
        );
    }

    Ok(CaptureResultDto {
        entry: outcome.entry.into(),
        journal_failed: outcome.journal_failed,
    })
}

#[tauri::command]
pub async fn forget_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
) -> Result<(), IpcError> {
    let memory_id =
        MemoryId::new(memory_id).map_err(|error| IpcError::invalid(error.to_string()))?;

    // The placeholder is ignored by the server, which scopes the delete to the
    // namespace it derived from the signature. See `caller_placeholder`.
    let user_id = echobrain_core::caller_placeholder()
        .map_err(|error| IpcError::invalid(error.to_string()))?;

    state
        .aws()
        .memory
        .forget(&user_id, &memory_id)
        .await
        .map_err(echobrain_core::EchoError::from)?;

    // Tombstone only after the remote delete succeeded. The other order would
    // hide a memory locally that is still there and still searchable.
    if let Err(error) = state.journal.tombstone(&memory_id).await {
        tracing::warn!(%error, %memory_id, "deleted remotely but not tombstoned locally");
    }

    Ok(())
}

/// One page of the library, newest first.
///
/// Returns memories rather than hits: nothing here was scored, and a
/// `similarity` of 1.0 on every row — which is what this used to send — is a
/// number the UI could render as a full meter next to memories nobody searched
/// for.
#[tauri::command]
pub async fn list_memories(
    state: State<'_, Arc<AppState>>,
    limit: Option<u32>,
    cursor: Option<String>,
    filter: Option<MemoryFilterDto>,
) -> Result<MemoryPageDto, IpcError> {
    let user_id = echobrain_core::caller_placeholder()
        .map_err(|error| IpcError::invalid(error.to_string()))?;

    let limit = match limit {
        Some(value) => echobrain_core::PageSize::new(value)
            .map_err(|error| IpcError::invalid(error.to_string()))?,
        None => echobrain_core::PageSize::DEFAULT,
    };
    let cursor = cursor
        .map(echobrain_core::Cursor::new)
        .transpose()
        .map_err(|error| IpcError::invalid(error.to_string()))?;

    let page = state
        .aws()
        .memory
        .list(
            &user_id,
            &echobrain_core::ListQuery {
                limit,
                cursor,
                filter: filter.unwrap_or_default().try_into()?,
            },
        )
        .await
        .map_err(IpcError::from)?;

    Ok(MemoryPageDto {
        memories: page.memories.into_iter().map(MemoryDto::from).collect(),
        next_cursor: page.next.map(|cursor| cursor.to_string()),
    })
}

#[tauri::command]
pub async fn update_memory(
    state: State<'_, Arc<AppState>>,
    memory_id: String,
    updates: UpdateMemoryDto,
) -> Result<(), IpcError> {
    let memory_id =
        MemoryId::new(memory_id).map_err(|error| IpcError::invalid(error.to_string()))?;
    let user_id = echobrain_core::caller_placeholder()
        .map_err(|error| IpcError::invalid(error.to_string()))?;

    let kind = match updates.kind {
        Some(k) => Some(
            k.parse::<MemoryKind>()
                .map_err(|error| IpcError::invalid(error.to_string()))?,
        ),
        None => None,
    };

    state
        .aws()
        .memory
        .update(echobrain_core::UpdateCommand {
            user_id,
            memory_id,
            text: updates.text,
            kind,
            ttl: None,
            rating: updates.rating,
            active: updates.active,
        })
        .await
        .map_err(IpcError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn library(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
) -> Result<Vec<JournalEntryDto>, IpcError> {
    let entries = state.journal.recent(limit.unwrap_or(50)).await?;
    Ok(entries.into_iter().map(JournalEntryDto::from).collect())
}

/// Whether the microphone button should exist at all.
#[tauri::command]
pub fn dictation_available(state: State<'_, Arc<AppState>>) -> bool {
    state.transcriber.is_some()
}

/// Open the microphone and start accumulating audio.
///
/// The macOS permission prompt appears here, on the first call — not at launch,
/// which would ask before the user has expressed any interest in dictating.
#[tauri::command]
pub async fn start_dictation(state: State<'_, Arc<AppState>>) -> Result<(), IpcError> {
    if state.transcriber.is_none() {
        return Err(IpcError::invalid(
            "o modelo de voz não está instalado — rode `make model`",
        ));
    }

    // Barge-in. Opening the microphone means the user is talking, so whatever
    // the app was saying stops now — otherwise it talks over the very
    // interruption it was being interrupted by. Here rather than in the
    // frontend because it must hold however the microphone was opened: the
    // button, the hotkey, or the conversation loop.
    if let Some(speaker) = &state.speaker {
        speaker.stop();
    }

    state.capture.start().await?;
    Ok(())
}

/// Stop recording and transcribe what was said.
///
/// Returns the text rather than storing it. Transcription is fallible and the
/// kind is a guess, so the transcript lands in the capture field for the user
/// to glance at and confirm — one keystroke, and no silent wrong memories.
#[tauri::command]
pub async fn stop_dictation(state: State<'_, Arc<AppState>>) -> Result<DictationDto, IpcError> {
    let Some(transcriber) = state.transcriber.clone() else {
        return Err(IpcError::invalid(
            "o modelo de voz não está instalado — rode `make model`",
        ));
    };

    let samples = state.capture.stop().await?;
    let seconds = samples.duration().as_secs_f32();
    let transcript = transcriber.transcribe(&samples).await?;

    Ok(DictationDto {
        text: transcript.text,
        language: transcript.language,
        seconds,
    })
}

/// Stop recording, skip transcription, and measure the voice identity.
///
/// Used during enrollment to collect embeddings for a new VoiceProfile.
#[tauri::command]
pub async fn stop_and_measure_voice(state: State<'_, Arc<AppState>>) -> Result<Vec<f32>, IpcError> {
    let Some(identity) = state.voice_identity.clone() else {
        return Err(IpcError::invalid(
            "o modelo de identidade de voz não está instalado — rode `make voice-model`",
        ));
    };

    let samples = state.capture.stop().await?;
    let embedding = identity.embed(&samples)?;
    Ok(embedding.as_slice().to_vec()) // Return the Vec<f32> directly
}

#[tauri::command]
pub fn save_voice_profile(
    state: State<'_, Arc<AppState>>,
    samples: Vec<Vec<f32>>,
) -> Result<VoiceProfileDto, IpcError> {
    if samples.len() < echobrain_core::VoiceProfile::MIN_ENROLMENT {
        return Err(IpcError::invalid(format!(
            "São necessárias pelo menos {} amostras.",
            echobrain_core::VoiceProfile::MIN_ENROLMENT
        )));
    }

    let mut embeddings: Vec<echobrain_core::VoiceEmbedding> = Vec::with_capacity(samples.len());
    for s in samples {
        let e = echobrain_core::VoiceEmbedding::new(s)
            .ok_or_else(|| IpcError::invalid("Amostra inválida (vazia ou silenciosa)"))?;
        embeddings.push(e);
    }

    let profile = echobrain_core::VoiceProfile::enrol(&embeddings, std::time::SystemTime::now())
        .map_err(|e| IpcError::invalid(e.to_string()))?;

    // Save to disk
    Config::save_voice_profile(&profile).map_err(|e| IpcError::invalid(e.to_string()))?;

    // Update state
    match state.voice_profile.write() {
        Ok(mut p) => *p = Some(profile.clone()),
        Err(poisoned) => *poisoned.into_inner() = Some(profile.clone()),
    }

    // Enrolling turns the filter on. Recording three samples of your own voice
    // is already the deliberate act the old default was waiting for, and until
    // now the only way to act on it was to hand-edit `config.toml` — so the
    // feature existed, the user had used it, and every other voice in the room
    // still became a memory. The settings screen carries the switch to undo
    // this; the enrolment screen says what it does.
    {
        let mut settings = write(&state.settings);
        settings.unknown_voice = echobrain_core::UnknownVoice::Ignore.as_str().to_string();
        let voice = settings.clone();
        drop(settings);

        let config = {
            let mut config = write(&state.config);
            config.voice = voice;
            config.clone()
        };
        if let Err(error) = config.save() {
            // The profile is already saved and in effect; failing to persist
            // the switch is worth reporting, not worth discarding enrolment.
            tracing::warn!(%error, "enrolled, but the speaker filter setting was not written");
        }
    }

    Ok(VoiceProfileDto::from(profile))
}

/// Current input level, `0.0..=1.0`. Polled at frame rate to drive the
/// waveform, so it is deliberately cheap and non-async.
#[tauri::command]
pub fn audio_level(state: State<'_, Arc<AppState>>) -> f32 {
    state.capture.level()
}

/// Whether conversation mode can be offered.
///
/// It needs the recogniser and the continuous microphone. Speech and answering
/// are *not* required: without a voice the turn is shown rather than spoken,
/// and without a model it still captures.
#[tauri::command]
pub fn conversation_available(state: State<'_, Arc<AppState>>) -> bool {
    state.transcriber.is_some() && state.monitor.is_some()
}

/// Open the microphone and keep it open, taking turns until told to stop.
///
/// The microphone light stays on for the whole conversation. That is the cost,
/// it is visible, and it is why this is a mode the user turns on rather than
/// something the app does on its own.
#[tauri::command]
pub fn start_conversation<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), IpcError> {
    if state.transcriber.is_none() {
        return Err(IpcError::invalid(
            "o modelo de voz não está instalado — rode `make model`",
        ));
    }

    let mut running = lock(&state.conversation);
    if running.as_ref().is_some_and(|c| c.is_running()) {
        // Idempotent, like the hotkey: asking twice is not an error and must
        // not open a second detector on the same frames.
        return Ok(());
    }

    let settings = read(&state.settings).clone();
    let started = crate::conversation::start(
        app,
        Arc::clone(state.inner()),
        Duration::from_millis(u64::from(settings.hangover_ms)),
        Duration::from_secs(u64::from(settings.follow_up_seconds)),
    )?;
    *running = Some(started);
    Ok(())
}

/// End the conversation and close the microphone.
#[tauri::command]
pub fn stop_conversation(state: State<'_, Arc<AppState>>) {
    if let Some(conversation) = lock(&state.conversation).take() {
        conversation.stop();
    }
    if let Some(speaker) = &state.speaker {
        speaker.stop();
    }
    if let Some(monitor) = &state.monitor {
        monitor.stop_monitoring();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a question can be answered at all — a model is configured and the
/// crate that reaches it was built in.
#[tauri::command]
pub fn answering_available(state: State<'_, Arc<AppState>>) -> bool {
    state.aws().answer.is_some()
}

/// Whether answers can be spoken.
#[tauri::command]
pub fn speech_available(state: State<'_, Arc<AppState>>) -> bool {
    state.speaker.is_some()
}

/// Route an utterance between storing and asking.
///
/// Exposed separately from [`ask_memories`] so the UI can show the guess as an
/// editable chip *before* acting on it — the same treatment the memory kind
/// gets, for the same reason: it is a heuristic and it will be wrong.
#[tauri::command]
pub fn classify_intent(state: State<'_, Arc<AppState>>, text: String) -> String {
    state.intent.classify(&text).as_str().to_string()
}

/// Answer a question from the stored memories.
///
/// # Why this returns before the speaking finishes
///
/// Sentences are handed to the voice as the model produces them, so speech
/// starts a second or so in. Awaiting the audio here would hold the IPC call
/// open for the length of the answer and delay the text — the user would hear
/// the answer before seeing it, and see nothing at all until the voice stopped.
/// The speaking task therefore outlives the command, ordered by the speaker's
/// own queue.
#[tauri::command]
pub async fn ask_memories(
    state: State<'_, Arc<AppState>>,
    question: String,
    speak: Option<bool>,
    min_similarity: Option<f32>,
) -> Result<AnswerDto, IpcError> {
    let Some(answering) = state.aws().answer.clone() else {
        return Err(IpcError::invalid(
            "as respostas por voz não estão configuradas — defina `answer_model` nas configurações",
        ));
    };

    // Whether to speak is the frontend's call: it is the only side that knows
    // whether the question was typed or dictated.
    let speaker = speak
        .unwrap_or(false)
        .then(|| state.speaker.clone())
        .flatten();

    // Barge-in: a new question silences the previous answer immediately, before
    // any token has arrived.
    if let Some(speaker) = &speaker {
        speaker.stop();
    }

    let (sentences, mut queue) = tokio::sync::mpsc::unbounded_channel::<String>();
    let speaking = speaker.map(|speaker| {
        tokio::spawn(async move {
            while let Some(sentence) = queue.recv().await {
                if let Err(error) = speaker.speak(&sentence).await {
                    tracing::warn!(%error, "the answer could not be spoken");
                    break;
                }
            }
        })
    });

    // The floor comes from the live settings unless the caller overrode it, so
    // moving the slider takes effect on the next question rather than the next
    // launch.
    let floor = min_similarity.or_else(|| Some(read(&state.settings).min_similarity));

    // Spoken or written follows the same flag that decides whether to speak,
    // because it answers the same question: a dictated question is heard, and a
    // typed one is read. See `AnswerStyle` for why the caller has to say.
    let style = if speaking.is_some() {
        AnswerStyle::Spoken
    } else {
        AnswerStyle::Written
    };

    let mut sink = ChannelSink { sentences };
    let answer = answering
        .execute(
            AnswerRequest {
                question,
                min_similarity: floor,
                style,
            },
            &mut sink,
        )
        .await;

    // Closing the channel is what tells the speaking task there is no more to
    // say; without this it would wait forever and the task would leak.
    drop(sink);

    match answer {
        Ok(answer) => {
            let aws = state.aws();
            let memory_ids: Vec<_> = answer
                .cited
                .iter()
                .map(|hit| hit.memory.memory_id.clone())
                .collect();
            aws.reconciler.observed(&memory_ids).await;
            Ok(answer.into())
        }
        Err(error) => {
            // The question failed partway. Stop mid-sentence rather than let
            // the voice finish reading half an answer as though it were whole.
            if let Some(speaking) = speaking {
                speaking.abort();
            }
            Err(error.into())
        }
    }
}

/// Speak arbitrary text. Used by the settings screen's *Testar voz*, and by the
/// UI to read a memory back on demand.
#[tauri::command]
pub async fn speak(state: State<'_, Arc<AppState>>, text: String) -> Result<(), IpcError> {
    let Some(speaker) = state.speaker.clone() else {
        return Err(IpcError::invalid(
            "nenhuma voz em português está instalada neste Mac",
        ));
    };

    // Logged on both sides. Speaking is the one command whose success is
    // audible and whose failure is silent: nothing comes out of the speakers
    // either way, so without this line "nothing happened" is the entire bug
    // report, and the engine and voice that produced it are already gone.
    let (engine, voice) = {
        let settings = read(&state.settings);
        (
            settings
                .tts_engine
                .clone()
                .unwrap_or_else(|| "system".into()),
            settings
                .tts_voice
                .clone()
                .unwrap_or_else(|| "<default>".into()),
        )
    };
    tracing::info!(%engine, %voice, characters = text.chars().count(), "speaking");

    match speaker.speak(&text).await {
        Ok(()) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, %engine, %voice, "the voice failed");
            Err(error.into())
        }
    }
}

/// Silence whatever is being said. Safe when nothing is.
#[tauri::command]
pub fn stop_speaking(state: State<'_, Arc<AppState>>) {
    if let Some(speaker) = &state.speaker {
        speaker.stop();
    }
}

/// The pt-BR voices this Mac has installed, for the settings screen.
#[tauri::command]
pub fn available_voices() -> Vec<VoiceDto> {
    let mut voices: Vec<VoiceDto> = echobrain_audio::SystemSpeaker::available_voices()
        .into_iter()
        .map(|voice| VoiceDto {
            name: voice.name,
            locale: voice.locale,
            engine: "system".into(),
        })
        .collect();

    let polly_voices = vec![
        "Vitoria",
        "Camila",
        "Ricardo",
        "Thiago",
        "Ines",
        "Cristiano",
    ];
    for name in polly_voices {
        voices.push(VoiceDto {
            name: name.into(),
            locale: "pt-BR".into(),
            engine: "polly".into(),
        });
    }

    // The last unlogged command on the "Testar voz" path, and the one that
    // decides whether the button is pressable at all: the screen disables it on
    // `voices.length === 0`. A disabled button runs no handler, so every other
    // log on this path stays silent and the failure looks like nothing at all.
    tracing::debug!(count = voices.len(), "voices listed");

    voices
}

/// The current `[voice]` settings.
#[tauri::command]
pub fn get_settings(state: State<'_, Arc<AppState>>) -> VoiceSettingsDto {
    // Logged because the settings screen renders nothing at all until this
    // answers: if it never arrives, the dialog is a spinner and none of its
    // buttons exist to be clicked.
    tracing::debug!("settings read");
    VoiceSettingsDto::from(&*read(&state.settings))
}

/// Rebuild the AWS-backed services from `config` and swap them in.
///
/// The swap is the last thing that happens, and it replaces the whole
/// [`crate::aws::AwsStack`] at once: there is no window in which the app
/// searches against one account and speaks through another.
///
/// Polly is updated separately because [`echobrain_audio::PollySpeaker`] is
/// held outside the stack — the routing speaker and the conversation loop keep
/// references to that one object — so it takes the new client rather than being
/// replaced.
async fn rebuild_aws(state: &Arc<AppState>, config: &Config) {
    let stack = crate::aws::build(config, Arc::clone(&state.journal)).await;
    state.polly_speaker.set_client(stack.polly_client.clone());
    *write(&state.aws) = Arc::new(stack);
    tracing::info!(
        region = %config.region,
        profile = config.profile.as_deref().unwrap_or("<default chain>"),
        "the AWS clients were rebuilt without a restart"
    );
}

/// Save the `[voice]` settings and apply them.
///
/// # What takes effect when
///
/// Voice, rate and the similarity floor are read per utterance and per question,
/// so they change on the next one. `answer_model` and `answer_max_tokens` are
/// baked into a Bedrock client, so changing either rebuilds the AWS stack — the
/// same mechanism the connection screen uses. Nothing here needs a restart.
///
/// The rebuild is conditional because it resolves credentials, and doing that on
/// every rate-slider drag would put a filesystem read behind a control that is
/// dragged continuously.
#[tauri::command]
pub async fn set_settings(
    state: State<'_, Arc<AppState>>,
    settings: VoiceSettingsDto,
) -> Result<VoiceSettingsDto, IpcError> {
    // "Testar voz" calls this before it speaks, so this line is the bisect
    // point: reached but no `speaking` after it means the save failed, and
    // absent entirely means the button never ran.
    tracing::debug!(
        engine = ?settings.tts_engine,
        voice = ?settings.tts_voice,
        "settings write"
    );

    let (applied, answering_changed) = {
        let mut current = write(&state.settings);
        let was = (
            current.answer_model.clone(),
            current.answer_max_tokens,
            current.min_similarity,
        );
        current.tts_engine = settings.tts_engine.clone().filter(|v| !v.trim().is_empty());
        current.tts_voice = settings.tts_voice.clone().filter(|v| !v.trim().is_empty());
        current.tts_rate = settings.tts_rate.clamp(90, 400);
        current.answer_model = settings
            .answer_model
            .clone()
            .filter(|m| !m.trim().is_empty());
        current.answer_max_tokens = settings.answer_max_tokens.clamp(256, 8_192);
        current.min_similarity = settings.min_similarity.clamp(0.0, 1.0);
        current.voice_threshold = settings.voice_threshold.clamp(0.0, 1.0);
        // Anything the domain does not recognise becomes `allow`, so a bad
        // value can never start silently discarding the user's speech.
        current.unknown_voice = if settings.unknown_voice == UnknownVoice::Ignore.as_str() {
            UnknownVoice::Ignore
        } else {
            UnknownVoice::Allow
        }
        .as_str()
        .to_string();
        let changed = was
            != (
                current.answer_model.clone(),
                current.answer_max_tokens,
                current.min_similarity,
            );
        (current.clone(), changed)
    };

    if let Some(speaker) = &state.system_speaker {
        speaker.set_voice(applied.tts_voice.as_deref());
        speaker.set_rate(applied.tts_rate);
    }
    state.polly_speaker.set_voice(applied.tts_voice.as_deref());

    // Persisted last: an unwritable disk should not silently leave the running
    // app configured differently from what the user was told was saved — but it
    // also must not lose the change they just made, so the error is reported
    // with the applied values already in effect.
    let config = {
        let mut config = write(&state.config);
        config.voice = applied.clone();
        config.clone()
    };

    // Before persisting, for the same reason `set_connection` builds first: a
    // rebuild that panics must not leave a file on disk describing a state the
    // running app never reached.
    if answering_changed {
        rebuild_aws(&state, &config).await;
    }

    // "Testar voz" saves before it speaks, so a failure here is reported to the
    // user as a voice that does not work. Naming it in the log keeps the two
    // apart.
    if let Err(error) = config.save() {
        tracing::warn!(%error, "the settings could not be written to disk");
        return Err(IpcError::invalid(error.to_string()));
    }

    Ok(VoiceSettingsDto::from(&applied))
}

/// The connection settings, plus the machine facts the screen renders with them.
#[tauri::command]
pub fn get_connection(state: State<'_, Arc<AppState>>) -> ConnectionDto {
    tracing::debug!("connection read");
    ConnectionDto::from(&*read(&state.config))
}

/// Point the app at a different API, region or AWS profile, without a restart.
///
/// # What a successful return does and does not mean
///
/// It means the settings are well-formed and every AWS client in the process has
/// been rebuilt against them. It does **not** mean the account is reachable:
/// resolving a credential chain is a local operation, so a profile that does not
/// exist, or one whose SSO session has expired, fails on the next search rather
/// than here. Proving otherwise would mean spending an embedding on every save.
/// The SSO button is the remedy; see [`login_sso`].
#[tauri::command]
pub async fn set_connection(
    state: State<'_, Arc<AppState>>,
    connection: ConnectionSettingsDto,
) -> Result<ConnectionDto, IpcError> {
    let endpoint = connection.endpoint.trim().to_string();
    let region = connection.region.trim().to_string();

    // Validated here rather than left to the SDK, which accepts a nonsense
    // endpoint at construction and fails much later with a transport error that
    // says nothing about the box the user typed into.
    if !endpoint.starts_with("https://") {
        return Err(IpcError::invalid(
            "o endpoint precisa começar com https:// — a API é assinada com SigV4 e não aceita texto claro",
        ));
    }
    if region.is_empty() {
        return Err(IpcError::invalid(
            "a região não pode ficar vazia: é ela que o SigV4 assina, e sem ela toda chamada falha como se fossem as credenciais",
        ));
    }

    // A candidate, not the live config: nothing is committed until the stack
    // built from it exists.
    let candidate = {
        let current = read(&state.config);
        Config {
            endpoint,
            region,
            profile: connection.profile.filter(|value| !value.trim().is_empty()),
            github_login: connection
                .github_login
                .filter(|value| !value.trim().is_empty()),
            // Untouched: this screen does not own the `[voice]` section, and
            // reading it from the live config is what keeps a connection save
            // from resetting the voice.
            voice: current.voice.clone(),
        }
    };

    rebuild_aws(&state, &candidate).await;
    *write(&state.config) = candidate.clone();

    // Persisted last. An unwritable disk must not stop the change the user just
    // made from taking effect — it is already live — but they have to be told,
    // or the next launch would silently come back on the old account.
    if let Err(error) = candidate.save() {
        tracing::warn!(%error, "the connection settings could not be written to disk");
        return Err(IpcError::invalid(error.to_string()));
    }

    Ok(ConnectionDto::from(&candidate))
}

/// Read a lock without letting a poisoned one take the app down.
///
/// A panic while holding one of these leaves settings mid-write at worst; the
/// alternative — propagating the poison — turns one bad request into an app
/// that can never read its own configuration again.
fn read<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Forwards sentences to the speaking task.
///
/// [`SentenceSink`] is deliberately synchronous and infallible — it is called
/// from inside the token loop, where blocking would stall the stream — so this
/// hands off over an unbounded channel and never waits.
struct ChannelSink {
    sentences: tokio::sync::mpsc::UnboundedSender<String>,
}

impl SentenceSink for ChannelSink {
    fn sentence(&mut self, sentence: &str) {
        // A closed channel means the speaking task is gone, which happens when
        // speech was not requested at all. Dropping is correct: the text still
        // reaches the UI through the returned answer.
        let _ = self.sentences.send(sentence.to_string());
    }
}

/// An unknown kind is the caller's mistake, not a reason to silently default.
fn parse_kind(kind: Option<String>) -> Result<Option<MemoryKind>, IpcError> {
    kind.filter(|value| !value.trim().is_empty())
        .map(|value| value.parse::<MemoryKind>())
        .transpose()
        .map_err(|error| IpcError::invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_blank_means_let_the_classifier_decide() {
        assert_eq!(
            parse_kind(Some("preference".into())).expect("valid"),
            Some(MemoryKind::Preference)
        );
        assert_eq!(parse_kind(None).expect("valid"), None);
        assert_eq!(parse_kind(Some("  ".into())).expect("valid"), None);
    }

    #[test]
    fn an_unknown_kind_is_rejected_as_invalid_input() {
        let error = parse_kind(Some("daydream".into())).expect_err("unknown kind");
        // The two things the caller acts on: that this is their mistake rather
        // than a broken backend, and which value was refused. The previous
        // assertion looked for "MemoryKind", a type name that never appears in
        // the message — it could not have passed even once the call compiled.
        assert!(error.invalid_input);
        assert!(
            error.message.contains("daydream"),
            "the message must name the value it refused, got: {}",
            error.message
        );
    }
}

#[tauri::command]
pub fn get_voice_profile(state: State<'_, Arc<AppState>>) -> Option<VoiceProfileDto> {
    let profile = match state.voice_profile.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    profile.map(VoiceProfileDto::from)
}

/// Refresh the SSO session for a profile, by shelling out to the AWS CLI.
///
/// # Why this takes a profile instead of configuring one
///
/// It used to write a hard-coded `[profile my-profile]` block — one person's start
/// URL, account id and role — into `~/.aws/config`, then *append* `profile =
/// "my-profile"` as raw text to `config.toml`. Both were wrong once the profile
/// became something the user picks: the append can produce a second `profile =`
/// key, and `Config::load` treats a malformed file as a hard error, so the app
/// would refuse to start with no way out but a text editor.
///
/// Creating a profile is `aws configure sso`'s job. This only logs one in.
#[tauri::command]
pub async fn login_sso(state: State<'_, Arc<AppState>>, profile: String) -> Result<(), IpcError> {
    let profile = profile.trim().to_string();
    if profile.is_empty() {
        return Err(IpcError::invalid(
            "escolha um perfil antes de entrar: o login SSO é por perfil",
        ));
    }

    // Checked before spawning, because `aws sso login` on an unknown profile
    // exits non-zero with a message on a stderr nobody is reading — the user
    // would see only "falhou".
    if !crate::config::aws_profile_names().contains(&profile) {
        return Err(IpcError::invalid(format!(
            "o perfil '{profile}' não existe em ~/.aws/config — crie-o com `aws configure sso`"
        )));
    }

    // `spawn_blocking` because this waits for a browser round trip. Left on the
    // async worker it was on, it blocks a runtime thread for as long as the user
    // takes to authenticate — which is the one moment the UI must stay alive.
    //
    // `PATH` is explicit: a `.app` opened from the Finder inherits none, and the
    // `aws` binary would simply not be found.
    let logging_in = profile.clone();
    let status = tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("aws")
            .env(
                "PATH",
                "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            )
            .args(["sso", "login", "--profile", &logging_in])
            .status()
    })
    .await
    .map_err(|error| IpcError::invalid(format!("o login SSO não pôde ser iniciado: {error}")))?
    .map_err(|error| {
        IpcError::invalid(format!(
            "não foi possível executar o AWS CLI (verifique se está instalado): {error}"
        ))
    })?;

    if !status.success() {
        return Err(IpcError::invalid("Falha na autenticação AWS SSO."));
    }

    // The credential providers cache per profile, so a stack built before the
    // login can still be holding the expired session. Rebuilding is what makes
    // the button's effect immediate instead of "now restart the app".
    let config = read(&state.config).clone();
    rebuild_aws(&state, &config).await;

    Ok(())
}
