use crate::config::VoiceConfig;
use async_trait::async_trait;
use echobrain_audio::{PollySpeaker, SystemSpeaker};
use echobrain_core::{EchoError, Speaker};
use std::sync::Arc;
use std::sync::RwLock;

#[derive(Debug)]
pub struct RoutingSpeaker {
    pub system: Option<Arc<SystemSpeaker>>,
    pub polly: Arc<PollySpeaker>,
    pub settings: Arc<RwLock<VoiceConfig>>,
}

#[async_trait]
impl Speaker for RoutingSpeaker {
    async fn speak(&self, text: &str) -> Result<(), EchoError> {
        // Poison-tolerant, as everywhere else settings are read. Unwrapping
        // here meant a single panic anywhere under this lock silenced the app
        // for the rest of the session: every later `speak` would panic on the
        // poison instead of reaching a voice.
        let engine = {
            let settings = match self.settings.read() {
                Ok(settings) => settings,
                Err(poisoned) => poisoned.into_inner(),
            };
            settings
                .tts_engine
                .clone()
                .unwrap_or_else(|| "system".into())
        };
        if engine == "polly" {
            self.polly.speak(text).await
        } else {
            if let Some(sys) = &self.system {
                sys.speak(text).await
            } else {
                Err(EchoError::audio(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No system voice found",
                )))
            }
        }
    }

    fn stop(&self) {
        if let Some(sys) = &self.system {
            sys.stop();
        }
        self.polly.stop();
    }

    fn is_speaking(&self) -> bool {
        let sys_speaking = self
            .system
            .as_ref()
            .map(|s| s.is_speaking())
            .unwrap_or(false);
        sys_speaking || self.polly.is_speaking()
    }
}
