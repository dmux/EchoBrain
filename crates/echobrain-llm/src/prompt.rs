//! The prompt, and the shape of the memories handed to the model.
//!
//! # This file is a safety boundary, not string formatting
//!
//! Everything EchoBrain returned before this existed was verbatim: the sentence
//! that came back was the sentence that went in, and a wrong answer meant a
//! wrong *search*, which the similarity meter made visible. A composed answer
//! is different — it can be fluent, confident and unsupported, and it arrives
//! by voice, where there is nothing to skim.
//!
//! Three things guard against that, and this module holds the first two:
//!
//! 1. The model sees only the recalled memories. No history, no tools, no
//!    outside knowledge it is invited to use.
//! 2. It is told exactly what to say when the memories do not answer the
//!    question, rather than left to improvise a hedge.
//!
//! The third lives in the UI: the sources are rendered under every answer.
//!
//! # Two prompts, one set of rules
//!
//! [`SYSTEM_SPOKEN`] and [`SYSTEM_WRITTEN`] differ *only* in form. The four
//! rules that make this a boundary — memories as the sole source, the fixed
//! sentence for "not found", no identifiers or similarity scores, contradictions
//! surfaced rather than resolved — are byte-identical in both, and a test says
//! so. The temptation when loosening the format is to loosen the grounding along
//! with it, and a long fluent invention is harder to catch than a short one.
//!
//! What varies is the medium. Spoken, there is no skimming and no scrollback, so
//! three sentences of plain prose is the whole budget. On screen there is both,
//! and the same limit would throw away detail the memories actually contain.

use std::sync::LazyLock;

use echobrain_core::{AnswerStyle, Hit};

/// The rules that do not depend on where the answer lands.
///
/// Kept as one constant spliced into both prompts rather than copied, so that
/// editing a safety rule cannot silently edit only one of the two.
const RULES: &str = "\
- Nunca use conhecimento próprio nem invente detalhes. As memórias são a \
única fonte.
- Se as memórias não respondem à pergunta, responda exatamente: \"Não \
encontrei nada sobre isso nas suas memórias.\" e nada mais.
- Fale na segunda pessoa (\"você decidiu…\"), porque são as memórias de quem \
está ouvindo.
- Situe no tempo quando ajudar (\"há três semanas\", \"em janeiro\"). Nunca \
mencione identificadores, porcentagens de similaridade ou o número da memória.
- Se as memórias se contradizem, diga que se contradizem e apresente as duas \
versões em vez de escolher uma.";

/// The system prompt for an answer that will be read out loud.
///
/// Fixed for the life of the process, which is also what makes it worth caching
/// on the Bedrock side — it is the same bytes on every question, and only the
/// memories below it change.
pub static SYSTEM_SPOKEN: LazyLock<String> = LazyLock::new(|| {
    format!(
        "\
Você é a voz do EchoBrain, a memória pessoal de quem pergunta. Responde \
usando SOMENTE as memórias fornecidas na mensagem do usuário.

Regras:
- Responda em português do Brasil, no máximo três frases, em texto corrido. \
Sem listas, sem markdown, sem emojis: o texto será lido em voz alta.
{RULES}"
    )
});

/// The system prompt for an answer that will only be rendered on screen.
///
/// # Why this one has no sentence limit
///
/// The question was typed, so the answer is read, and reading recovers
/// everything speech cannot: the reader skims, re-reads, and jumps to the part
/// they wanted. Capping it at three sentences there does not protect anybody —
/// it just discards what the memories say. Markdown is allowed for the same
/// reason, and because the UI already renders it (`answer-card.tsx` →
/// `MarkdownContent`).
///
/// The length still comes from the memories, not from the model's willingness to
/// write: "as long as the recalled memories warrant, and no longer" is the
/// instruction, because padding a thin recall into a full page is the written
/// form of the invention the spoken prompt guards against.
pub static SYSTEM_WRITTEN: LazyLock<String> = LazyLock::new(|| {
    format!(
        "\
Você é a voz do EchoBrain, a memória pessoal de quem pergunta. Responde \
usando SOMENTE as memórias fornecidas na mensagem do usuário.

Regras:
- Responda em português do Brasil. O texto será lido na tela, não em voz alta: \
use markdown quando ajudar — parágrafos, listas, negrito, títulos curtos.
- O tamanho vem das memórias, não da vontade de escrever: cubra o que elas \
dizem e pare. Se uma frase basta, responda com uma frase.
- Quando as memórias derem o porquê de algo, inclua o porquê — é o que se \
perde numa resposta curta.
{RULES}"
    )
});

/// Which system prompt a request gets.
///
/// Both are built once and live for the process, so this stays as cacheable on
/// the Bedrock side as the single constant it replaced: the same bytes on every
/// question of the same style.
pub fn system(style: AnswerStyle) -> &'static str {
    match style {
        AnswerStyle::Spoken => SYSTEM_SPOKEN.as_str(),
        AnswerStyle::Written => SYSTEM_WRITTEN.as_str(),
    }
}

/// Render the question and the recalled memories as the user turn.
///
/// The memories are numbered and dated but carry no similarity score: a model
/// shown "67%" starts hedging in the answer ("provavelmente…"), and the meter
/// is the UI's job, not the narrator's.
pub fn user_turn(question: &str, hits: &[Hit], now: std::time::SystemTime) -> String {
    let mut prompt = String::with_capacity(512 + hits.len() * 256);

    prompt.push_str("Memórias recuperadas, da mais relevante para a menos:\n\n");
    for (index, hit) in hits.iter().enumerate() {
        prompt.push_str(&format!(
            "[{}] ({}, {}) {}\n",
            index + 1,
            kind_label(hit),
            age(hit, now),
            hit.memory.text.trim(),
        ));
    }

    prompt.push_str("\nPergunta: ");
    prompt.push_str(question.trim());

    prompt
}

/// pt-BR name for the memory kind, matching the labels the UI already uses.
fn kind_label(hit: &Hit) -> &'static str {
    match hit.memory.kind {
        echobrain_core::MemoryKind::Fact => "fato",
        echobrain_core::MemoryKind::Preference => "preferência",
        echobrain_core::MemoryKind::Episode => "episódio",
        echobrain_core::MemoryKind::Snippet => "código",
    }
}

/// How long ago the memory was written, in words rather than a timestamp.
///
/// The model is asked to situate answers in time, and it does that far better
/// from "há três semanas" than from an ISO date it has to subtract from a
/// today it cannot know.
fn age(hit: &Hit, now: std::time::SystemTime) -> String {
    let Ok(elapsed) = now.duration_since(hit.memory.created_at) else {
        // A clock that went backwards, or a memory from the future. Neither is
        // worth failing an answer over.
        return "há pouco".to_string();
    };

    let days = elapsed.as_secs() / 86_400;
    match days {
        0 => "hoje".to_string(),
        1 => "ontem".to_string(),
        2..=13 => format!("há {days} dias"),
        14..=59 => format!("há {} semanas", days / 7),
        60..=364 => format!("há {} meses", days / 30),
        _ => format!("há {} anos", days / 365),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, SystemTime};

    use agent_memory_core::{Memory, MemoryId, MemoryKind, UserId};
    use echobrain_core::Similarity;

    fn hit(text: &str, kind: MemoryKind, age: Duration, now: SystemTime) -> Hit {
        Hit {
            memory: Memory {
                user_id: UserId::new("caller").expect("valid user"),
                memory_id: MemoryId::new(format!("mem-{}", text.len())).expect("valid id"),
                text: text.to_string(),
                kind,
                created_at: now - age,
                expires_at: None,
                source: Some("test".into()),
                github_login: None,
                github_repo: None,
                rating: None,
                active: true,
            },
            similarity: Similarity::from_distance(
                agent_memory_core::Distance::new(0.6).expect("finite"),
            ),
        }
    }

    #[test]
    fn the_prompt_carries_every_memory_and_the_question() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let hits = vec![
            hit(
                "preferimos índice vetorial no DynamoDB",
                MemoryKind::Episode,
                Duration::from_secs(21 * 86_400),
                now,
            ),
            hit(
                "o Bruno assumiu a plataforma",
                MemoryKind::Fact,
                Duration::from_secs(86_400),
                now,
            ),
        ];

        let rendered = user_turn("por que não OpenSearch?", &hits, now);

        assert!(rendered.contains("preferimos índice vetorial no DynamoDB"));
        assert!(rendered.contains("o Bruno assumiu a plataforma"));
        assert!(rendered.contains("por que não OpenSearch?"));
        assert!(rendered.contains("[1]") && rendered.contains("[2]"));
    }

    #[test]
    fn similarity_never_reaches_the_model() {
        // A model shown a score starts hedging in the answer. The meter belongs
        // in the UI, where the user can weigh it themselves.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let hits = vec![hit("algo", MemoryKind::Episode, Duration::ZERO, now)];

        let rendered = user_turn("pergunta", &hits, now);

        assert!(!rendered.contains('%'));
        assert!(!rendered.to_lowercase().contains("similar"));
    }

    #[test]
    fn ages_are_words_rather_than_timestamps() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let day = 86_400;

        let cases = [
            (0, "hoje"),
            (day, "ontem"),
            (3 * day, "há 3 dias"),
            (21 * day, "há 3 semanas"),
            (90 * day, "há 3 meses"),
            (800 * day, "há 2 anos"),
        ];

        for (seconds, expected) in cases {
            let hit = hit("m", MemoryKind::Fact, Duration::from_secs(seconds), now);
            assert_eq!(age(&hit, now), expected, "for {seconds}s of age");
        }
    }

    #[test]
    fn a_memory_from_the_future_does_not_break_the_prompt() {
        // Clocks drift, and a memory written on another device can carry a
        // timestamp slightly ahead of this one's. Not worth failing over.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let future = Hit {
            memory: Memory {
                created_at: now + Duration::from_secs(3_600),
                ..hit("m", MemoryKind::Fact, Duration::ZERO, now).memory
            },
            ..hit("m", MemoryKind::Fact, Duration::ZERO, now)
        };

        assert_eq!(age(&future, now), "há pouco");
    }

    #[test]
    fn the_system_prompt_states_the_refusal_verbatim() {
        // The use case returns `Answer::NOTHING_FOUND` when the search is
        // empty; the model must produce the identical sentence when the search
        // returned something that turns out not to answer the question, or the
        // app has two different voices for the same outcome.
        //
        // Both styles, because the refusal is the one sentence a longer format
        // most invites a model to improve on.
        for style in [AnswerStyle::Spoken, AnswerStyle::Written] {
            assert!(
                system(style).contains(echobrain_core::Answer::NOTHING_FOUND),
                "{style:?}: the prompt and the domain must agree on the exact \
                 refusal sentence"
            );
        }
    }

    #[test]
    fn both_styles_carry_the_same_safety_rules() {
        // The whole point of splitting the prompt was to vary the *form*. This
        // is the test that keeps it that way: relaxing "three sentences" is a
        // formatting change, relaxing "never invent" is a different product, and
        // the second one is easy to do by accident while doing the first.
        for style in [AnswerStyle::Spoken, AnswerStyle::Written] {
            let prompt = system(style);
            assert!(
                prompt.contains(RULES),
                "{style:?} must carry the shared rules verbatim"
            );
            assert!(
                prompt.contains("Nunca use conhecimento próprio"),
                "{style:?} must forbid outside knowledge"
            );
            assert!(
                prompt.contains("SOMENTE as memórias"),
                "{style:?} must name the memories as the only source"
            );
        }
    }

    #[test]
    fn only_the_spoken_prompt_bans_markdown_and_caps_the_length() {
        assert!(SYSTEM_SPOKEN.contains("no máximo três frases"));
        assert!(SYSTEM_SPOKEN.contains("Sem listas, sem markdown"));

        // A written answer is read, and reading recovers everything speech
        // cannot — so neither limit belongs there.
        assert!(!SYSTEM_WRITTEN.contains("no máximo três frases"));
        assert!(!SYSTEM_WRITTEN.contains("Sem listas, sem markdown"));
        assert!(SYSTEM_WRITTEN.contains("use markdown"));
    }
}
