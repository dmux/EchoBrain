//! The IPC contract, exercised through Tauri's mock runtime.
//!
//! What this covers that nothing else does:
//!
//! * every command named in `TauriGateway` is actually registered;
//! * Tauri's camelCase-to-snake_case argument conversion really happens, so the
//!   frontend's `topK` binds to `top_k`. If it did not, every search would fail
//!   with a missing-argument error — and no unit test would notice;
//! * responses reach JavaScript in the shape `types.ts` declares.
//!
//! No window, no network, no AWS: the memory layer is the domain's own
//! in-memory implementation.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use agent_memory_core::LocalMemoryService;
use agent_memory_core::testing::{FixedClock, InMemoryRepository, SeqIdGenerator, StubEmbedder};
use echobrain_core::testing::InMemoryJournal;
use echobrain_core::{
    CueClassifier, CueIntent, IngestUtterance, Journal, MemoryService, SearchMemories,
};
use echobrain_lib::commands::AppState;
use serde_json::{Value, json};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindowBuilder};

const DIMENSIONS: usize = 1024;

/// An `AudioCapture` that opens nothing.
#[derive(Debug)]
struct SilentCapture;

#[async_trait::async_trait]
impl echobrain_core::AudioCapture for SilentCapture {
    async fn start(&self) -> Result<(), echobrain_core::EchoError> {
        Ok(())
    }

    async fn stop(&self) -> Result<echobrain_core::Samples, echobrain_core::EchoError> {
        Err(echobrain_core::EchoError::EmptyUtterance)
    }

    fn level(&self) -> f32 {
        0.0
    }
}

/// A `Speaker` that records what it was asked to say instead of making noise.
///
/// The point of the assertions below is that the text reached the port at all:
/// whether Polly or `say` then produces sound is the adapter's business, and
/// tested where those live.
#[derive(Debug, Default)]
struct RecordingSpeaker {
    said: std::sync::Mutex<Vec<String>>,
}

impl RecordingSpeaker {
    fn said(&self) -> Vec<String> {
        self.said.lock().expect("not poisoned").clone()
    }
}

#[async_trait::async_trait]
impl echobrain_core::Speaker for RecordingSpeaker {
    async fn speak(&self, text: &str) -> Result<(), echobrain_core::EchoError> {
        self.said
            .lock()
            .expect("not poisoned")
            .push(text.to_string());
        Ok(())
    }

    fn stop(&self) {}

    fn is_speaking(&self) -> bool {
        false
    }
}

fn app() -> tauri::App<tauri::test::MockRuntime> {
    app_with_speaker(None)
}

fn app_with_speaker(
    speaker: Option<Arc<dyn echobrain_core::Speaker>>,
) -> tauri::App<tauri::test::MockRuntime> {
    let memory: Arc<dyn MemoryService> = Arc::new(LocalMemoryService::new(
        InMemoryRepository::default(),
        StubEmbedder::new(DIMENSIONS),
        FixedClock::new(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        SeqIdGenerator::default(),
    ));
    let journal: Arc<dyn Journal> = Arc::new(InMemoryJournal::new());

    // Constructed against a bare config rather than a resolved one: no
    // credentials are needed to *hold* a client, and none of these tests reach
    // the network.
    let polly_client = aws_sdk_polly::Client::from_conf(
        aws_sdk_polly::Config::builder()
            .behavior_version(aws_sdk_polly::config::BehaviorVersion::latest())
            .region(aws_sdk_polly::config::Region::new("us-east-1"))
            .build(),
    );

    // The real `aws::build` resolves a credential chain, so the tests assemble
    // the stack directly around a local memory service instead. The wiring that
    // matters to the commands — one search instance shared with answering, one
    // ingest over the journal — is the same either way.
    let aws = echobrain_lib::aws::AwsStack {
        search: Arc::new(SearchMemories::new(Arc::clone(&memory))),
        ingest: IngestUtterance::new(
            Arc::clone(&memory),
            Arc::new(CueClassifier::new()),
            Arc::clone(&journal),
            "echobrain-ipc-test",
        ),
        memory: Arc::clone(&memory),
        // A Mac with no answering model configured — what a fresh install looks
        // like, and a state the UI must survive.
        answer: None,
        // No answering model in a bare install, so the cue router's default
        // stands — which is the state most machines are in.
        intent_judge: None,
        polly_client: polly_client.clone(),
        reconciler: Arc::new(echobrain_core::usecase::reconcile::Reconciler::new(
            Arc::clone(&journal),
            Arc::clone(&memory),
        )),
    };

    let state = AppState {
        aws: Arc::new(std::sync::RwLock::new(Arc::new(aws))),
        journal,
        // A silent capture: the tests never open a real device, and no
        // microphone permission is involved.
        capture: Arc::new(SilentCapture),
        // The model is a 570 MB optional download, so `None` is the state most
        // machines are in and the one the UI has to handle.
        transcriber: None,
        // Likewise: a Mac with no Portuguese voice. A state the UI must
        // survive, and what a fresh install looks like.
        speaker,
        intent: Arc::new(CueIntent::new()),
        cue: Arc::new(CueIntent::new()),
        // No continuous microphone in a mock runtime, which is also the state
        // conversation mode has to refuse cleanly from.
        monitor: None,
        voice_identity: None,
        voice_profile: Arc::new(std::sync::RwLock::new(None)),
        conversation: Arc::new(std::sync::Mutex::new(None)),
        system_speaker: None,
        // Unlike `system_speaker`, this one is not an `Option`, so even a test
        // that never speaks has to build it.
        polly_speaker: Arc::new(echobrain_audio::PollySpeaker::new(
            polly_client,
            echobrain_audio::DEFAULT_POLLY_VOICE,
        )),
        settings: Arc::new(std::sync::RwLock::new(Default::default())),
        config: Arc::new(std::sync::RwLock::new(Default::default())),
    };

    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            echobrain_lib::commands::search_memories,
            echobrain_lib::commands::capture_memory,
            echobrain_lib::commands::forget_memory,
            echobrain_lib::commands::library,
            echobrain_lib::commands::list_memories,
            echobrain_lib::commands::update_memory,
            echobrain_lib::commands::dictation_available,
            echobrain_lib::commands::start_dictation,
            echobrain_lib::commands::stop_dictation,
            echobrain_lib::commands::audio_level,
            echobrain_lib::commands::answering_available,
            echobrain_lib::commands::speech_available,
            echobrain_lib::commands::classify_intent,
            echobrain_lib::commands::ask_memories,
            echobrain_lib::commands::speak,
            echobrain_lib::commands::stop_speaking,
            echobrain_lib::commands::available_voices,
            echobrain_lib::commands::get_settings,
            echobrain_lib::commands::set_settings,
            echobrain_lib::commands::get_connection,
            echobrain_lib::commands::set_connection,
            echobrain_lib::commands::conversation_available,
            echobrain_lib::commands::start_conversation,
            echobrain_lib::commands::stop_conversation,
        ])
        // The *real* context, not `mock_context`: only this carries the ACL
        // that `tauri-build` generated from `capabilities/`. A mock context has
        // an empty authority, which denies every command and would make this
        // test say nothing about whether the shipped app works.
        .build(tauri::generate_context!())
        .expect("the app builds");

    app.manage(Arc::new(state));
    WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("the mock window builds");

    app
}

/// Invoke a command the way the frontend does: camelCase keys, JSON body.
fn invoke(
    app: &tauri::App<tauri::test::MockRuntime>,
    cmd: &str,
    args: Value,
) -> Result<Value, Value> {
    let webview = app.get_webview_window("main").expect("the window exists");

    let response = tauri::test::get_ipc_response(
        &webview,
        InvokeRequest {
            cmd: cmd.to_string(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            // The scheme a bundled frontend really runs under on macOS. The
            // capability is `local`, so an origin the ACL reads as remote is
            // rejected — which is the behaviour being relied on, not worked
            // around.
            url: "tauri://localhost".parse().expect("valid url"),
            body: args.into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    );

    match response {
        Ok(body) => Ok(body.deserialize().expect("the response is JSON")),
        Err(error) => {
            eprintln!("[ipc] {cmd} failed: {error}");
            Err(error)
        }
    }
}

#[test]
fn capture_then_search_round_trips_through_the_ipc_boundary() {
    let app = app();

    let captured = invoke(
        &app,
        "capture_memory",
        json!({ "text": "eu prefiro café coado", "kind": null, "ttlSeconds": null }),
    )
    .expect("capture succeeds");

    assert_eq!(
        captured["entry"]["kind"], "preference",
        "the classifier's guess must survive the boundary"
    );
    assert_eq!(
        captured["entry"]["state"], "indexing",
        "a fresh write must not claim to be searchable"
    );
    assert!(captured["journalFailed"].is_null());
    assert!(
        captured["entry"]["memoryId"].is_string(),
        "the frontend reads memoryId, not memory_id"
    );

    // `topK` is the whole point of this assertion: Tauri converts it to `top_k`.
    // Without that conversion the command would reject the call outright.
    let hits = invoke(
        &app,
        "search_memories",
        json!({ "query": "que café eu gosto?", "kind": null, "topK": 3, "minSimilarity": null }),
    )
    .expect("search succeeds");

    let hits = hits.as_array().expect("an array of hits");
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0]["similarity"].is_number(),
        "the UI receives similarity, never a distance"
    );
    assert!(
        hits[0].get("distance").is_none(),
        "no distance may cross into the frontend"
    );
}

#[test]
fn an_out_of_range_top_k_comes_back_tagged_as_invalid_input() {
    let app = app();

    // 101 exceeds the DynamoDB SearchVectors ceiling, and must be refused
    // before it costs an embedding.
    let error = invoke(
        &app,
        "search_memories",
        json!({ "query": "qualquer coisa", "kind": null, "topK": 101, "minSimilarity": null }),
    )
    .expect_err("101 is out of range");

    assert_eq!(
        error["invalidInput"], true,
        "the UI distinguishes a bad query from a broken backend by this flag"
    );
}

#[test]
fn an_unknown_kind_is_rejected_rather_than_defaulted() {
    let app = app();

    let error = invoke(
        &app,
        "capture_memory",
        json!({ "text": "algo", "kind": "daydream", "ttlSeconds": null }),
    )
    .expect_err("unknown kinds are refused");

    assert_eq!(error["invalidInput"], true);
}

#[test]
fn silence_is_reported_as_invalid_input_not_a_crash() {
    let app = app();

    let error = invoke(
        &app,
        "capture_memory",
        json!({ "text": "   ", "kind": null, "ttlSeconds": null }),
    )
    .expect_err("whitespace is not an utterance");

    assert_eq!(error["invalidInput"], true);
}

#[test]
fn dictation_reports_itself_unavailable_without_the_model() {
    let app = app();

    let available = invoke(&app, "dictation_available", json!({})).expect("the command answers");
    assert_eq!(
        available, false,
        "the UI hides the microphone rather than offering a button that always fails"
    );

    // The hotkey is registered whether or not a model is installed, so the
    // commands must refuse cleanly rather than panic.
    let error = invoke(&app, "start_dictation", json!({})).expect_err("no model, no dictation");
    assert_eq!(error["invalidInput"], true);
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("make model"),
        "the message must say how to fix it, got {}",
        error["message"]
    );
}

#[test]
fn the_level_meter_answers_even_when_nothing_is_recording() {
    // Polled at ~30 fps from the UI; it must never be the thing that throws.
    let app = app();
    let level = invoke(&app, "audio_level", json!({})).expect("the command answers");
    assert_eq!(level, 0.0);
}

#[test]
fn the_voice_features_report_themselves_unavailable_on_a_bare_install() {
    let app = app();

    // Both are optional in different ways — one needs a system voice, the other
    // needs a Bedrock model the account is entitled to — and the UI hides each
    // path rather than offering a button that always fails.
    assert_eq!(
        invoke(&app, "speech_available", json!({})).expect("answers"),
        false
    );
    assert_eq!(
        invoke(&app, "answering_available", json!({})).expect("answers"),
        false
    );

    let error = invoke(
        &app,
        "ask_memories",
        json!({ "question": "por que não OpenSearch?", "speak": false, "minSimilarity": null }),
    )
    .expect_err("no model, no answer");
    assert_eq!(error["invalidInput"], true);
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("answer_model"),
        "the message must name the setting to fix, got {}",
        error["message"]
    );

    let error = invoke(&app, "speak", json!({ "text": "olá" })).expect_err("no voice, no speech");
    assert_eq!(error["invalidInput"], true);

    // Barge-in must be safe when nothing is speaking — the conversation loop
    // calls it without checking.
    invoke(&app, "stop_speaking", json!({})).expect("stopping nothing is not an error");
}

#[test]
fn intent_routing_crosses_the_boundary_as_the_strings_the_ui_expects() {
    let app = app();

    let recorded = invoke(
        &app,
        "classify_intent",
        json!({ "text": "o deploy saiu às três da tarde" }),
    )
    .expect("answers");
    assert_eq!(recorded, "record");

    let asked = invoke(
        &app,
        "classify_intent",
        json!({ "text": "por que a gente não foi de OpenSearch?" }),
    )
    .expect("answers");
    assert_eq!(
        asked, "ask",
        "the frontend switches the whole capture flow on this string"
    );
}

/// The three-file rule, checked as data rather than by invoking anything.
///
/// Adding a command means naming it in `build.rs`, in `generate_handler!` and
/// in `capabilities/default.json`. Miss one and the app still builds, still
/// launches, still looks right, and then every `invoke()` of that command fails
/// at runtime with *"not allowed. Plugin not found"*.
///
/// `login_sso` shipped in exactly that state — present in two of the three,
/// working only because a stale `permissions/autogenerated/login_sso.toml`
/// survived on disk, and one clean regeneration away from breaking.
///
/// The speaking half of the "Testar voz" button.
///
/// The `set_settings` half lives in `settings_persistence.rs`, on its own,
/// because it writes `config.toml` for real — see the note there.
#[test]
fn the_test_voice_button_reaches_the_speaker_with_its_text() {
    let recorder = Arc::new(RecordingSpeaker::default());
    let app = app_with_speaker(Some(recorder.clone()));

    invoke(
        &app,
        "speak",
        json!({ "text": "Assim que eu vou ler as suas memórias." }),
    )
    .expect("speak is reachable");

    assert_eq!(
        recorder.said(),
        vec!["Assim que eu vou ler as suas memórias.".to_string()],
        "the button's text must reach the speaker port"
    );
}

/// `speak` must report a missing voice rather than pretending it spoke.
///
/// Silence and success are indistinguishable from the outside, so the one thing
/// this can never do is return `Ok` having made no sound.
#[test]
fn speaking_without_a_voice_installed_is_an_error_not_a_silent_success() {
    let app = app();
    let error = invoke(
        &app,
        "speak",
        json!({ "text": "Assim que eu vou ler as suas memórias." }),
    )
    .expect_err("no speaker is configured in this state");

    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("voz")),
        "the reason must say something about the voice, got: {error}"
    );
}

/// The *Conexão* section is the only place to fix a misconfigured account, so
/// it has to be able to draw itself before anything works — including the
/// lists, which it renders in the same pass as the values.
#[test]
fn the_connection_screen_can_populate_itself() {
    let app = app();

    let connection = invoke(&app, "get_connection", json!({}))
        .expect("get_connection is reachable — the section cannot render without it");

    for key in [
        "endpoint",
        "region",
        "profile",
        "githubLogin",
        "profiles",
        "envOverrides",
        "sso",
    ] {
        assert!(
            connection.get(key).is_some(),
            "`{key}` is missing from the connection DTO, so `Connection` in types.ts \
             cannot bind: {connection}"
        );
    }

    // Both are `Vec`, never null. The screen branches on `.length`, and `null`
    // would throw where an empty list is a meaningful, ordinary answer — a
    // machine with no `~/.aws/config` at all.
    assert!(
        connection["profiles"].is_array(),
        "profiles must be a list even when empty: {connection}"
    );
    assert!(
        connection["envOverrides"].is_array(),
        "envOverrides must be a list even when empty: {connection}"
    );

    // `sso` is the one field allowed to be null — a profile with static keys has
    // no session — but when it is present the screen reads four fields off it,
    // and a missing one would render "expira em NaN".
    assert!(
        connection["sso"].is_null() || connection["sso"].is_object(),
        "sso must be null or the full status object: {connection}"
    );
    if let Some(sso) = connection["sso"].as_object() {
        for key in ["session", "expiresAt", "expiresIn", "renewable"] {
            assert!(
                sso.contains_key(key),
                "`{key}` is missing from the SSO status, so the countdown cannot render: {connection}"
            );
        }
    }
}

/// A cleartext endpoint is refused before anything is rebuilt or written.
///
/// Tagged `invalidInput` so the frontend can tell "fix the box you typed in"
/// from "the backend is unreachable" without matching on message strings.
#[test]
fn a_connection_without_https_is_rejected_as_bad_input() {
    let app = app();

    let error = invoke(
        &app,
        "set_connection",
        json!({ "connection": {
            "endpoint": "http://example.com/",
            "region": "us-east-1",
            "profile": null,
            "githubLogin": null,
        }}),
    )
    .expect_err("a cleartext endpoint cannot be signed with SigV4");

    assert_eq!(
        error["invalidInput"], true,
        "the user has to be told this is their input, not a broken backend: {error}"
    );
}

/// An empty region is the other half of the same guard.
///
/// It is the one field whose absence fails *late* and misleadingly — signing
/// without a region reads as a credentials error — so it must not reach the
/// SDK at all.
#[test]
fn a_connection_without_a_region_is_rejected_as_bad_input() {
    let app = app();

    let error = invoke(
        &app,
        "set_connection",
        json!({ "connection": {
            "endpoint": "https://example.execute-api.us-east-1.amazonaws.com/",
            "region": "   ",
            "profile": null,
            "githubLogin": null,
        }}),
    )
    .expect_err("a blank region cannot sign anything");

    assert_eq!(
        error["invalidInput"], true,
        "reported as bad input: {error}"
    );
}

/// The settings screen renders nothing until `get_settings` answers, and its
/// test button is disabled while `available_voices` is empty. Both are
/// therefore load-bearing for a screen that can be used at all.
#[test]
fn the_settings_screen_can_populate_itself() {
    let app = app();

    let settings = invoke(&app, "get_settings", json!({}))
        .expect("get_settings is reachable — the dialog is a spinner until it answers");
    for key in [
        "ttsEngine",
        "ttsVoice",
        "ttsRate",
        "answerModel",
        "answerMaxTokens",
        "minSimilarity",
        "voiceThreshold",
    ] {
        assert!(
            settings.get(key).is_some(),
            "`{key}` is missing from the settings DTO, so `VoiceSettings` in types.ts cannot bind: {settings}"
        );
    }

    let voices =
        invoke(&app, "available_voices", json!({})).expect("available_voices is reachable");
    let voices = voices.as_array().expect("a list of voices");

    // `disabled={testing || voices.length === 0}`: an empty list is not a
    // cosmetic problem, it is a "Testar voz" button that cannot be pressed and
    // says nothing about why.
    assert!(
        !voices.is_empty(),
        "an empty voice list disables the test button outright"
    );
    assert!(
        voices.iter().any(|voice| voice["engine"] == "polly"),
        "the Polly voices are hard-coded and must always be offered: {voices:?}"
    );
}

/// Deliberately static: the honest dynamic test would be to invoke every
/// command, and some of them have side effects no test should have —
/// `login_sso` appends to `~/.aws/config` and shells out to `aws sso login`.
#[test]
fn every_command_is_named_in_all_three_places() {
    let build_rs = include_str!("../build.rs");
    let lib_rs = include_str!("../src/lib.rs");
    let capabilities = include_str!("../capabilities/default.json");

    let declared = quoted_command_list(build_rs);
    assert!(
        declared.len() >= 8,
        "the command list in build.rs was not parsed: {declared:?}"
    );

    let permissions: Value = serde_json::from_str(capabilities).expect("the capability is JSON");
    let granted = permissions["permissions"]
        .as_array()
        .expect("permissions is an array")
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|permission| permission.strip_prefix("allow-"))
        .map(|permission| permission.replace('-', "_"))
        .collect::<Vec<_>>();

    for command in &declared {
        assert!(
            granted.contains(command),
            "`{command}` is in build.rs but has no `allow-{}` in capabilities/default.json",
            command.replace('_', "-")
        );
        assert!(
            lib_rs.contains(&format!("commands::{command},")),
            "`{command}` is in build.rs but missing from generate_handler! in lib.rs"
        );
    }

    for command in &granted {
        assert!(
            declared.contains(command),
            "`allow-{}` is granted but `{command}` is not named in build.rs — \
             this is the `login_sso` failure mode: it works only until the \
             generated permissions are rebuilt from scratch",
            command.replace('_', "-")
        );
    }

    // The other direction, which the two loops above cannot see. A command
    // registered *only* in `generate_handler!` compiles, runs, and is then
    // refused by the ACL the first time the frontend calls it — which is how
    // `list_memories` and `update_memory` shipped unreachable: the library view
    // could list nothing and save nothing, with "Plugin not found" as the only
    // clue, and every test here still passed.
    let registered = registered_commands(lib_rs);
    assert!(
        registered.len() >= declared.len(),
        "the generate_handler! list in lib.rs was not parsed: {registered:?}"
    );

    for command in registered {
        assert!(
            declared.contains(&command),
            "`commands::{command}` is in generate_handler! but not in build.rs, \
             so the ACL will reject every call to it at runtime"
        );
    }
}

/// The command names inside `generate_handler![…]` in `lib.rs`.
fn registered_commands(lib_rs: &str) -> Vec<String> {
    let start = lib_rs
        .find("generate_handler![")
        .expect("lib.rs registers a command list");
    let rest = &lib_rs[start..];
    let end = rest.find(']').expect("the command list is closed");

    rest[..end]
        .lines()
        .filter_map(|line| line.trim().strip_prefix("commands::"))
        .filter_map(|line| line.strip_suffix(','))
        .map(str::to_owned)
        .collect()
}

/// Pull the string literals out of `build.rs`'s `commands(&[…])` list.
///
/// Line comments are stripped first: the list is documented inline, and a
/// comment that quotes an error message would otherwise be read as a command
/// name — which is precisely what happened the first time this test ran.
fn quoted_command_list(build_rs: &str) -> Vec<String> {
    let start = build_rs
        .find(".commands(&[")
        .expect("build.rs declares a command list");
    let rest = &build_rs[start..];
    let end = rest.find("])").expect("the command list is closed");

    rest[..end]
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .flat_map(|line| {
            line.split('"')
                // Quoted spans are the odd-numbered pieces of a split on the quote.
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_library_lists_captures_and_forget_removes_them() {
    let app = app();

    let captured = invoke(
        &app,
        "capture_memory",
        json!({ "text": "decidimos por DynamoDB", "kind": null, "ttlSeconds": null }),
    )
    .expect("capture succeeds");
    let memory_id = captured["entry"]["memoryId"]
        .as_str()
        .expect("an id")
        .to_string();

    let library = invoke(&app, "library", json!({ "limit": 10 })).expect("library succeeds");
    assert_eq!(library.as_array().expect("array").len(), 1);

    invoke(&app, "forget_memory", json!({ "memoryId": memory_id })).expect("forget succeeds");

    let library = invoke(&app, "library", json!({ "limit": 10 })).expect("library succeeds");
    assert!(
        library.as_array().expect("array").is_empty(),
        "a forgotten memory leaves the library view"
    );

    // The path the library screen actually takes. It reads `list_memories`,
    // not the local journal, so a delete that only tombstoned locally would
    // still show the memory on the next refresh.
    let listed = invoke(&app, "list_memories", json!({})).expect("list succeeds");
    assert!(
        listed["memories"].as_array().expect("array").is_empty(),
        "a forgotten memory is gone from the API listing too"
    );
}

#[test]
fn rating_and_deactivating_a_memory_cross_the_boundary_as_the_library_sends_them() {
    // `updates` is a nested object, which is the shape that broke once already:
    // `setSettings` sent `{ update: settings }` against a parameter called
    // something else and Tauri rejected the call while binding, long before any
    // of this code ran. See `tests/argument_names.rs`.
    let app = app();

    let captured = invoke(
        &app,
        "capture_memory",
        json!({ "text": "prefiro arquitetura hexagonal", "kind": null, "ttlSeconds": null }),
    )
    .expect("capture succeeds");
    let memory_id = captured["entry"]["memoryId"]
        .as_str()
        .expect("an id")
        .to_string();

    invoke(
        &app,
        "update_memory",
        json!({ "memoryId": memory_id, "updates": { "rating": 4, "active": false } }),
    )
    .expect("update succeeds");

    let listed = invoke(&app, "list_memories", json!({})).expect("list succeeds");
    let memories = listed["memories"].as_array().expect("array");
    assert_eq!(memories.len(), 1);

    let memory = &memories[0];
    assert_eq!(memory["rating"], json!(4));
    assert_eq!(
        memory["active"],
        json!(false),
        "a deactivated memory is still listed — that is the whole point of the \
         flag over a delete"
    );

    // Clearing the rating is `null`, not an omitted field: omitting it means
    // "leave it alone", and the two have to stay distinguishable across JSON.
    invoke(
        &app,
        "update_memory",
        json!({ "memoryId": memory_id, "updates": { "rating": null } }),
    )
    .expect("clearing succeeds");

    let listed = invoke(&app, "list_memories", json!({})).expect("list succeeds");
    assert_eq!(listed["memories"][0]["rating"], json!(null));
    assert_eq!(
        listed["memories"][0]["active"],
        json!(false),
        "an update names what it changes and leaves the rest alone"
    );
}

#[test]
fn the_library_pages_through_everything_even_when_a_filtered_page_comes_back_empty() {
    // The rule the infinite scroll is built on: `nextCursor` is the only signal
    // for the end. The backend filters *after* taking the page, so a page can
    // arrive empty with rows still behind it — and a scroll that stopped on a
    // short page would show a truncated library over a full namespace.
    let app = app();

    for index in 0..7 {
        invoke(
            &app,
            "capture_memory",
            json!({ "text": format!("memória comum {index}"), "kind": "fact", "ttlSeconds": null }),
        )
        .expect("capture succeeds");
    }
    invoke(
        &app,
        "capture_memory",
        json!({ "text": "a única preferência", "kind": "preference", "ttlSeconds": null }),
    )
    .expect("capture succeeds");

    // Unfiltered: every memory is reached, once.
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let page = invoke(
            &app,
            "list_memories",
            json!({ "limit": 3, "cursor": cursor, "filter": null }),
        )
        .expect("list succeeds");

        for memory in page["memories"].as_array().expect("array") {
            seen.push(memory["text"].as_str().expect("text").to_string());
        }

        cursor = page["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(seen.len() <= 8, "the cursor is not advancing");
    }
    assert_eq!(seen.len(), 8, "no page was skipped or repeated");

    // Filtered down to one row, with a page size that guarantees at least one
    // page matches nothing at all.
    let mut found: Vec<String> = Vec::new();
    let mut cursor = Value::Null;
    let mut empty_pages_with_more = 0;
    loop {
        let page = invoke(
            &app,
            "list_memories",
            json!({
                "limit": 2,
                "cursor": cursor,
                "filter": { "kind": "preference" },
            }),
        )
        .expect("list succeeds");

        let memories = page["memories"].as_array().expect("array");
        cursor = page["nextCursor"].clone();

        if memories.is_empty() && !cursor.is_null() {
            empty_pages_with_more += 1;
        }
        for memory in memories {
            found.push(memory["text"].as_str().expect("text").to_string());
        }

        if cursor.is_null() {
            break;
        }
    }

    assert_eq!(found, vec!["a única preferência".to_string()]);
    assert!(
        empty_pages_with_more > 0,
        "this test only proves anything if a page really did come back empty \
         with more to follow"
    );
}

#[test]
fn a_blank_filter_field_narrows_nothing() {
    // An empty text box is not a filter for the empty string. Without the
    // trimming behind this, typing into a filter and deleting it again would
    // blank the library.
    let app = app();
    invoke(
        &app,
        "capture_memory",
        json!({ "text": "decidimos por DynamoDB", "kind": null, "ttlSeconds": null }),
    )
    .expect("capture succeeds");

    let listed = invoke(
        &app,
        "list_memories",
        json!({ "filter": { "githubRepo": "   ", "textContains": "" } }),
    )
    .expect("list succeeds");

    assert_eq!(listed["memories"].as_array().expect("array").len(), 1);
}

#[test]
fn a_page_size_the_backend_would_refuse_is_rejected_as_bad_input() {
    let app = app();
    let error =
        invoke(&app, "list_memories", json!({ "limit": 0 })).expect_err("zero is not a page size");
    assert_eq!(error["invalidInput"], json!(true), "{error}");
}
