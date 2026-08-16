//! [`Answerer`] over Amazon Bedrock's Converse API.

use std::time::SystemTime;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ContentBlockDelta, ConversationRole, ConverseStreamOutput,
    InferenceConfiguration, Message, SystemContentBlock,
};
use echobrain_core::{Answer, AnswerStyle, Answerer, EchoError, Hit, SentenceSink};

use crate::prompt;
use crate::sentences::Sentences;

/// The recommended model: the cheapest one that can do this job well.
///
/// # Why a small model is the right one here, not just the cheap one
///
/// The task is deliberately narrow. Read at most five memories, answer in three
/// sentences of pt-BR, and when they do not answer the question say one fixed
/// sentence instead. There is no reasoning to do, no tools to choose between
/// and no long context to hold — the hard part was the retrieval, and that
/// already happened. Model capability buys almost nothing past this bar, while
/// every question pays for it.
///
/// It also keeps the failure mode cheap. A question asked by voice fires the
/// moment the microphone closes, with no confirmation step, so a stray hotkey
/// press is a billed call.
///
/// # A note on getting there
///
/// Every profile in the account this was built against first returned
/// `ResourceNotFoundException` — *"Model use case details have not been
/// submitted for this account"* — which is a console form, not something code
/// can work around. Worth recognising, because it names a model id that was
/// never wrong: see [`BedrockError::Invoke`], which carries the service's own
/// sentence for exactly this reason.
///
/// ⚠️ Bedrock addresses Claude through **regional inference profiles**, and the
/// available ids depend on the account and region — they cannot be derived.
/// List what this account has with:
///
/// ```text
/// aws bedrock list-inference-profiles --region us-east-1 \
///   --query 'inferenceProfileSummaries[?contains(inferenceProfileId, `anthropic`)].inferenceProfileId'
/// ```
///
/// A wrong or unentitled id fails on the first question with
/// `ValidationException` or `AccessDeniedException`, never at build time —
/// which is why [`BedrockAnswerer::probe`] exists.
pub const DEFAULT_MODEL_ID: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

/// The output ceiling.
///
/// A ceiling, not a reservation: unused budget costs nothing, so this is sized
/// for the worst case rather than the expected one. Brevity is the prompt's
/// job.
///
/// It is generous for a reason that only applies to some models. Where thinking
/// is on by default, `max_tokens` caps thinking **plus** the reply, and a
/// budget sized to three spoken sentences yields a reply that is entirely
/// reasoning and then truncates mid-word. [`DEFAULT_MODEL_ID`] does not think
/// by default and would be fine with a quarter of this — but the setting is
/// shared, and the failure it prevents is silent.
pub const DEFAULT_MAX_TOKENS: i32 = 2_048;

#[derive(Debug, thiserror::Error)]
pub enum BedrockError {
    /// # Why the service's own words are in the message
    ///
    /// `IpcError` shows only `Display` on the outer error, with the source
    /// chain going to `tracing` — right for most failures, because a user
    /// cannot act on a stack of SDK types.
    ///
    /// This one is the exception. Bedrock's refusals are specific and
    /// actionable in a way no wrapper can restate: *"Model use case details
    /// have not been submitted for this account"* names a form to fill in,
    /// while a generic "the model rejected the request" sends the reader to
    /// check a model id that was never wrong. Rewriting that in our own voice
    /// made the error less useful, which was discovered by hitting it.
    #[error("Bedrock recusou `{model_id}`: {detail}")]
    Invoke {
        model_id: String,
        detail: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("the answer stream failed midway")]
    Stream(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("the request could not be built")]
    Build(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl From<BedrockError> for EchoError {
    fn from(error: BedrockError) -> Self {
        EchoError::answering(error)
    }
}

/// Composes answers with a Claude model on Bedrock.
#[derive(Debug, Clone)]
pub struct BedrockAnswerer {
    client: Client,
    model_id: String,
    max_tokens: i32,
}

impl BedrockAnswerer {
    /// Build from the `SdkConfig` the app already resolved for the memory API.
    ///
    /// Same credentials, same profile, same SSO session — answering adds no
    /// secret of its own. The region must be one where the model is available,
    /// which is not necessarily the region the memory API lives in.
    pub fn new(sdk_config: &aws_config::SdkConfig, model_id: impl Into<String>) -> Self {
        Self {
            client: Client::new(sdk_config),
            model_id: model_id.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: i32) -> Self {
        self.max_tokens = max_tokens.max(256);
        self
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Ask the model to say one word, to prove the id and the entitlement.
    ///
    /// Model access on Bedrock is granted per account, per region and per
    /// model, and none of that is visible from here until a call is made. The
    /// settings screen calls this once so the failure lands on a *Testar*
    /// button rather than on the first real question, where the user would be
    /// waiting for an answer and get an error instead.
    pub async fn probe(&self) -> Result<(), EchoError> {
        let request = self
            .client
            .converse()
            .model_id(&self.model_id)
            .messages(
                Message::builder()
                    .role(ConversationRole::User)
                    .content(ContentBlock::Text("Responda apenas: ok".into()))
                    .build()
                    .map_err(|error| BedrockError::Build(Box::new(error)))?,
            )
            .inference_config(InferenceConfiguration::builder().max_tokens(512).build());

        request.send().await.map_err(|error| BedrockError::Invoke {
            model_id: self.model_id.clone(),
            detail: service_detail(&error),
            source: Box::new(error),
        })?;

        Ok(())
    }
}

#[async_trait]
impl Answerer for BedrockAnswerer {
    async fn answer(
        &self,
        question: &str,
        hits: &[Hit],
        style: AnswerStyle,
        sink: &mut dyn SentenceSink,
    ) -> Result<Answer, EchoError> {
        let user_turn = prompt::user_turn(question, hits, SystemTime::now());

        let response = self
            .client
            .converse_stream()
            .model_id(&self.model_id)
            .system(SystemContentBlock::Text(prompt::system(style).to_string()))
            .messages(
                Message::builder()
                    .role(ConversationRole::User)
                    .content(ContentBlock::Text(user_turn))
                    .build()
                    .map_err(|error| BedrockError::Build(Box::new(error)))?,
            )
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(self.max_tokens)
                    .build(),
            )
            .send()
            .await
            .map_err(|error| BedrockError::Invoke {
                model_id: self.model_id.clone(),
                detail: service_detail(&error),
                source: Box::new(error),
            })?;

        let mut stream = response.stream;
        let mut sentences = Sentences::new();
        let mut full = String::new();

        loop {
            let event = stream
                .recv()
                .await
                .map_err(|error| BedrockError::Stream(Box::new(error)))?;

            let Some(event) = event else { break };

            // Only text deltas matter. Thinking blocks, tool blocks and the
            // metadata frame are all skipped rather than matched exhaustively:
            // the enum is `#[non_exhaustive]` and gains variants over time,
            // and a new one must not break answering.
            if let ConverseStreamOutput::ContentBlockDelta(delta) = event
                && let Some(ContentBlockDelta::Text(token)) = delta.delta
            {
                full.push_str(&token);
                for sentence in sentences.push(&token) {
                    sink.sentence(&sentence);
                }
            }
        }

        if let Some(last) = sentences.finish() {
            sink.sentence(&last);
        }

        let text = full.trim().to_string();

        if text.is_empty() {
            // An empty completion is not an answer. Reporting nothing found is
            // better than speaking silence and showing sources for it — but the
            // call still happened and still billed, so `declined`, not
            // `nothing_found`.
            return Ok(Answer::declined());
        }

        let refused = is_refusal(&text);
        if refused {
            return Ok(Answer::declined());
        }

        Ok(Answer {
            text,
            cited: hits.to_vec(),
            grounded: true,
            consulted: true,
        })
    }
}

/// The one sentence out of an SDK error chain that a person can act on.
///
/// Walks to the deepest source, because that is where the service put its
/// message — the outer layers are transport and dispatch wrappers whose
/// `Display` says "service error" and nothing else.
pub(crate) fn service_detail(error: &(dyn std::error::Error + 'static)) -> String {
    let mut deepest = error.to_string();
    let mut current = error.source();
    while let Some(source) = current {
        let text = source.to_string();
        if !text.trim().is_empty() {
            deepest = text;
        }
        current = source.source();
    }
    deepest
}

/// Whether the model produced the "nothing found" sentence it was instructed to
/// produce when the memories do not answer the question.
///
/// # Why this is matched rather than trusted
///
/// The search returning hits does not mean the hits *answer* anything — they
/// cleared a similarity floor, which is a statement about vectors, not about
/// relevance. When the model correctly declines, the result must be reported as
/// ungrounded so the UI stops rendering memory cards beneath it: sources under
/// an admission would imply the admission came from them.
///
/// The comparison is deliberately loose about trailing punctuation and case,
/// and deliberately strict about everything else. A model that hedges its way
/// around the sentence ("não encontrei nada sobre isso, mas talvez…") is not
/// declining — it is answering, and it should be treated as one so the sources
/// stay visible for the user to check it against.
fn is_refusal(text: &str) -> bool {
    fn normalise(value: &str) -> String {
        value
            .trim()
            .trim_end_matches(['.', '!', ' '])
            .to_lowercase()
    }

    normalise(text) == normalise(Answer::NOTHING_FOUND)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instructed_sentence_is_recognised_as_a_refusal() {
        assert!(is_refusal(Answer::NOTHING_FOUND));
        // Trailing punctuation and casing drift between completions.
        assert!(is_refusal(
            "Não encontrei nada sobre isso nas suas memórias"
        ));
        assert!(is_refusal(
            "  não encontrei nada sobre isso nas suas memórias.  "
        ));
    }

    #[test]
    fn an_answer_that_merely_starts_like_the_refusal_is_still_an_answer() {
        // The model hedged and then answered anyway. Treating this as a refusal
        // would hide the sources for a sentence that very much needs them.
        assert!(!is_refusal(
            "Não encontrei nada sobre isso nas suas memórias, mas você mencionou o custo por byte."
        ));
    }

    #[test]
    fn an_ordinary_answer_is_not_a_refusal() {
        assert!(!is_refusal(
            "Você descartou o OpenSearch por causa do custo por byte."
        ));
        assert!(!is_refusal(""));
    }
}
