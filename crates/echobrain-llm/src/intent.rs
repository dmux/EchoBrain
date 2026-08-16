//! Routing an ambiguous utterance between storing and asking, with a model.
//!
//! # Why this is worth a network call
//!
//! [`echobrain_core::CueIntent`] decides in microseconds and is right whenever
//! the speaker gave a cue — an imperative, a question mark, an interrogative
//! opener. What it cannot do is read a sentence with no cue at all, and it
//! resolves those to [`Intent::Record`] because on the hotkey path a lost
//! memory is worse than an odd one.
//!
//! Conversation mode broke that trade. With the microphone open, the ambiguous
//! middle stops being the occasional dictated remark and becomes most of what
//! is heard, and "everything ambiguous is a memory" turns the app into a
//! machine that stores its own room. Only the utterances the cue router
//! declined reach this module, so the token cost falls on exactly the sentences
//! nothing cheaper could classify.
//!
//! # Why the prompt refuses to elaborate
//!
//! It answers with one word. Anything longer is a parsing problem for no gain,
//! and a model given room to explain will use it.

use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message,
};
use echobrain_core::{EchoError, Intent, IntentJudge};

use crate::bedrock::{BedrockError, service_detail};

/// Small: the reply is one word, and the ceiling is a runaway guard rather than
/// a length target. Room is left for a model that thinks before answering.
const MAX_TOKENS: i32 = 512;

const SYSTEM: &str = "\
Você classifica falas em português do Brasil captadas por um aplicativo de \
memórias pessoais. Responda com UMA palavra, sem pontuação e sem explicação:

GUARDAR — a pessoa está afirmando, contando ou registrando algo que ela quer \
lembrar depois. Fatos, preferências, decisões, coisas que aconteceram.
PERGUNTAR — a pessoa está pedindo uma informação de volta, consultando o que \
já foi guardado, ou fazendo uma pergunta de qualquer tipo.

Na dúvida entre as duas, responda PERGUNTAR: devolver uma busca inesperada \
custa um instante, enquanto guardar uma pergunta polui a memória com linhas \
que ninguém escreveu de propósito.

Frases ditas pelo próprio aplicativo — confirmações como \"pronto, guardei\" \
ou \"não encontrei nada\" — não são nenhum dos dois; para elas responda \
PERGUNTAR, porque uma busca sem resultado não deixa rastro.";

/// Routes ambiguous utterances with a Claude model on Bedrock.
#[derive(Debug, Clone)]
pub struct BedrockIntentJudge {
    client: Client,
    model_id: String,
}

impl BedrockIntentJudge {
    /// Built from the same `SdkConfig` as everything else, so judging adds no
    /// credential of its own.
    pub fn new(sdk_config: &aws_config::SdkConfig, model_id: impl Into<String>) -> Self {
        Self {
            client: Client::new(sdk_config),
            model_id: model_id.into(),
        }
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[async_trait]
impl IntentJudge for BedrockIntentJudge {
    async fn judge(&self, text: &str) -> Result<Intent, EchoError> {
        let message = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(text.to_string()))
            .build()
            .map_err(|error| BedrockError::Build(Box::new(error)))?;

        let response = self
            .client
            .converse()
            .model_id(&self.model_id)
            .system(aws_sdk_bedrockruntime::types::SystemContentBlock::Text(
                SYSTEM.to_string(),
            ))
            .messages(message)
            .inference_config(
                InferenceConfiguration::builder()
                    .max_tokens(MAX_TOKENS)
                    .build(),
            )
            .send()
            .await
            .map_err(|error| BedrockError::Invoke {
                model_id: self.model_id.clone(),
                detail: service_detail(&error),
                source: Box::new(error),
            })?;

        Ok(parse(&reply_text(&response)))
    }
}

/// Flatten whatever text blocks came back into one string.
fn reply_text(response: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput) -> String {
    let Some(aws_sdk_bedrockruntime::types::ConverseOutput::Message(message)) =
        response.output.as_ref()
    else {
        return String::new();
    };
    message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Read the one word, leniently.
///
/// A model told to answer with a single word usually does, but not always —
/// and an unparseable reply must not become a silent `Record`, which is the
/// very default this exists to stop applying blindly. Anything unrecognised is
/// therefore `Ask`, matching what the prompt asks for under doubt: a search
/// that finds nothing leaves no trace, while a wrong memory does.
fn parse(reply: &str) -> Intent {
    let reply = reply.trim().to_lowercase();
    if reply.contains("guardar") {
        Intent::Record
    } else {
        Intent::Ask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_one_word_replies_are_read() {
        assert_eq!(parse("GUARDAR"), Intent::Record);
        assert_eq!(parse("PERGUNTAR"), Intent::Ask);
        assert_eq!(parse(" guardar\n"), Intent::Record);
    }

    #[test]
    fn a_model_that_explains_itself_is_still_understood() {
        assert_eq!(parse("GUARDAR — é uma afirmação."), Intent::Record);
    }

    /// The direction an unreadable answer must fall. `Record` is the default
    /// this whole module exists to stop applying to ambiguous speech, so it is
    /// the one reply that has to be said explicitly.
    #[test]
    fn an_unreadable_reply_asks_rather_than_stores() {
        assert_eq!(parse(""), Intent::Ask);
        assert_eq!(parse("não sei"), Intent::Ask);
        assert_eq!(parse("<desculpe, não posso>"), Intent::Ask);
    }
}
