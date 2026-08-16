//! Neither settings screen may lose what the other one owns.
//!
//! # What changed
//!
//! This used to assert that a save kept "the fields the settings screen does not
//! own" — `profile`, `region`, `endpoint`, `github_login`. The screen owns them
//! now: they are edited in the *Conexão* section, through `set_connection`. What
//! survives is the halving, not the ownership. There are two writers into one
//! file, and each has to leave the other's section exactly as it found it.
//!
//! The direction that used to bite is still the dangerous one. `set_settings`
//! merges through `state.config` rather than reconstructing it, and a version
//! that rebuilt the struct instead dropped `profile = "my-profile"` and took every
//! AWS credential with it.
//!
//! # Why this is not driven through `set_settings`
//!
//! `set_settings` calls `Config::save`, which resolves its path from `$HOME`.
//! Invoking that command from a test therefore rewrites the developer's real
//! `~/Library/Application Support/EchoBrain/config.toml` — which is not
//! hypothetical. A test doing exactly that dropped `profile = "my-profile"`,
//! because the IPC harness builds its `AppState` from `Config::default()` where
//! `profile` is `None`; every AWS call afterwards fell back to the default
//! credential chain and the app looked configured while reaching nothing.
//!
//! Redirecting `$HOME` would need `std::env::set_var`, and this workspace
//! forbids `unsafe`. So the property is tested one layer down, against a
//! scratch file, through [`Config::save_at`].
//!
//! What this leaves uncovered is the merge inside `set_settings` itself. That
//! needs the config path injected into `AppState` rather than resolved from a
//! global, which is a change worth making deliberately.

use std::time::{SystemTime, UNIX_EPOCH};

use echobrain_lib::config::Config;

/// A configured machine, as either screen would find it.
fn configured() -> Config {
    Config {
        endpoint: "https://example.execute-api.us-east-1.amazonaws.com/".into(),
        region: "us-east-1".into(),
        profile: Some("my-profile".into()),
        github_login: Some("someone".into()),
        voice: echobrain_lib::config::VoiceConfig {
            // Fields with no control on either screen. They have to survive too:
            // an unrendered field is the easiest kind to drop.
            follow_up_seconds: 6,
            hangover_ms: 800,
            tts_engine: Some("polly".into()),
            tts_voice: Some("Vitoria".into()),
            tts_rate: 185,
            answer_model: Some("us.anthropic.claude-haiku-4-5-20251001-v1:0".into()),
            ..Default::default()
        },
    }
}

#[test]
fn saving_the_voice_settings_keeps_the_connection() {
    let path = scratch_file("voice-keeps-connection");

    // What `set_settings` does: replace `[voice]` on the config it already
    // holds, then write the whole thing back.
    let mut config = configured();
    config.voice.tts_rate = 220;
    config.voice.tts_voice = Some("Camila".into());

    config.save_at(&path).expect("the scratch file is writable");
    let reloaded = Config::load_at(&path).expect("the saved file parses");

    assert_eq!(
        reloaded.profile.as_deref(),
        Some("my-profile"),
        "changing the voice must not drop the AWS profile — that costs every credential"
    );
    assert_eq!(reloaded.region, "us-east-1");
    assert_eq!(
        reloaded.endpoint,
        "https://example.execute-api.us-east-1.amazonaws.com/"
    );
    assert_eq!(reloaded.github_login.as_deref(), Some("someone"));

    assert_eq!(reloaded.voice.tts_rate, 220);
    assert_eq!(reloaded.voice.tts_voice.as_deref(), Some("Camila"));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn saving_the_connection_keeps_the_voice_section() {
    let path = scratch_file("connection-keeps-voice");

    // What `set_connection` does: build a candidate from the four editable
    // fields, carrying `voice` across from the live config untouched.
    let current = configured();
    let candidate = Config {
        endpoint: "https://other.execute-api.sa-east-1.amazonaws.com/".into(),
        region: "sa-east-1".into(),
        profile: Some("trabalho".into()),
        github_login: None,
        voice: current.voice.clone(),
    };

    candidate
        .save_at(&path)
        .expect("the scratch file is writable");
    let reloaded = Config::load_at(&path).expect("the saved file parses");

    assert_eq!(reloaded.region, "sa-east-1");
    assert_eq!(reloaded.profile.as_deref(), Some("trabalho"));

    // Every `[voice]` field, including the ones neither screen renders.
    assert_eq!(reloaded.voice.tts_engine.as_deref(), Some("polly"));
    assert_eq!(reloaded.voice.tts_voice.as_deref(), Some("Vitoria"));
    assert_eq!(reloaded.voice.tts_rate, 185);
    assert_eq!(
        reloaded.voice.answer_model.as_deref(),
        Some("us.anthropic.claude-haiku-4-5-20251001-v1:0"),
        "moving accounts must not silently turn answering off"
    );
    assert_eq!(reloaded.voice.follow_up_seconds, 6);
    assert_eq!(reloaded.voice.hangover_ms, 800);
    assert_eq!(reloaded.voice.unknown_voice, current.voice.unknown_voice);

    let _ = std::fs::remove_file(&path);
}

/// A `None` profile must stay absent rather than being written as something the
/// next load reads back as a profile named "".
#[test]
fn an_absent_profile_round_trips_as_absent() {
    let path = scratch_file("absent-profile");

    let config = Config::default();
    assert!(config.profile.is_none(), "the default names no profile");
    config.save_at(&path).expect("writable");

    let text = std::fs::read_to_string(&path).expect("readable");
    assert!(
        !text.contains("profile"),
        "an absent profile must not be written at all, got:\n{text}"
    );
    assert!(Config::load_at(&path).expect("parses").profile.is_none());

    let _ = std::fs::remove_file(&path);
}

/// A scratch path, without pulling in a temp-file crate for one filename.
///
/// `who` is the caller's name, and it is not decoration. The clock alone was
/// not enough: these tests run in parallel in one process, `SystemTime::now`
/// is coarse enough that two of them read the same value, and `save_at` stages
/// through `<path>.toml.tmp` — so a collision had one test renaming the other's
/// staging file away, which surfaced as a *third* test failing to write at all.
fn scratch_file(who: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "echobrain-config-{who}-{}-{unique}.toml",
        std::process::id()
    ))
}
