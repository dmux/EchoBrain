//! What crosses the IPC boundary.
//!
//! Mirrors `ui/src/lib/memory/types.ts`. Kept as a separate layer from the
//! domain for the same reason `agent-memory-contract` is separate from
//! `agent-memory-core`: the wire format and the domain must be able to change
//! for different reasons.
//!
//! Note what is absent: `Distance`. The UI receives `similarity` only, already
//! inverted by `echobrain-core`, so no view can render the metric backwards.

use std::time::{SystemTime, UNIX_EPOCH};

use echobrain_core::{Answer, EchoError, Hit, JournalEntry, Memory};
use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize a field that has to tell "absent" from "explicitly null".
///
/// `Option<Option<T>>` alone does not do it: serde's own `Option` impl maps a
/// JSON `null` to the *outer* `None`, so a null and a missing key arrive
/// identically. Only `#[serde(default, deserialize_with = "double_option")]`
/// separates them — the default supplies `None` when the key is absent, and
/// this wraps whatever was actually present in `Some`.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

use crate::config::VoiceConfig;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDto {
    pub memory_id: String,
    pub kind: String,
    pub text: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub source: Option<String>,
    pub github_login: Option<String>,
    #[serde(rename = "githubRepo")]
    pub github_repo: Option<String>,
    pub rating: Option<u8>,
    pub active: bool,
}

/// What the library view narrows a listing by. All optional, all "and".
///
/// Metadata only. Filtering by repository or by rating is a filing question;
/// finding a memory by what it *says* is `search`, and no combination of these
/// substitutes for it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFilterDto {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub github_repo: Option<String>,
    #[serde(default)]
    pub github_login: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub min_rating: Option<u8>,
    #[serde(default)]
    pub text_contains: Option<String>,
}

impl TryFrom<MemoryFilterDto> for echobrain_core::MemoryFilter {
    type Error = IpcError;

    fn try_from(dto: MemoryFilterDto) -> Result<Self, Self::Error> {
        /// A field the screen left blank narrows nothing. Without this, an
        /// empty text box would filter for memories whose repository is the
        /// empty string, and the library would go blank the moment someone
        /// typed into a filter and deleted it again.
        fn present(value: Option<String>) -> Option<String> {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }

        Ok(Self {
            kind: present(dto.kind)
                .map(|kind| kind.parse::<echobrain_core::MemoryKind>())
                .transpose()
                .map_err(|error| IpcError::invalid(error.to_string()))?,
            github_repo: present(dto.github_repo),
            github_login: present(dto.github_login),
            source: present(dto.source),
            active: dto.active,
            // Zero stars is the UI's way of saying "unrated", and as a floor it
            // would mean "any", which is what no filter already means.
            min_rating: dto.min_rating.filter(|rating| *rating > 0),
            text_contains: present(dto.text_contains),
        })
    }
}

/// One page of the library.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryPageDto {
    pub memories: Vec<MemoryDto>,
    /// Pass back as `cursor` for the next page. `null` is the end — and the
    /// only signal for it. A short page means the filter dropped rows, not that
    /// the list is over, so a scroll that stops on one shows a truncated
    /// library over a full namespace.
    pub next_cursor: Option<String>,
}

/// A partial edit from the library view. An absent field is one the screen is
/// not touching — rating a memory must not overwrite its text with whatever
/// copy the row happened to be holding.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMemoryDto {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    /// The one field where absent and `null` differ: absent leaves the rating
    /// alone, `null` clears it. Un-starring sends `null`, and without
    /// [`double_option`] serde folds it onto "no change" and the star comes
    /// straight back.
    #[serde(default, deserialize_with = "double_option")]
    pub rating: Option<Option<u8>>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitDto {
    pub memory: MemoryDto,
    /// 0..=1, where 1 is identical.
    pub similarity: f32,
}

/// Flat rather than nested, matching the TypeScript
/// `interface JournalEntry extends Memory`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryDto {
    pub memory_id: String,
    pub kind: String,
    pub text: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub source: Option<String>,
    pub github_login: Option<String>,
    #[serde(rename = "githubRepo")]
    pub github_repo: Option<String>,
    pub state: String,
    pub rating: Option<u8>,
    pub active: bool,
}

/// What one dictation produced. Not stored yet — the user confirms first.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationDto {
    pub text: String,
    pub language: Option<String>,
    /// How long the recording was, so the UI can tell a real utterance from a
    /// stray hotkey press.
    pub seconds: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResultDto {
    pub entry: JournalEntryDto,
    pub journal_failed: Option<String>,
}

/// A composed answer and the memories behind it.
///
/// `cited` is not optional on the wire for the same reason it is not optional
/// in the domain: this is the only text EchoBrain returns that nobody dictated,
/// and every view that renders it has to be able to render its sources without
/// asking for them separately. See [`echobrain_core::Answer`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerDto {
    pub text: String,
    pub cited: Vec<HitDto>,
    /// False when no memory supported the answer — the UI shows this as an
    /// empty state rather than as a result with zero sources.
    pub grounded: bool,
    /// Whether a model was called. Distinguishes "nothing cleared the floor,
    /// nothing was spent" from "the model read them and declined", which look
    /// identical otherwise and cost very differently.
    pub consulted: bool,
}

impl From<Answer> for AnswerDto {
    fn from(answer: Answer) -> Self {
        Self {
            text: answer.text,
            cited: answer.cited.into_iter().map(HitDto::from).collect(),
            grounded: answer.grounded,
            consulted: answer.consulted,
        }
    }
}

/// A system voice the settings screen can offer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceDto {
    pub name: String,
    pub locale: String,
    pub engine: String,
}

/// The `[voice]` settings, both directions.
///
/// `Deserialize` as well as `Serialize` because this one crosses *into* Rust —
/// it is the only DTO in this file that does.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSettingsDto {
    pub tts_engine: Option<String>,
    pub tts_voice: Option<String>,
    pub tts_rate: u16,
    /// `None` disables answering entirely, which is the default and is a
    /// supported way to run: capture and search never cost a token.
    pub answer_model: Option<String>,
    pub answer_max_tokens: i32,
    /// The similarity floor. Shown as a number in the UI, not hidden behind a
    /// word, because it is the single dial that decides whether the app can
    /// compose a confident answer out of unrelated memories.
    pub min_similarity: f32,
    pub voice_threshold: f32,
    /// "allow" or "ignore". What to do in conversation mode with a voice that
    /// does not match the enrolled profile. Crossed the boundary late: the
    /// setting existed in `config.toml` from the start but had no control, so
    /// enrolling a voice changed nothing anyone could see.
    pub unknown_voice: String,
}

/// Which AWS account the app talks to, and as whom.
///
/// Everything the connection screen needs in one round trip — including the
/// lists it renders — so it can never draw itself half configured, with a
/// profile selected out of a menu that has not arrived yet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDto {
    pub endpoint: String,
    pub region: String,
    pub profile: Option<String>,
    pub github_login: Option<String>,
    #[serde(rename = "githubRepo")]
    pub github_repo: Option<String>,

    /// Names found in `~/.aws/config` and `~/.aws/credentials`.
    ///
    /// Empty is a normal answer, not a failure — a machine configured purely
    /// through environment variables has no `~/.aws` — and the screen falls back
    /// to a free-text field rather than showing an empty menu that looks broken.
    pub profiles: Vec<String>,

    /// Environment variables set in this process that outrank the saved file at
    /// the *next* launch. See [`crate::config::ENV_OVERRIDES`].
    ///
    /// On the wire because the alternative is a screen that appears to save a
    /// profile and then silently loses it on restart — `make run` exports
    /// `AWS_PROFILE`, so this is the ordinary case rather than the exotic one.
    pub env_overrides: Vec<String>,

    /// The SSO session behind the selected profile, when there is one.
    ///
    /// `None` for a profile that does not use SSO, and for one nobody has
    /// logged into yet. Neither is a problem to report, so the screen shows
    /// nothing at all rather than an alarming blank.
    pub sso: Option<SsoStatusDto>,
}

/// How long the login lasts, and whether it renews itself.
///
/// The second field is the one worth crossing the boundary for: it is decided
/// by the shape of `~/.aws/config`, it cannot be seen from anywhere else, and
/// it separates a session that survives the working day from one that dies in
/// eight hours. See [`crate::sso`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoStatusDto {
    /// The `[sso-session]` name, or `None` for the legacy profile shape.
    pub session: Option<String>,
    /// Unix seconds, so the screen can count down without asking again.
    pub expires_at: i64,
    /// Seconds remaining. Negative once expired, which the screen renders
    /// differently — an expired session is an action, not a countdown.
    pub expires_in: i64,
    pub renewable: bool,
}

impl From<crate::sso::SsoStatus> for SsoStatusDto {
    fn from(status: crate::sso::SsoStatus) -> Self {
        Self {
            session: status.session,
            expires_at: status.expires_at,
            expires_in: status.expires_in,
            renewable: status.renewable,
        }
    }
}

/// The editable half of [`ConnectionDto`]. Crosses *into* Rust.
///
/// `profiles` and `env_overrides` are deliberately absent: they are facts about
/// the machine, and accepting them back would let the frontend claim an
/// override was gone when it was not.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSettingsDto {
    pub endpoint: String,
    pub region: String,
    pub profile: Option<String>,
    pub github_login: Option<String>,
    #[serde(rename = "githubRepo")]
    pub github_repo: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceProfileDto {
    pub enrolled_from: usize,
    pub enrolled_at: u64,
}

impl From<echobrain_core::VoiceProfile> for VoiceProfileDto {
    fn from(profile: echobrain_core::VoiceProfile) -> Self {
        Self {
            enrolled_from: profile.enrolled_from,
            enrolled_at: profile
                .enrolled_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

impl From<&crate::config::Config> for ConnectionDto {
    fn from(config: &crate::config::Config) -> Self {
        Self {
            endpoint: config.endpoint.clone(),
            region: config.region.clone(),
            profile: config.profile.clone(),
            github_login: config.github_login.clone(),
            github_repo: None,
            profiles: crate::config::aws_profile_names(),
            env_overrides: crate::config::active_env_overrides(),
            // `default` when no profile is named, because that is the section
            // the default credential chain reads — a machine whose `[default]`
            // does SSO has a session to report like any other.
            sso: crate::sso::status(config.profile.as_deref().unwrap_or("default"))
                .map(SsoStatusDto::from),
        }
    }
}

impl From<&VoiceConfig> for VoiceSettingsDto {
    fn from(config: &VoiceConfig) -> Self {
        Self {
            tts_engine: config.tts_engine.clone(),
            tts_voice: config.tts_voice.clone(),
            tts_rate: config.tts_rate,
            answer_model: config.answer_model.clone(),
            answer_max_tokens: config.answer_max_tokens,
            min_similarity: config.min_similarity,
            voice_threshold: config.voice_threshold,
            unknown_voice: config.unknown_voice.clone(),
        }
    }
}

/// A failure the UI can act on.
///
/// `invalid_input` is the same discriminator the memory domain uses, carried
/// across the boundary so the frontend can tell "fix your query" from "the
/// backend is unreachable" without parsing message strings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub message: String,
    pub invalid_input: bool,
}

impl From<EchoError> for IpcError {
    fn from(error: EchoError) -> Self {
        Self {
            invalid_input: error.is_invalid_input(),
            // `Display` on the outer error omits sources, which is what a user
            // should see. The full chain goes to `tracing` instead.
            message: error.to_string(),
        }
    }
}

impl IpcError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            invalid_input: true,
        }
    }

    /// A failure from the memory API, with its cause chain preserved.
    ///
    /// `MemoryError::Repository` displays as the bare words "repository
    /// failure" — the HTTP status, the API's error code and its message all
    /// live in `source()`, which `Display` does not walk. Reported as-is, a 404
    /// from a route that was never deployed and a 403 from an expired login are
    /// the same three words on screen and nothing in the log.
    ///
    /// So the chain is flattened into the message rather than dropped. These
    /// strings are technical, but the alternative is a user who cannot tell you
    /// what went wrong and a log that cannot either.
    fn from_chain(error: &dyn std::error::Error, invalid_input: bool) -> Self {
        let mut message = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }

        Self {
            message,
            invalid_input,
        }
    }
}

impl From<echobrain_core::MemoryError> for IpcError {
    fn from(error: echobrain_core::MemoryError) -> Self {
        // The domain already knows which failures are the caller's fault, so
        // the flag is read from it rather than hardcoded at each call site —
        // where every one of them said "not your fault", including for a
        // rejected rating.
        let reported = Self::from_chain(&error, error.is_invalid_input());
        if !reported.invalid_input {
            tracing::warn!(message = %reported.message, "the memory API call failed");
        }
        reported
    }
}

impl From<Memory> for MemoryDto {
    fn from(memory: Memory) -> Self {
        Self {
            memory_id: memory.memory_id.into_inner(),
            kind: memory.kind.as_str().to_string(),
            text: memory.text,
            created_at: unix_seconds(memory.created_at),
            expires_at: memory.expires_at.map(unix_seconds),
            source: memory.source,
            github_login: memory.github_login,
            github_repo: memory.github_repo,
            rating: memory.rating,
            active: memory.active,
        }
    }
}

impl From<Hit> for HitDto {
    fn from(hit: Hit) -> Self {
        Self {
            similarity: hit.similarity.get(),
            memory: hit.memory.into(),
        }
    }
}

impl From<JournalEntry> for JournalEntryDto {
    fn from(entry: JournalEntry) -> Self {
        Self {
            memory_id: entry.memory_id.into_inner(),
            kind: entry.kind.as_str().to_string(),
            text: entry.text,
            created_at: unix_seconds(entry.created_at),
            expires_at: entry.expires_at.map(unix_seconds),
            source: Some(entry.source),
            github_login: entry.github_login,
            github_repo: entry.github_repo,
            state: entry.state.as_str().to_string(),
            rating: entry.rating,
            active: entry.active,
        }
    }
}

/// Clamped at the epoch rather than propagating an error: a clock behind 1970
/// is a broken machine, not a reason to fail a search.
fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_format_is_camel_case_so_typescript_needs_no_mapping() {
        let dto = JournalEntryDto {
            memory_id: "mem-1".into(),
            kind: "preference".into(),
            text: "café coado".into(),
            created_at: 1_700_000_000,
            expires_at: None,
            source: Some("echobrain-macos".into()),
            github_login: None,
            github_repo: None,
            state: "indexing".into(),
            rating: None,
            active: true,
        };

        let json = serde_json::to_value(&dto).expect("serialises");
        assert_eq!(json["memoryId"], "mem-1");
        assert_eq!(json["createdAt"], 1_700_000_000u64);
        assert_eq!(json["state"], "indexing");
        assert!(
            json.get("memory_id").is_none(),
            "snake_case must not leak to the frontend"
        );
    }

    #[test]
    fn an_empty_utterance_crosses_as_invalid_input_not_a_system_failure() {
        let error = IpcError::from(EchoError::EmptyUtterance);
        assert!(error.invalid_input);

        let json = serde_json::to_value(&error).expect("serialises");
        assert_eq!(json["invalidInput"], true);
    }

    #[test]
    fn a_clock_before_the_epoch_clamps_instead_of_panicking() {
        assert_eq!(unix_seconds(UNIX_EPOCH), 0);
        assert_eq!(
            unix_seconds(UNIX_EPOCH - std::time::Duration::from_secs(60)),
            0
        );
    }
}
