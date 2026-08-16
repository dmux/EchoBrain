//! A heuristic [`KindClassifier`].
//!
//! # Why this is a guess, and says so
//!
//! The memory domain has three kinds, and picking the right one improves recall
//! because `kind` is an `INLINE_FILTER` on the vector index — filtering happens
//! in storage instead of dragging candidates across the network to discard
//! them. An LLM would classify well, but this project deliberately has no
//! inference in the loop, so what is left is cue phrases.
//!
//! Cue phrases are wrong often enough that the honest design is to make the
//! result correctable in one click rather than to chase accuracy here. The
//! default is [`MemoryKind::Episode`] because dictated speech is episodic by
//! default: someone talking into a microphone is usually narrating something
//! that happened, and a wrong `Episode` costs less than a wrong `Fact`, which
//! asserts something durable about the person.

use agent_memory_core::MemoryKind;

use crate::ports::KindClassifier;

/// Markers for a stated taste or working style.
const PREFERENCE_CUES: &[&str] = &[
    // pt-BR
    "eu prefiro",
    "prefiro ",
    "eu gosto",
    "não gosto",
    "nao gosto",
    "eu odeio",
    "meu favorito",
    "minha favorita",
    "costumo ",
    "sempre uso",
    // en
    "i prefer",
    "i like",
    "i don't like",
    "i dont like",
    "i hate",
    "i love",
    "my favourite",
    "my favorite",
];

/// Markers for something durably true about the person or their world.
const FACT_CUES: &[&str] = &[
    // pt-BR
    "meu nome é",
    "meu nome e ",
    "eu moro",
    "eu trabalho",
    "eu sou ",
    "minha equipe",
    "meu time",
    // en
    "my name is",
    "i live in",
    "i work at",
    "i work on",
    "i am a",
    "i'm a ",
    "my team",
];

/// Cue-phrase classifier. Case-insensitive, accent-sensitive by design — the
/// unaccented variants are listed explicitly so that stripping accents (which
/// would also mangle the text for other purposes) is not needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct CueClassifier;

impl CueClassifier {
    pub const fn new() -> Self {
        Self
    }
}

impl KindClassifier for CueClassifier {
    fn classify(&self, text: &str) -> MemoryKind {
        let haystack = text.to_lowercase();

        // Preference is checked first: "eu trabalho melhor de manhã, prefiro
        // reuniões à tarde" is a preference that also contains a fact cue, and
        // the preference is the part worth keeping as such.
        if PREFERENCE_CUES.iter().any(|cue| haystack.contains(cue)) {
            return MemoryKind::Preference;
        }
        if FACT_CUES.iter().any(|cue| haystack.contains(cue)) {
            return MemoryKind::Fact;
        }
        MemoryKind::Episode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str) -> MemoryKind {
        CueClassifier::new().classify(text)
    }

    #[test]
    fn stated_tastes_are_preferences_in_both_languages() {
        assert_eq!(
            classify("eu prefiro arquitetura hexagonal"),
            MemoryKind::Preference
        );
        assert_eq!(
            classify("I prefer pour-over coffee"),
            MemoryKind::Preference
        );
        assert_eq!(
            classify("não gosto de reuniões longas"),
            MemoryKind::Preference
        );
    }

    #[test]
    fn durable_statements_about_the_person_are_facts() {
        assert_eq!(classify("meu nome é Rafael"), MemoryKind::Fact);
        assert_eq!(classify("I live in Lisbon"), MemoryKind::Fact);
    }

    #[test]
    fn anything_else_falls_back_to_episode() {
        // The common case: someone narrating what happened.
        assert_eq!(
            classify("decidimos usar índice vetorial no DynamoDB por causa do custo por byte"),
            MemoryKind::Episode
        );
        assert_eq!(classify(""), MemoryKind::Episode);
    }

    #[test]
    fn preference_wins_when_a_sentence_carries_both_cues() {
        // Contains "eu trabalho" (fact cue) and "prefiro" (preference cue).
        assert_eq!(
            classify("eu trabalho na Acme mas prefiro projetos pequenos"),
            MemoryKind::Preference
        );
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify("EU PREFIRO café coado"), MemoryKind::Preference);
        assert_eq!(classify("My Name Is Rafael"), MemoryKind::Fact);
    }
}
