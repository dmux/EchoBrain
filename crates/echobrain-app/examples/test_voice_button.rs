//! "Testar voz", end to end, without the window.
//!
//! Runs the real composition root — `build_state()`, the same function the
//! Tauri app calls — behind Tauri's mock runtime with the **real** ACL context,
//! then invokes the two commands the button invokes, in its order, with the
//! payload the settings screen sends. Real config, real credentials, real
//! Polly: the audio comes out of the speakers.
//!
//! Everything between the button's `onClick` and the sound is therefore covered
//! here. What is not is the `onClick` itself — see the notes at the end.
//!
//! ```bash
//! env -u AWS_ENDPOINT_URL -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY \
//!   -u AWS_PROFILE cargo run -p echobrain-app --example test_voice_button
//! ```

use std::sync::Arc;

use echobrain_lib::config::Config;
use serde_json::{Value, json};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{Manager, WebviewWindowBuilder};

const PHRASE: &str = "Assim que eu vou ler as suas memórias.";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("echobrain=debug,info")
        .init();

    let before = Config::load().expect("the real config loads");
    println!("▸ config lido");
    println!("    profile = {:?}", before.profile);
    println!("    engine  = {:?}", before.voice.tts_engine);
    println!("    voice   = {:?}", before.voice.tts_voice);

    let state = tauri::async_runtime::block_on(echobrain_lib::build_state())
        .expect("the real composition root builds");

    // Exactly what the app reports to the UI, and what decides whether the
    // button is even offered.
    println!(
        "\n  speech_available seria {} (speaker presente: {})",
        state.speaker.is_some(),
        state.speaker.is_some()
    );

    let app = mock_builder()
        .invoke_handler(tauri::generate_handler![
            echobrain_lib::commands::speech_available,
            echobrain_lib::commands::available_voices,
            echobrain_lib::commands::get_settings,
            echobrain_lib::commands::set_settings,
            echobrain_lib::commands::speak,
        ])
        // The real context carries the generated ACL. A mock context has an
        // empty authority and would deny everything, proving nothing.
        .build(tauri::generate_context!())
        .expect("the app builds");
    app.manage(Arc::new(state));
    WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("the mock window builds");

    // 1. What the dialog does when it opens. If either of these fails the
    //    screen never renders its form and the button does not exist.
    let settings = step("get_settings (dialog abre)", |_| {
        invoke(&app, "get_settings", json!({}))
    });
    let voices = step("available_voices (habilita o botão)", |_| {
        invoke(&app, "available_voices", json!({}))
    });

    if let Some(list) = voices.as_ref().and_then(|v| v.as_array()) {
        let polly = list.iter().filter(|v| v["engine"] == "polly").count();
        let system = list.iter().filter(|v| v["engine"] == "system").count();
        println!(
            "    {} vozes ({} system, {} polly)",
            list.len(),
            system,
            polly
        );
        if list.is_empty() {
            println!("    >>> LISTA VAZIA: o botão ficaria desabilitado <<<");
        }
    }

    // 2. What the button does on click, in order.
    let Some(settings) = settings else {
        println!("\nabortado: sem settings o botão não existe na tela.");
        return;
    };

    step("set_settings (o botão salva antes de falar)", |_| {
        invoke(&app, "set_settings", json!({ "settings": settings }))
    });

    step("speak (o áudio sai aqui)", |_| {
        invoke(&app, "speak", json!({ "text": PHRASE }))
    });

    // 3. The save must not have cost anything. This is the failure that took
    //    the AWS profile out of the file and every credential with it.
    let after = Config::load().expect("the config still parses");
    println!("\n  config depois do save:");
    println!("    profile = {:?}", after.profile);
    if after.profile != before.profile {
        println!("    >>> O PERFIL MUDOU — o save derrubou credenciais <<<");
    } else {
        println!("    perfil preservado");
    }

    println!("\nSe você ouviu a frase, o caminho inteiro está bom e o defeito");
    println!("está no onClick do React. Se não ouviu, o erro está acima.");
}

/// Run a step, printing what it returned or exactly how it failed.
fn step(label: &str, body: impl FnOnce(()) -> Result<Value, Value>) -> Option<Value> {
    println!("\n▸ {label}");
    match body(()) {
        Ok(value) => {
            println!("  ok: {value}");
            Some(value)
        }
        Err(error) => {
            println!("  FALHOU: {error}");
            None
        }
    }
}

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
            url: "tauri://localhost".parse().expect("valid url"),
            body: args.into(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    );
    match response {
        Ok(body) => Ok(body.deserialize().unwrap_or(Value::Null)),
        Err(error) => Err(error),
    }
}
