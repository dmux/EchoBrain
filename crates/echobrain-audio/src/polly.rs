use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_polly::types::{Engine, OutputFormat, VoiceId};
use echobrain_core::{EchoError, Speaker};

const AFPLAY: &str = "/usr/bin/afplay";
const POLL: Duration = Duration::from_millis(40);

pub const DEFAULT_POLLY_VOICE: &str = "Vitoria";

#[derive(Debug, thiserror::Error)]
pub enum PollyError {
    #[error("could not start {AFPLAY} — is this macOS?")]
    Spawn(#[source] std::io::Error),

    #[error("the playback process failed")]
    Wait(#[source] std::io::Error),

    #[error("Polly API error: {0}")]
    Api(String),

    #[error("Failed to write audio to temporary file")]
    FileWrite(#[source] std::io::Error),
}

impl From<PollyError> for EchoError {
    fn from(error: PollyError) -> Self {
        EchoError::speech(error)
    }
}

/// Speaks through Amazon Polly, playing back the mp3 using macOS `afplay`.
pub struct PollySpeaker {
    /// Behind a lock so the settings screen can move the app to another AWS
    /// profile without rebuilding the speaker. The routing speaker and the
    /// conversation loop both hold this one object; replacing it would mean
    /// rebuilding them too, and a half-swapped set would leave the app speaking
    /// as the previous account.
    client: Mutex<aws_sdk_polly::Client>,
    voice: Mutex<String>,
    current: Arc<Mutex<Option<Child>>>,
    generation: Arc<AtomicU64>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for PollySpeaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollySpeaker")
            .field("voice", &self.voice())
            .field("speaking", &self.is_speaking())
            .finish_non_exhaustive()
    }
}

impl PollySpeaker {
    pub fn new(client: aws_sdk_polly::Client, voice: impl Into<String>) -> Self {
        Self {
            client: Mutex::new(client),
            voice: Mutex::new(voice.into()),
            current: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            turn: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn set_voice(&self, requested: Option<&str>) {
        let resolved = requested.unwrap_or(DEFAULT_POLLY_VOICE).to_string();
        match self.voice.lock() {
            Ok(mut voice) => *voice = resolved,
            Err(poisoned) => *poisoned.into_inner() = resolved,
        }
    }

    /// Point synthesis at a different account, after the AWS profile changed.
    ///
    /// A sentence already in flight finishes on the old client, which is the
    /// only coherent outcome: it was authorised when it started.
    pub fn set_client(&self, client: aws_sdk_polly::Client) {
        match self.client.lock() {
            Ok(mut current) => *current = client,
            Err(poisoned) => *poisoned.into_inner() = client,
        }
    }

    /// Cloned out rather than used under the guard: synthesis is an `await`, and
    /// a `std::sync` guard held across one would make the future non-`Send`.
    /// Clients are handles over a shared connection pool, so this is cheap.
    fn client(&self) -> aws_sdk_polly::Client {
        match self.client.lock() {
            Ok(client) => client.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn voice(&self) -> String {
        match self.voice.lock() {
            Ok(voice) => voice.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn kill_current(&self) {
        let mut guard = match self.current.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[async_trait]
impl Speaker for PollySpeaker {
    async fn speak(&self, text: &str) -> Result<(), EchoError> {
        if text.trim().is_empty() {
            return Ok(());
        }

        // Queue behind any sentence already playing.
        let _turn = self.turn.lock().await;

        // Captured after acquiring the turn, not before — the same ordering as
        // `SystemSpeaker`, and for a sharper reason here. Snapshotting first
        // put the whole queue wait *and* the synthesis round trip to AWS inside
        // the cancellation window, so any `stop` in that span discarded the
        // audio and returned `Ok(())`. `stop` fires on every barge-in, this
        // window is a network call rather than a `spawn`, and the failure is
        // silent: the app simply does not speak, with nothing logged and no
        // error for the UI to show.
        //
        // A `stop` arriving *during* synthesis still cancels, because it lands
        // after this load. What no longer cancels is one that arrived before
        // this call's turn began, which was never this utterance's to honour.
        let generation = self.generation.load(Ordering::SeqCst);

        let voice_id = VoiceId::from(self.voice().as_str());

        // Fetch audio stream from Amazon Polly
        let resp = self
            .client()
            .synthesize_speech()
            .text(text)
            .engine(Engine::Neural)
            .output_format(OutputFormat::Mp3)
            .voice_id(voice_id)
            .send()
            .await
            .map_err(|e| PollyError::Api(e.to_string()))?;

        let audio_stream = resp.audio_stream;
        let data = audio_stream
            .collect()
            .await
            .map_err(|e| PollyError::Api(e.to_string()))?;
        let bytes = data.into_bytes();

        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join(format!("polly_speech_{}.mp3", std::process::id()));

        tokio::fs::write(&file_path, bytes)
            .await
            .map_err(PollyError::FileWrite)?;

        let child = Command::new(AFPLAY)
            .arg(&file_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(PollyError::Spawn)?;

        {
            let mut guard = match self.current.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if self.generation.load(Ordering::SeqCst) != generation {
                drop(guard);
                let mut orphan = child;
                let _ = orphan.kill();
                let _ = orphan.wait();
                let _ = std::fs::remove_file(&file_path);
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
                    None => {
                        let _ = std::fs::remove_file(&file_path);
                        return Ok(());
                    }
                    Some(child) => child.try_wait().map_err(PollyError::Wait)?.is_some(),
                };
                if finished {
                    *guard = None;
                    let _ = std::fs::remove_file(&file_path);
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

impl Drop for PollySpeaker {
    fn drop(&mut self) {
        self.kill_current();
    }
}
