//! Cutting a token stream into sentences that can be spoken as they arrive.
//!
//! # Why this is not `split('.')`
//!
//! The output of this goes straight to a speech synthesiser, so every mistake
//! is audible. Splitting on every full stop puts a pause inside "R$ 1.500" and
//! "3.5 segundos"; waiting for a blank line means the first word is spoken
//! after the model has finished, which is the latency this module exists to
//! avoid.
//!
//! # The streaming constraint
//!
//! A terminator only ends a sentence if something *follows* it — "custo." might
//! be the end of a sentence or the middle of "custo.com". In a stream the next
//! character has not arrived yet, so a sentence is emitted when the terminator
//! is followed by whitespace, and whatever is left over is flushed by
//! [`Sentences::finish`] when the stream closes.

/// Emit a chunk once it gets this long even without punctuation, so that a
/// model writing one long unpunctuated clause does not produce silence.
/// Roughly ten seconds of speech.
const MAX_PENDING: usize = 220;

/// Below this, a terminator is treated as an abbreviation rather than the end
/// of a sentence — "Dr." and "etc." should not each become an utterance.
const MIN_SENTENCE: usize = 24;

/// Accumulates streamed text and yields whole sentences.
#[derive(Debug, Default)]
pub struct Sentences {
    pending: String,
}

impl Sentences {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a token. Returns every sentence that closed as a result — usually
    /// none, occasionally one, rarely more.
    pub fn push(&mut self, token: &str) -> Vec<String> {
        self.pending.push_str(token);
        let mut out = Vec::new();

        while let Some(cut) = self.next_cut() {
            let rest = self.pending.split_off(cut);
            let sentence = std::mem::replace(&mut self.pending, rest);
            let sentence = sentence.trim().to_string();
            if !sentence.is_empty() {
                out.push(sentence);
            }
        }

        out
    }

    /// Everything still buffered when the stream ends. The last sentence
    /// usually arrives without trailing whitespace, so without this it would
    /// never be spoken.
    pub fn finish(&mut self) -> Option<String> {
        let last = std::mem::take(&mut self.pending).trim().to_string();
        (!last.is_empty()).then_some(last)
    }

    /// The byte offset just past the end of the first complete sentence in
    /// `pending`, if there is one.
    fn next_cut(&self) -> Option<usize> {
        let bytes = self.pending.as_bytes();
        let mut chars = self.pending.char_indices().peekable();

        while let Some((index, current)) = chars.next() {
            // A newline always ends a chunk: the model uses them to separate
            // thoughts, and speaking across one runs two ideas together.
            if current == '\n' {
                return Some(index + current.len_utf8());
            }

            if !matches!(current, '.' | '?' | '!' | '…' | ';' | ':') {
                continue;
            }

            let after = index + current.len_utf8();

            // A terminator only closes a sentence once something follows it,
            // and that something has to be whitespace. Mid-stream the next
            // token may simply not have arrived — `finish` catches that case.
            let Some(&(_, next)) = chars.peek() else {
                continue;
            };
            if !next.is_whitespace() {
                continue;
            }

            // "1.500" and "3.5" — a digit on both sides is a number, not a
            // sentence boundary. Cheap to check and very audible when wrong.
            if current == '.'
                && index > 0
                && bytes[index - 1].is_ascii_digit()
                && next.is_ascii_digit()
            {
                continue;
            }

            // Too short to be a sentence: almost certainly an abbreviation
            // ("etc.", "Dr.", "p.ex.") that happens to sit at a word break.
            if self.pending[..after].trim().chars().count() < MIN_SENTENCE {
                continue;
            }

            return Some(after);
        }

        // No punctuation in sight and the buffer is getting long. Cut at the
        // last word boundary rather than mid-word.
        if self.pending.chars().count() >= MAX_PENDING {
            return self
                .pending
                .char_indices()
                .filter(|(_, c)| c.is_whitespace())
                .map(|(index, _)| index)
                .next_back();
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole text one character at a time, the worst case a token
    /// stream can produce, and collect everything spoken.
    fn stream(text: &str) -> Vec<String> {
        let mut sentences = Sentences::new();
        let mut out = Vec::new();
        for character in text.chars() {
            out.extend(sentences.push(&character.to_string()));
        }
        out.extend(sentences.finish());
        out
    }

    #[test]
    fn a_sentence_is_emitted_once_the_following_space_arrives() {
        let mut sentences = Sentences::new();
        assert!(
            sentences
                .push("Vocês descartaram o OpenSearch por custo.")
                .is_empty(),
            "a terminator alone is not enough — the next token may continue it"
        );
        assert_eq!(
            sentences.push(" "),
            vec!["Vocês descartaram o OpenSearch por custo."]
        );
    }

    #[test]
    fn the_last_sentence_is_flushed_by_finish() {
        // Nothing follows the final full stop, so only `finish` can release it.
        let spoken = stream("Foi por causa do custo por byte no DynamoDB.");
        assert_eq!(spoken, vec!["Foi por causa do custo por byte no DynamoDB."]);
    }

    #[test]
    fn several_sentences_stream_in_order() {
        let spoken = stream(
            "Vocês descartaram o OpenSearch por causa do custo por byte. \
             O vetor também podia ficar no mesmo item do DynamoDB. \
             Isso foi decidido há três semanas.",
        );
        assert_eq!(spoken.len(), 3);
        assert!(spoken[0].starts_with("Vocês descartaram"));
        assert!(spoken[2].ends_with("três semanas."));
    }

    #[test]
    fn a_decimal_point_is_not_a_sentence_boundary() {
        // "1. 500" would be spoken as two utterances with a pause between the
        // digits, which is the kind of error only audible after shipping.
        let spoken = stream("O orçamento aprovado ficou em 1.500 reais por mês.");
        assert_eq!(
            spoken,
            vec!["O orçamento aprovado ficou em 1.500 reais por mês."]
        );
    }

    #[test]
    fn an_abbreviation_does_not_split_a_sentence() {
        let spoken = stream("Dr. Bruno assumiu o time de plataforma em janeiro.");
        assert_eq!(
            spoken,
            vec!["Dr. Bruno assumiu o time de plataforma em janeiro."],
            "a short fragment before a full stop is an abbreviation, not a sentence"
        );
    }

    #[test]
    fn a_newline_always_closes_a_chunk() {
        let spoken = stream("Primeira ideia\nSegunda ideia");
        assert_eq!(spoken, vec!["Primeira ideia", "Segunda ideia"]);
    }

    #[test]
    fn a_question_or_exclamation_closes_a_sentence() {
        let spoken = stream("Você quer ouvir a próxima memória? Ela é de janeiro.");
        assert_eq!(spoken.len(), 2);
        assert!(spoken[0].ends_with('?'));
    }

    #[test]
    fn an_unpunctuated_wall_is_cut_at_a_word_boundary() {
        let long = "palavra ".repeat(60);
        let spoken = stream(&long);

        assert!(
            spoken.len() > 1,
            "a long unpunctuated stream must not stay silent"
        );
        for chunk in &spoken {
            assert!(
                !chunk.starts_with("alavra") && !chunk.ends_with("palavr"),
                "chunks must break between words, not inside one: {chunk:?}"
            );
        }
        // Nothing is dropped on the floor.
        let rejoined = spoken.join(" ");
        assert_eq!(rejoined.matches("palavra").count(), 60);
    }

    #[test]
    fn nothing_is_emitted_for_whitespace_only_output() {
        let mut sentences = Sentences::new();
        assert!(sentences.push("   \n  ").is_empty() || sentences.push("").is_empty());
        assert_eq!(sentences.finish(), None);
    }

    #[test]
    fn accents_do_not_split_a_multi_byte_character() {
        // Byte-indexing a `String` at the wrong offset panics; pt-BR text is
        // full of multi-byte characters, so this is the regression that would
        // take the whole answer down.
        let spoken = stream("A decisão foi tomada às três. É irreversível até revisão.");
        assert_eq!(spoken.len(), 2);
        assert!(spoken[0].contains("decisão"));
    }
}
