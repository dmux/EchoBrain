//! Where the app learns which API to talk to, and as whom.
//!
//! # Why a file rather than the environment
//!
//! The sibling repository's MCP server reads `AGENT_MEMORY_API` and `AWS_REGION`
//! from its process environment, which is right for something a shell launches.
//! A `.app` opened from the Finder inherits no shell profile at all: both
//! variables are simply absent, and signing fails with `SigningError::NoRegion`
//! however carefully the machine is configured.
//!
//! So configuration is explicit and travels as a value into
//! `RemoteMemoryService::with_config`. Environment variables are still honoured
//! when present, because that is convenient during `tauri dev` — but they are
//! an override, not the mechanism.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A placeholder, not a working stack.
///
/// This is deliberately not anybody's real deployment. The memory API lives in
/// the caller's own AWS account — see the sibling `agent-memory` project — so a
/// default pointing at someone else's API Gateway would be useless to everyone
/// but its owner, and would tie this repository to one account.
///
/// Point it at your own with `endpoint` in `config.toml`, the *Conexão* screen,
/// or `AGENT_MEMORY_API`.
const DEFAULT_ENDPOINT: &str = "https://abc123xyz0.execute-api.us-east-1.amazonaws.com/";

/// The region of the **API**, which is what SigV4 signs for.
///
/// Deliberately not the region of the user's AWS profile. Signing with
/// `sa-east-1` because that is what `~/.aws/config` says, while the API lives in
/// `us-east-1`, produces a signature mismatch that reads like a credentials
/// problem and is not one.
const DEFAULT_REGION: &str = "us-east-1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub endpoint: String,
    pub region: String,
    /// Named AWS profile to sign with. `None` uses the default chain.
    pub profile: Option<String>,
    /// Attribution label written onto memories. Never used for authorization.
    pub github_login: Option<String>,
    /// Everything about speaking and answering. See [`VoiceConfig`].
    pub voice: VoiceConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            region: DEFAULT_REGION.to_string(),
            profile: None,
            github_login: None,
            voice: VoiceConfig::default(),
        }
    }
}

/// The `[voice]` section.
///
/// # Why `answer_model` has no default
///
/// Everything else here degrades: no voice installed means no speech, and the
/// app still works. Answering is different — it is the first thing EchoBrain
/// does that costs money, and a default would mean the app started spending on
/// the first question of a user who never asked for it. Bedrock model access is
/// also granted per account and per region, so any value shipped here would be
/// wrong for most accounts anyway.
///
/// Find the right one with:
///
/// ```text
/// aws bedrock list-inference-profiles --region us-east-1 \
///   --query 'inferenceProfileSummaries[?contains(inferenceProfileId, `anthropic`)].inferenceProfileId'
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// 'system' or 'polly'. Defaults to 'system'.
    pub tts_engine: Option<String>,
    /// macOS system voice or Polly voice ID. `None` resolves to `Luciana`, then to whatever
    /// Portuguese voice is installed, then to no speech at all.
    pub tts_voice: Option<String>,
    /// Words per minute.
    pub tts_rate: u16,
    /// Bedrock inference profile id. `None` disables answering entirely.
    pub answer_model: Option<String>,
    /// Output ceiling per answer. Not the length of the reply — the prompt
    /// governs that — but a runaway guard that also has to leave room for the
    /// model's own reasoning tokens.
    pub answer_max_tokens: i32,
    /// The similarity floor below which a question is answered with "nothing
    /// found" instead of being handed to the model. The single most important
    /// setting against a confident wrong answer; see
    /// [`echobrain_core::DEFAULT_MIN_SIMILARITY`].
    pub min_similarity: f32,
    /// How much silence ends a spoken turn, in milliseconds.
    ///
    /// Short enough that the answer does not feel delayed, long enough that the
    /// beat in "então… a gente decidiu" is not read as the end of the sentence.
    /// Erring long is the safer direction: cutting someone off mid-thought
    /// loses what they were saying.
    pub hangover_ms: u32,
    /// How long the microphone stays open after an answer, in seconds, waiting
    /// for a follow-up before closing.
    pub follow_up_seconds: u32,
    /// What to do in conversation mode with a voice that does not match the
    /// enrolled profile. Defaults to allowing everything: filtering must be
    /// something the user turned on after seeing their own score, never
    /// something that silently starts discarding speech.
    pub unknown_voice: String,
    /// Acceptance threshold on `0.0..=1.0`. Shown as a number, because the
    /// right value depends on the room and the only way to pick it is to see
    /// your own.
    pub voice_threshold: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            tts_engine: None,
            tts_voice: None,
            tts_rate: echobrain_audio::DEFAULT_RATE,
            answer_model: None,
            answer_max_tokens: echobrain_llm::DEFAULT_MAX_TOKENS,
            min_similarity: echobrain_core::DEFAULT_MIN_SIMILARITY,
            hangover_ms: 800,
            follow_up_seconds: 0,
            unknown_voice: echobrain_core::UnknownVoice::default().as_str().to_string(),
            voice_threshold: echobrain_core::VoiceProfile::DEFAULT_THRESHOLD,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid TOML")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not write {path}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} could not be serialised")]
    Serialise {
        path: String,
        #[source]
        source: toml::ser::Error,
    },

    #[error("no home directory, so there is nowhere to save settings")]
    NoHome,
}

impl Config {
    /// `~/Library/Application Support/EchoBrain/config.toml` on macOS.
    pub fn path() -> Option<PathBuf> {
        dirs_config_dir().map(|dir| dir.join("EchoBrain").join("config.toml"))
    }

    /// Load, falling back to defaults when the file is absent.
    ///
    /// A missing file is the normal first-run state, not a failure. A malformed
    /// one *is* a failure: silently substituting defaults would point the app at
    /// a different backend than the user asked for.
    pub fn load() -> Result<Self, ConfigError> {
        let Some(path) = Self::path() else {
            return Ok(Self::with_env_overrides(Self::default()));
        };

        let config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source,
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.display().to_string(),
                    source,
                });
            }
        };

        Ok(Self::with_env_overrides(config))
    }

    /// Write the config back, atomically.
    ///
    /// # Why the temporary file and rename
    ///
    /// [`Self::load`] treats a malformed file as a hard error rather than
    /// silently falling back to defaults — deliberately, because defaulting
    /// would point the app at a different backend than the user asked for. That
    /// makes a half-written file fatal: the app would refuse to start, and the
    /// only way out would be editing TOML by hand.
    ///
    /// Writing to a sibling path and renaming makes the swap atomic on the same
    /// filesystem, so a crash mid-save leaves the previous config intact.
    pub fn save(&self) -> Result<(), ConfigError> {
        let Some(path) = Self::path() else {
            return Err(ConfigError::NoHome);
        };
        self.save_at(&path)
    }

    /// [`Self::save`] with the destination named.
    ///
    /// Exists so the round trip can be tested against a scratch directory.
    /// Without it the only way to exercise saving is to overwrite the real
    /// `config.toml`, which a test run did once — dropping `profile` and taking
    /// every AWS credential with it.
    pub fn save_at(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.display().to_string(),
                source,
            })?;
        }

        let text = toml::to_string_pretty(self).map_err(|source| ConfigError::Serialise {
            path: path.display().to_string(),
            source,
        })?;

        let staging = path.with_extension("toml.tmp");
        std::fs::write(&staging, text).map_err(|source| ConfigError::Write {
            path: staging.display().to_string(),
            source,
        })?;
        std::fs::rename(&staging, path).map_err(|source| ConfigError::Write {
            path: path.display().to_string(),
            source,
        })?;

        Ok(())
    }

    /// Read a config from a named file, without the environment overrides
    /// [`Self::load`] applies. The other half of [`Self::save_at`].
    pub fn load_at(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// `~/Library/Application Support/EchoBrain/voice.json`
    pub fn profile_path() -> Option<PathBuf> {
        dirs_config_dir().map(|dir| dir.join("EchoBrain").join("voice.json"))
    }

    /// `~/Library/Application Support/EchoBrain/echobrain.log`
    ///
    /// A `.app` opened from the Finder has no terminal attached: stdout and
    /// stderr go nowhere, so every `tracing::warn!` in this codebase is
    /// invisible in exactly the build a user runs. A failure then reaches the
    /// developer as "it stopped working", with the message that would have
    /// explained it already discarded.
    pub fn log_path() -> Option<PathBuf> {
        dirs_config_dir().map(|dir| dir.join("EchoBrain").join("echobrain.log"))
    }

    /// `~/Library/Application Support/EchoBrain/journal.sqlite3`
    pub fn journal_path() -> Option<PathBuf> {
        dirs_config_dir().map(|dir| dir.join("EchoBrain").join("journal.sqlite3"))
    }

    pub fn load_voice_profile() -> Result<Option<echobrain_core::VoiceProfile>, ConfigError> {
        let Some(path) = Self::profile_path() else {
            return Ok(None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|source| ConfigError::Read {
                    path: path.display().to_string(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
                })
                .map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ConfigError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn save_voice_profile(profile: &echobrain_core::VoiceProfile) -> Result<(), ConfigError> {
        let Some(path) = Self::profile_path() else {
            return Err(ConfigError::NoHome);
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let text = serde_json::to_string(profile).map_err(|source| ConfigError::Write {
            path: path.display().to_string(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        })?;
        std::fs::write(&path, text).map_err(|source| ConfigError::Write {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }

    /// Environment overrides, for `tauri dev` from a configured shell.
    ///
    /// Note what this does *not* do: read `AWS_ENDPOINT_URL`. A machine pointed
    /// at LocalStack would otherwise silently redirect signing, and the failure
    /// surfaces as a 403 from the real API rather than as anything that mentions
    /// LocalStack.
    fn with_env_overrides(mut self) -> Self {
        if let Ok(endpoint) = std::env::var("AGENT_MEMORY_API")
            && !endpoint.trim().is_empty()
        {
            self.endpoint = endpoint;
        }
        if let Ok(region) = std::env::var("ECHOBRAIN_AWS_REGION")
            && !region.trim().is_empty()
        {
            self.region = region;
        }
        if let Ok(profile) = std::env::var("AWS_PROFILE")
            && !profile.trim().is_empty()
        {
            self.profile = Some(profile);
        }
        self
    }
}

/// The environment variables [`Config::with_env_overrides`] honours, paired
/// with the field each one displaces.
///
/// Exposed so the settings screen can say *this box is being overridden* rather
/// than appearing to save a value the next launch will discard. `make run`
/// exports `AWS_PROFILE`, so this is the common case rather than the exotic one.
pub const ENV_OVERRIDES: [(&str, &str); 3] = [
    ("AGENT_MEMORY_API", "endpoint"),
    ("ECHOBRAIN_AWS_REGION", "region"),
    ("AWS_PROFILE", "profile"),
];

/// Which of [`ENV_OVERRIDES`] are set in this process, by variable name.
pub fn active_env_overrides() -> Vec<String> {
    ENV_OVERRIDES
        .iter()
        .filter(|(variable, _)| std::env::var(variable).is_ok_and(|value| !value.trim().is_empty()))
        .map(|(variable, _)| variable.to_string())
        .collect()
}

/// Profile names from the text of an AWS config or credentials file.
///
/// The two files spell the same thing differently — `~/.aws/config` uses
/// `[profile name]` (with a bare `[default]` for the default one) while
/// `~/.aws/credentials` uses `[name]` — so both forms are accepted and the
/// caller merges the results.
///
/// Pure, and separate from [`aws_profile_names`], because the interesting cases
/// are all about parsing and none of them are about the filesystem.
pub fn parse_profile_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with(';'))
        .filter_map(|line| line.strip_prefix('[')?.strip_suffix(']'))
        .map(|section| section.trim().strip_prefix("profile ").unwrap_or(section))
        .map(str::trim)
        // `[sso-session foo]` and `[services bar]` are configuration blocks,
        // not profiles. Offering them in the picker would produce a profile
        // name the credential chain has never heard of.
        .filter(|name| !name.is_empty() && !name.contains(' '))
        .map(str::to_string)
        .collect()
}

/// Every profile this machine declares, deduplicated and sorted.
///
/// Empty is a normal answer, not a failure: a machine configured purely through
/// environment variables, or one where the SDK was never set up by hand, has
/// nothing to list. The settings screen falls back to a free-text field rather
/// than offering an empty menu.
pub fn aws_profile_names() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let aws = home.join(".aws");

    let mut names: Vec<String> = ["config", "credentials"]
        .iter()
        .filter_map(|file| std::fs::read_to_string(aws.join(file)).ok())
        .flat_map(|text| parse_profile_names(&text))
        .collect();

    names.sort();
    names.dedup();
    names
}

/// `~/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` elsewhere.
///
/// Hand-rolled rather than pulling in the `dirs` crate for one path: this is the
/// only place the app touches the filesystem layout.
fn dirs_config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".config"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_the_deployed_stack_in_its_own_region() {
        let config = Config::default();
        assert!(config.endpoint.contains("execute-api"));
        assert_eq!(
            config.region, "us-east-1",
            "the signing region must be the API's, not the user's profile region"
        );
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_for_everything_else() {
        let config: Config =
            toml::from_str(r#"github_login = "dmux""#).expect("partial config parses");
        assert_eq!(config.github_login.as_deref(), Some("dmux"));
        assert_eq!(config.region, "us-east-1");
        assert!(config.endpoint.contains("execute-api"));
    }

    #[test]
    fn the_voice_section_round_trips_through_toml() {
        // Settings are written by a UI and read back at launch. A field that
        // serialises but does not parse would be lost on the next start, and
        // nothing else in the app would notice.
        let mut config = Config::default();
        config.voice.tts_engine = Some("system".into());
        config.voice.tts_voice = Some("Luciana".into());
        config.voice.tts_rate = 200;
        config.voice.answer_model = Some("us.anthropic.claude-opus-5".into());
        config.voice.min_similarity = 0.42;

        let text = toml::to_string_pretty(&config).expect("serialises");
        let parsed: Config = toml::from_str(&text).expect("parses back");

        assert_eq!(parsed.voice.tts_engine.as_deref(), Some("system"));
        assert_eq!(parsed.voice.tts_voice.as_deref(), Some("Luciana"));
        assert_eq!(parsed.voice.tts_rate, 200);
        assert_eq!(
            parsed.voice.answer_model.as_deref(),
            Some("us.anthropic.claude-opus-5")
        );
        assert!((parsed.voice.min_similarity - 0.42).abs() < f32::EPSILON);
    }

    #[test]
    fn answering_is_absent_by_default() {
        // The one feature that spends money must not be on until it is asked
        // for by name. Everything else here degrades quietly; this bills.
        assert!(
            Config::default().voice.answer_model.is_none(),
            "a default answering model would spend on the first question of a \
             user who never configured one"
        );
    }

    #[test]
    fn a_config_without_a_voice_section_still_parses() {
        // Every config.toml written before this section existed.
        let config: Config = toml::from_str(r#"github_login = "dmux""#).expect("parses");
        assert_eq!(config.voice.tts_rate, echobrain_audio::DEFAULT_RATE);
        assert!(config.voice.answer_model.is_none());
    }

    #[test]
    fn both_spellings_of_a_profile_section_are_recognised() {
        // `~/.aws/config` says `[profile x]`; `~/.aws/credentials` says `[x]`.
        // A picker that understood only one would come up empty on machines
        // configured the other way, which reads as "no profiles exist".
        let names = parse_profile_names(
            r#"
            [default]
            region = us-east-1

            [profile my-profile]
            sso_session = corp

            [work]
            aws_access_key_id = AKIA...
            "#,
        );

        assert_eq!(names, vec!["default", "my-profile", "work"]);
    }

    #[test]
    fn non_profile_sections_are_not_offered_as_profiles() {
        // `[sso-session corp]` is a block a profile *refers to*, not one the
        // credential chain can be pointed at. Listing it would hand the user a
        // name that fails with "profile not found" and looks like a bug here.
        let names = parse_profile_names(
            r#"
            [sso-session corp]
            sso_start_url = https://example.awsapps.com/start

            [services local]
            dynamodb =

            [profile real]
            "#,
        );

        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn a_commented_out_profile_is_not_a_profile() {
        let names = parse_profile_names("# [profile old]\n; [profile older]\n[profile live]\n");
        assert_eq!(names, vec!["live"]);
    }

    #[test]
    fn a_file_without_any_sections_yields_nothing_rather_than_failing() {
        // The empty answer has to be reachable: it is what a machine with no
        // `~/.aws` returns, and the settings screen switches to a free-text
        // field on it.
        assert!(parse_profile_names("").is_empty());
        assert!(parse_profile_names("region = us-east-1\n").is_empty());
    }

    #[test]
    fn a_full_file_overrides_every_default() {
        let config: Config = toml::from_str(
            r#"
            endpoint = "https://example.com/"
            region = "sa-east-1"
            profile = "my-profile"
            "#,
        )
        .expect("config parses");

        assert_eq!(config.endpoint, "https://example.com/");
        assert_eq!(config.region, "sa-east-1");
        assert_eq!(config.profile.as_deref(), Some("my-profile"));
    }
}
