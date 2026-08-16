//! The composition root.
//!
//! This is the one place that knows which implementation sits behind each port,
//! and it is deliberately short — the shape is lifted from
//! `agent-memory-mcp/src/main.rs` in the sibling repository, which wires the
//! same primary port in about the same number of lines.
//!
//! EchoBrain is a **third driving adapter** on `MemoryService`, next to the
//! deployed Lambda and the MCP server. It contains no backend, no embedding
//! code and no DynamoDB: `RemoteMemoryService` already speaks the API.

// `commands` and `config` are public so `examples/smoke.rs` can exercise the
// real composition root against the deployed API rather than a copy of it. A
// smoke test that wires its own services proves only that the copy works.
pub mod aws;
pub mod commands;
pub mod config;
pub mod conversation;
mod dto;
mod hotkey;
pub mod speaker;
pub mod sso;

use std::sync::Arc;

use echobrain_audio::{DEFAULT_MODEL, MicrophoneCapture, SystemSpeaker, WhisperTranscriber, model};
use echobrain_core::{
    AudioCapture, AudioMonitor, CueIntent, IntentClassifier, Journal, Speaker, Transcriber,
};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::commands::AppState;
use crate::config::Config;

pub fn run() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "echobrain=info,warn".into());

    // Truncated per launch rather than appended to: the question this answers
    // is always "what happened the last time I ran it", and an unbounded log
    // in Application Support is a second problem to own.
    let file = Config::log_path().and_then(|path| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::File::create(&path).ok()
    });

    // Both, not either: `make run` keeps its terminal output, and the bundle
    // gets a file to point at.
    match file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stdout.and(std::sync::Mutex::new(file)))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // Managed as an `Arc` so the conversation loop can hold the same
            // state the commands see. `State<'_, Arc<AppState>>` still derefs
            // to `AppState`, so every command reads unchanged.
            let state = tauri::async_runtime::block_on(build_state())?;

            // The reconciler's timer. Every 30 seconds rather than every few:
            // a pass with nothing waiting reads one local table and stops, and
            // the thing it is waiting for has a 90-second budget anyway.
            //
            // The lock is taken per tick, not held across the await, because
            // the settings screen replaces the AWS layer underneath it when the
            // profile changes.
            let aws_ref = state.aws.clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;

                    let reconciler = match aws_ref.read() {
                        Ok(aws) => aws.reconciler.clone(),
                        // Another thread panicked holding this lock. Recovered
                        // rather than propagated: killing the timer would leave
                        // every later capture stuck on "indexando" forever.
                        Err(poisoned) => poisoned.into_inner().reconciler.clone(),
                    };

                    match reconciler.probe_overdue().await {
                        Ok(report) if report.is_quiet() => {}
                        Ok(report) => {
                            for memory_id in &report.still_missing {
                                tracing::warn!(
                                    %memory_id,
                                    "stored but still not searchable past the consistency budget"
                                );
                            }
                            tracing::info!(
                                promoted = report.promoted,
                                failed = report.failed,
                                "reconciled"
                            );
                        }
                        Err(error) => tracing::warn!(%error, "reconciler probe failed"),
                    }
                }
            });

            tauri::Manager::manage(app, Arc::new(state));
            hotkey::register(app.handle())?;

            use tauri::Emitter;
            let show_i =
                tauri::menu::MenuItemBuilder::with_id("show", "Abrir Painel").build(app)?;
            let record_i = tauri::menu::MenuItemBuilder::with_id("record", "Gravar Nova Memória")
                .build(app)?;
            let search_i =
                tauri::menu::MenuItemBuilder::with_id("search", "Buscar Memória").build(app)?;
            let library_i =
                tauri::menu::MenuItemBuilder::with_id("library", "Biblioteca").build(app)?;
            let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let quit_i =
                tauri::menu::MenuItemBuilder::with_id("quit", "Encerrar EchoBrain").build(app)?;
            let menu = tauri::menu::MenuBuilder::new(app)
                .items(&[
                    &show_i, &sep1, &record_i, &search_i, &library_i, &sep2, &quit_i,
                ])
                .build()?;
            let mut tray = tauri::tray::TrayIconBuilder::new()
                .tooltip("EchoBrain")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    // Every item but "quit" starts by putting the window in
                    // front: the tray is reachable when the window is hidden,
                    // which is the only reason the menu exists.
                    match event.id().as_ref() {
                        "quit" => app.exit(0),
                        "show" => reveal(app),
                        "record" => {
                            reveal(app);
                            let _ = app.emit("echobrain://toggle-dictation", ());
                        }
                        "search" => {
                            reveal(app);
                            let _ = app.emit("echobrain://navigate", "search");
                        }
                        "library" => {
                            reveal(app);
                            let _ = app.emit("echobrain://navigate", "library");
                        }
                        _ => {}
                    }
                });
            // A tray with no icon is a tray you cannot click, but it is still
            // better than refusing to launch: the hotkey and the window both
            // work without it.
            match app.default_window_icon() {
                Some(icon) => tray = tray.icon(icon.clone()),
                None => tracing::warn!("no default window icon — the tray will be blank"),
            }
            let _tray = tray.build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::search_memories,
            commands::capture_memory,
            commands::forget_memory,
            commands::list_memories,
            commands::update_memory,
            commands::library,
            commands::dictation_available,
            commands::start_dictation,
            commands::stop_dictation,
            commands::audio_level,
            commands::login_sso,
            commands::answering_available,
            commands::speech_available,
            commands::classify_intent,
            commands::ask_memories,
            commands::speak,
            commands::stop_speaking,
            commands::available_voices,
            commands::get_settings,
            commands::set_settings,
            commands::get_connection,
            commands::set_connection,
            commands::conversation_available,
            commands::start_conversation,
            commands::stop_conversation,
            commands::get_voice_profile,
            commands::stop_and_measure_voice,
            commands::save_voice_profile,
        ])
        .run(tauri::generate_context!())
        .expect("the Tauri application failed to start");
}

/// Bring the main window to the front, if there is one.
///
/// Every failure here is ignored rather than propagated: this runs from a tray
/// menu handler that cannot report anything, and a window that refuses to show
/// is not a reason to take the app down with it.
fn reveal(app: &tauri::AppHandle) {
    use tauri::Manager;

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The SQLite journal, degrading to one that lives as long as the process.
///
/// A journal that cannot be opened must not stop the app from launching: the
/// journal is a cache of what this device wrote, and every capture is stored
/// remotely whether it is recorded here or not. Losing the file costs the
/// library view its history; refusing to start would cost everything.
///
/// The in-memory fallback failing is different — that is SQLite itself being
/// unusable, and nothing after this point would work either.
fn open_journal() -> Result<Arc<dyn Journal>, Box<dyn std::error::Error>> {
    match Config::journal_path() {
        Some(path) => match echobrain_journal::SqliteJournal::open(&path) {
            Ok(journal) => Ok(Arc::new(journal)),
            Err(error) => {
                tracing::warn!(%error, ?path, "journal unavailable; this session will not be kept");
                Ok(Arc::new(echobrain_journal::SqliteJournal::in_memory()?))
            }
        },
        None => {
            tracing::warn!("no config directory; this session will not be kept");
            Ok(Arc::new(echobrain_journal::SqliteJournal::in_memory()?))
        }
    }
}

/// Wire every port to its implementation. The only place that knows which.
pub async fn build_state() -> Result<AppState, Box<dyn std::error::Error>> {
    let config = Config::load()?;

    let journal = open_journal()?;

    let aws = aws::build(&config, Arc::clone(&journal)).await;

    // Spawning the capture thread does not open the device, so no microphone
    // permission prompt appears until the user actually dictates.
    // One device, two ports. `capture` hands back whole utterances for the
    // hotkey; `monitor` delivers frames for the conversation loop. The same
    // object serves both, so the stream is opened once however it is reached.
    let microphone = Arc::new(MicrophoneCapture::new());
    let capture: Arc<dyn AudioCapture> = microphone.clone();
    let monitor: Option<Arc<dyn AudioMonitor>> = Some(microphone as Arc<dyn AudioMonitor>);

    // A missing model disables dictation rather than failing the launch: it is
    // a 570 MB optional download, and typing must keep working without it.
    let transcriber: Option<Arc<dyn Transcriber>> = match model::require_model(DEFAULT_MODEL) {
        Ok(path) => match WhisperTranscriber::load(&path, Some("pt".to_string())) {
            Ok(loaded) => {
                tracing::info!(model = %path.display(), "dictation ready");
                Some(Arc::new(loaded) as Arc<dyn Transcriber>)
            }
            Err(error) => {
                tracing::warn!(%error, "the speech model failed to load; dictation is off");
                None
            }
        },
        Err(error) => {
            tracing::info!(%error, "dictation is off");
            None
        }
    };

    // Absent when the Mac has no Portuguese voice installed. Same posture as
    // the microphone: the feature disappears rather than failing loudly.
    //
    // Held twice: once behind the port, which is all the use cases may see, and
    // once concretely, so the settings screen can change the voice without
    // rebuilding everything that holds a reference to it.
    let system_speaker =
        SystemSpeaker::resolve_voice(config.voice.tts_voice.as_deref()).map(|voice| {
            tracing::info!(%voice, "speech ready");
            Arc::new(SystemSpeaker::new(voice, config.voice.tts_rate))
        });
    if system_speaker.is_none() {
        tracing::info!("no pt-BR system voice installed; answers will not be spoken");
    }

    let settings = Arc::new(std::sync::RwLock::new(config.voice.clone()));

    let polly_speaker = Arc::new(echobrain_audio::PollySpeaker::new(
        aws.polly_client.clone(),
        config
            .voice
            .tts_voice
            .clone()
            .unwrap_or(echobrain_audio::DEFAULT_POLLY_VOICE.to_string()),
    ));

    let speaker: Option<Arc<dyn Speaker>> = Some(Arc::new(crate::speaker::RoutingSpeaker {
        system: system_speaker.clone(),
        polly: polly_speaker.clone(),
        settings: settings.clone(),
    }) as Arc<dyn Speaker>);

    let voice_identity: Option<Arc<dyn echobrain_core::VoiceIdentity>> =
        match echobrain_audio::model::require_model("wespeaker.onnx") {
            // Lossy rather than `unwrap`: a non-UTF-8 home directory is the
            // user's business, not a reason to abort the launch. If the path is
            // mangled the load below fails and the filter stays off, which is a
            // supported state.
            Ok(path) => match echobrain_audio::WeSpeakerIdentity::load(&path.to_string_lossy()) {
                Ok(loaded) => {
                    tracing::info!(model = %path.display(), "voice identity filter ready");
                    Some(Arc::new(loaded))
                }
                Err(error) => {
                    tracing::warn!(%error, "the voice identity model failed to load");
                    None
                }
            },
            Err(error) => {
                tracing::info!(%error, "voice identity filter is off");
                None
            }
        };

    Ok(AppState {
        // Behind a lock rather than held directly, so the settings screen can
        // move the app to another account without a restart. Every command
        // takes a snapshot; see `AppState::aws`.
        aws: Arc::new(std::sync::RwLock::new(Arc::new(aws))),
        journal,
        capture,
        transcriber,
        speaker,
        intent: Arc::new(CueIntent::new()) as Arc<dyn IntentClassifier>,
        cue: Arc::new(CueIntent::new()),
        monitor,
        // Off until the embedding model exists. Nothing degrades without it:
        // the filter never fires and every utterance is treated as the owner's,
        // which is what the app did before this existed.
        voice_identity,
        voice_profile: Arc::new(std::sync::RwLock::new(
            crate::config::Config::load_voice_profile().unwrap_or(None),
        )),
        conversation: Arc::new(std::sync::Mutex::new(None)),
        system_speaker,
        polly_speaker,
        settings,
        config: Arc::new(std::sync::RwLock::new(config)),
    })
}
