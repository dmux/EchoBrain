//! Every argument key the frontend sends must be a parameter the command has.
//!
//! # The bug this exists for
//!
//! `TauriGateway.setSettings` sent `{ update: settings }` while the command
//! signature read `set_settings(state, settings)`. Tauri rejects that while
//! *binding arguments* — before the command body runs — so no `tracing` line
//! inside the command ever fired, nothing was written to `config.toml`, and
//! "Testar voz" never reached `speak` because it awaits the save first. From
//! the outside: a button that does nothing, in silence.
//!
//! # Why the IPC tests did not catch it
//!
//! They hand-write the payload. `ipc.rs` invoked `set_settings` with
//! `json!({ "settings": … })` — the shape the Rust side wants — and passed,
//! while the shape the frontend actually sends was wrong. A test that restates
//! the contract cannot detect the caller disagreeing with it, so this one reads
//! the real call sites out of `tauri.ts` instead.
//!
//! Static, like `every_command_is_named_in_all_three_places`, and for the same
//! reason: invoking every command for real would mean side effects no test
//! should have.

/// Parameters that are not arguments: Tauri injects them.
const INJECTED: [&str; 3] = ["state", "app", "window"];

#[test]
fn every_argument_the_frontend_sends_is_a_parameter_the_command_has() {
    let gateway = include_str!("../../../ui/src/lib/memory/tauri.ts");
    let commands = include_str!("../src/commands.rs");

    let call_sites = call_sites(gateway);
    assert!(
        call_sites.len() > 15,
        "the call sites in tauri.ts were not parsed: {call_sites:?}"
    );

    for (command, keys) in &call_sites {
        let Some(parameters) = parameters_of(commands, command) else {
            panic!("`{command}` is called from tauri.ts but is not a command in commands.rs");
        };

        for key in keys {
            let expected = snake_case(key);
            assert!(
                parameters.iter().any(|(name, _)| name == &expected),
                "tauri.ts sends `{key}` to `{command}`, which has no such parameter \
                 (it takes {parameters:?}). Tauri rejects this while binding arguments, \
                 before the command body runs — so nothing logs and nothing happens."
            );
        }

        // The other direction, for the arguments serde cannot default. An
        // `Option<T>` may be omitted; anything else must be sent or the call is
        // rejected with "missing required key".
        for (name, optional) in &parameters {
            if *optional || INJECTED.contains(&name.as_str()) {
                continue;
            }
            assert!(
                keys.iter().any(|key| &snake_case(key) == name),
                "`{command}` requires `{name}`, which tauri.ts never sends \
                 (it sends {keys:?}) — the call fails with \"missing required key\"."
            );
        }
    }
}

/// `("command_name", { a, b: …, c })` out of every `call(…)` in the gateway,
/// including the ones whose object spans several lines.
fn call_sites(gateway: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    let bytes = gateway.as_bytes();
    let mut at = 0;

    while let Some(offset) = gateway[at..].find("call") {
        let start = at + offset;
        at = start + 4;

        // Skip the turbofish on `call<Hit[]>("…")`, then require `("`.
        let mut cursor = start + 4;
        if bytes.get(cursor) == Some(&b'<') {
            let Some(end) = gateway[cursor..].find('>') else {
                continue;
            };
            cursor += end + 1;
        }
        if bytes.get(cursor) != Some(&b'(') {
            continue;
        }
        cursor += 1;
        if bytes.get(cursor) != Some(&b'"') {
            continue;
        }
        cursor += 1;

        let Some(end) = gateway[cursor..].find('"') else {
            continue;
        };
        let command = gateway[cursor..cursor + end].to_string();
        cursor += end + 1;

        let Some(brace) = gateway[cursor..].find('{') else {
            continue;
        };
        let object_start = cursor + brace;
        let Some(object_end) = matching_brace(gateway, object_start) else {
            continue;
        };

        found.push((command, object_keys(&gateway[object_start + 1..object_end])));
        at = object_end;
    }

    found
}

/// Sliced rather than `skip(open)`: `open` is a byte offset and `skip` counts
/// characters, which diverge the moment the file contains an accent — and this
/// one is full of Portuguese.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in text[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The keys of an object literal body, ignoring the values — which may
/// themselves contain commas, `?.` chains and nested objects.
fn object_keys(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();

    for character in body.chars() {
        match character {
            '{' | '(' | '[' => {
                depth += 1;
                current.push(character);
            }
            '}' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ',' if depth == 0 => {
                push_key(&mut keys, &current);
                current.clear();
            }
            _ => current.push(character),
        }
    }
    push_key(&mut keys, &current);

    keys
}

fn push_key(keys: &mut Vec<String>, entry: &str) {
    // `key: value` and the shorthand `key` both name the key before any colon.
    let name = entry.split(':').next().unwrap_or("").trim();
    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        keys.push(name.to_string());
    }
}

/// `(name, is_option)` for each parameter of `pub fn name(…)` in commands.rs.
fn parameters_of(commands: &str, command: &str) -> Option<Vec<(String, bool)>> {
    // The name, then the parameter list — with any generics skipped, because
    // `start_conversation<R: tauri::Runtime>(…)` is still a command.
    let after_name = [
        format!("pub async fn {command}"),
        format!("pub fn {command}"),
    ]
    .into_iter()
    .find_map(|needle| commands.find(&needle).map(|at| at + needle.len()))?;

    let rest = &commands[after_name..];
    let opens = rest.find('(')?;
    // Anything between the name and `(` must be a generic list, or this was a
    // different function whose name merely starts the same way.
    if !rest[..opens].trim_start().is_empty() && !rest.trim_start().starts_with('<') {
        return None;
    }
    let signature = after_name + opens + 1;

    let end = matching_paren(commands, signature)?;
    let mut parameters = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();

    for character in commands[signature..end].chars() {
        match character {
            '<' | '(' | '[' => {
                depth += 1;
                current.push(character);
            }
            '>' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(character);
            }
            ',' if depth == 0 => {
                push_parameter(&mut parameters, &current);
                current.clear();
            }
            _ => current.push(character),
        }
    }
    push_parameter(&mut parameters, &current);

    Some(parameters)
}

fn matching_paren(text: &str, after_open: usize) -> Option<usize> {
    let mut depth = 1usize;
    for (index, character) in text[after_open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after_open + index);
                }
            }
            _ => {}
        }
    }
    None
}

fn push_parameter(parameters: &mut Vec<(String, bool)>, entry: &str) {
    let entry = entry.trim();
    if entry.is_empty() {
        return;
    }
    let Some((name, type_name)) = entry.split_once(':') else {
        return;
    };
    parameters.push((
        name.trim().to_string(),
        type_name.trim_start().starts_with("Option<"),
    ));
}

fn snake_case(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for character in camel.chars() {
        if character.is_ascii_uppercase() {
            out.push('_');
            out.push(character.to_ascii_lowercase());
        } else {
            out.push(character);
        }
    }
    out
}

#[test]
fn the_camel_case_conversion_matches_what_tauri_does() {
    assert_eq!(snake_case("topK"), "top_k");
    assert_eq!(snake_case("minSimilarity"), "min_similarity");
    assert_eq!(snake_case("ttlSeconds"), "ttl_seconds");
    assert_eq!(snake_case("settings"), "settings");
}
