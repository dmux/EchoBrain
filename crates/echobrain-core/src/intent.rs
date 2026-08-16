//! Deciding whether a spoken sentence is something to **store** or something to
//! **ask**.
//!
//! # Why this exists at all
//!
//! Until voice could only capture, the hotkey had one meaning and needed no
//! interpretation: everything dictated became a memory. Now that the same
//! gesture can also search, one utterance has to be routed, and the router has
//! about a second to decide before the user notices a pause.
//!
//! # Why a heuristic, again
//!
//! The same reasoning as [`crate::classify`]: an LLM would route better, but
//! routing happens *before* the answer path is entered, so paying for inference
//! here would put a token cost on every capture — including the ones that never
//! reach the answering layer. Cue phrases cost nothing and are correctable.
//!
//! # Why the default is [`Intent::Record`]
//!
//! The two mistakes are not symmetric. A question mistaken for a memory costs
//! one embedding and leaves a slightly odd row in the store — visible, and
//! deletable in one click. A memory mistaken for a question is *thrown away*:
//! the search returns something unrelated, the utterance is never stored, and
//! the user finds out weeks later when recall comes up empty. So the ambiguous
//! middle falls to `Record`, which is also what the app did before this module
//! existed.

/// What the speaker wanted to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Store this utterance as a memory.
    Record,
    /// Search the memories and answer.
    Ask,
}

impl Intent {
    /// The wire form, matching the strings the frontend's `IntentChip` uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Ask => "ask",
        }
    }
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Routes an utterance between storing and asking.
///
/// Deliberately not an LLM, for the reason in the module docs. Wrong sometimes,
/// which is why the UI shows the choice as an editable chip rather than a
/// settled fact — the same treatment [`crate::ports::KindClassifier`] gets.
pub trait IntentClassifier: Send + Sync {
    fn classify(&self, text: &str) -> Intent;
}

/// Imperatives that name the action outright. These win over everything,
/// including a question mark: *"grava: por que não fomos de OpenSearch?"* is a
/// request to store a question, and the user said so.
const RECORD_COMMANDS: &[&str] = &[
    "grava",
    "grave",
    "gravar",
    "anota",
    "anote",
    "anotar",
    "guarda",
    "guarde",
    "guardar",
    "lembra que",
    "lembre que",
    "salva",
    "salve",
    "registra",
    "registre",
    "toma nota",
    // en
    "remember that",
    "note that",
    "save this",
    "store this",
];

/// The mirror image: imperatives that name searching.
const ASK_COMMANDS: &[&str] = &[
    "pergunta",
    "pergunte",
    "busca",
    "busque",
    "buscar",
    "procura",
    "procure",
    "procurar",
    "pesquisa",
    "pesquise",
    "me lembra",
    "me lembre",
    "o que eu disse",
    "o que eu falei",
    // Asking by naming a topic rather than an action. These are how a question
    // actually arrives by voice, and their absence is what filed "me fale sobre
    // o EchoBrain" in the store as a memory instead of searching for one.
    "me fala",
    "me fale",
    "me conta",
    "me conte",
    "me diz",
    "me diga",
    "fala sobre",
    "fale sobre",
    "conta sobre",
    "conte sobre",
    // en
    "search for",
    "look up",
    "what did i say",
    "remind me",
    "tell me about",
];

/// Sentence openers that make an utterance a question even without the
/// punctuation — which dictation drops more often than it keeps.
const INTERROGATIVE_OPENERS: &[&str] = &[
    "por que",
    "porque",
    "por quê",
    "pra que",
    "para que",
    "qual",
    "quais",
    "quando",
    "quem",
    "onde",
    "aonde",
    "cadê",
    "cade ",
    "como",
    "quanto",
    "quanta",
    "o que",
    "oque",
    "que que",
    "será que",
    "sera que",
    "você lembra",
    "voce lembra",
    "tu lembra",
    "lembra qual",
    "lembra quando",
    "lembra por que",
    // en
    "why ",
    "what ",
    "when ",
    "where ",
    "who ",
    "which ",
    "how ",
    "did i",
    "do i",
];

/// Cue-phrase intent router. Case-insensitive; the unaccented spellings are
/// listed explicitly rather than stripping accents, matching
/// [`crate::classify::CueClassifier`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CueIntent;

impl CueIntent {
    pub const fn new() -> Self {
        Self
    }
}

impl CueIntent {
    /// The cue that fired, or `None` when none did.
    ///
    /// `None` is the ambiguous middle — the case [`IntentClassifier::classify`]
    /// resolves to [`Intent::Record`] and the case a slower, better router
    /// should be given the chance to decide instead. Separating the two makes
    /// "no cue matched" something a caller can act on rather than a default it
    /// cannot see: in conversation mode, with the microphone open all day, that
    /// default is what turns every ambiguous remark into a memory.
    pub fn cue(&self, text: &str) -> Option<Intent> {
        let haystack = text.trim().to_lowercase();
        if haystack.is_empty() {
            return Some(Intent::Record);
        }

        // 1. An explicit command wins over everything else, including the
        //    question mark below. Matched only as an opener: "quero que você
        //    grave" is a command, but "a gente grava tudo no S3" is a memory
        //    that happens to contain the word.
        if starts_with_any(&haystack, RECORD_COMMANDS) {
            return Some(Intent::Record);
        }
        if starts_with_any(&haystack, ASK_COMMANDS) {
            return Some(Intent::Ask);
        }

        // 2. A question mark is the strongest signal the user can give without
        //    a command, but whisper only emits one when the prosody is clear.
        if haystack.ends_with('?') {
            return Some(Intent::Ask);
        }

        // 3. …so interrogative openers carry the rest, punctuation or not.
        if starts_with_any(&haystack, INTERROGATIVE_OPENERS) {
            return Some(Intent::Ask);
        }

        // 4. The ambiguous middle: no cue at all.
        None
    }
}

impl IntentClassifier for CueIntent {
    /// The cue, or [`Intent::Record`] when there was none. See the module docs
    /// for why the ambiguous middle falls that way, and [`CueIntent::cue`] for
    /// how a caller can decline that default and route it some other way.
    fn classify(&self, text: &str) -> Intent {
        self.cue(text).unwrap_or(Intent::Record)
    }
}

/// Whether the text opens with any of the cues, allowing for the filler a
/// spoken sentence tends to start with ("então grava que…", "ok, busca…").
///
/// The filler is matched as a whole word so that "taxa de acerto" is not read
/// as the filler "ta" followed by "xa…", and the separator is any punctuation
/// whisper might place after it — a comma is the common one.
fn starts_with_any(haystack: &str, cues: &[&str]) -> bool {
    // Greetings sit here rather than in the cue lists because that is what they
    // are: something said before the sentence starts. "Olá, pesquise sobre X"
    // was routed to `Record` purely because nothing stripped the "olá" first.
    const FILLERS: &[&str] = &[
        "então", "entao", "ok", "tá", "ta", "aí", "ai", "e", "olá", "ola", "oi", "opa", "ei",
    ];

    let opener = haystack.trim_start_matches(SEPARATORS);
    let stripped = FILLERS
        .iter()
        .find_map(|filler| {
            let rest = opener.strip_prefix(filler)?;
            // Only a filler if a separator follows: "e grava" is filler, "eu
            // gravo" is not, and "taxa" must not be read as "ta" + "xa".
            rest.starts_with(SEPARATORS)
                .then(|| rest.trim_start_matches(SEPARATORS))
        })
        .unwrap_or(opener);

    cues.iter().any(|cue| stripped.starts_with(cue))
}

/// What can separate a filler word from the command that follows it.
const SEPARATORS: [char; 4] = [' ', ',', ':', '…'];

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(text: &str) -> Intent {
        CueIntent::new().classify(text)
    }

    #[test]
    fn explicit_commands_name_the_action() {
        assert_eq!(intent("grava que o deploy saiu às 3"), Intent::Record);
        assert_eq!(intent("anota: comprar café"), Intent::Record);
        assert_eq!(intent("busca o que eu falei sobre OpenSearch"), Intent::Ask);
        assert_eq!(intent("me lembra da decisão do DynamoDB"), Intent::Ask);
    }

    #[test]
    fn a_record_command_wins_over_a_question_mark() {
        // The user asked for a question to be stored. Storing the question is
        // what they said, and the punctuation does not override the verb.
        assert_eq!(
            intent("grava: por que a gente não foi de OpenSearch?"),
            Intent::Record
        );
    }

    #[test]
    fn a_question_mark_routes_to_ask() {
        assert_eq!(
            intent("a gente decidiu alguma coisa sobre cache?"),
            Intent::Ask
        );
    }

    #[test]
    fn interrogatives_route_to_ask_without_punctuation() {
        // Whisper drops the question mark whenever the prosody is flat, which
        // is most of the time on a short dictated question.
        assert_eq!(intent("por que a gente não foi de OpenSearch"), Intent::Ask);
        assert_eq!(intent("qual era o custo por byte"), Intent::Ask);
        assert_eq!(intent("cadê a anotação da reunião"), Intent::Ask);
        assert_eq!(intent("quando foi que decidimos isso"), Intent::Ask);
    }

    #[test]
    fn narration_is_recorded() {
        assert_eq!(
            intent("preferimos índice vetorial no DynamoDB por causa do custo por byte"),
            Intent::Record
        );
        assert_eq!(
            intent("o Bruno vai assumir o time em janeiro"),
            Intent::Record
        );
    }

    #[test]
    fn a_cue_in_the_middle_of_a_sentence_is_not_a_command() {
        // "grava" appears, but the sentence is narration about a system that
        // records — not a request to record this sentence.
        assert_eq!(
            intent("o serviço grava tudo no S3 antes de indexar"),
            Intent::Record
        );
        // Same shape, but the utterance is a statement, not a search request.
        assert_eq!(
            intent("a gente procura sempre pelo significado, não pela palavra"),
            Intent::Record
        );
    }

    #[test]
    fn spoken_filler_before_a_command_still_routes() {
        assert_eq!(intent("então grava que o deploy saiu"), Intent::Record);
        assert_eq!(intent("ok, busca a decisão do OpenSearch"), Intent::Ask);
    }

    /// Both of these were found sitting in the live store as memories. Neither
    /// has a question mark, neither opens with an interrogative, and each
    /// missed the cue lists for its own reason — so the app filed the question
    /// away instead of answering it, and the user read that as "it did not
    /// understand me".
    #[test]
    fn a_question_that_names_a_topic_instead_of_an_action_is_asked() {
        // Missed because "me fale" was not an ask command.
        assert_eq!(intent("me fale sobre o EchoBrain"), Intent::Ask);
        assert_eq!(
            intent("me conta o que eu decidi sobre o índice"),
            Intent::Ask
        );

        // Missed because "olá" was not a filler, so the opener never reached
        // "pesquise" behind it.
        assert_eq!(intent("Olá, pesquise sobre EchoBrain"), Intent::Ask);
        assert_eq!(intent("oi, me diz onde ficou o modelo"), Intent::Ask);
    }

    #[test]
    fn a_greeting_filler_does_not_swallow_a_statement() {
        // The greeting is stripped, but what follows is still narration, so the
        // default stands. Guards against the new fillers flipping captures.
        assert_eq!(intent("olá, o deploy saiu ontem"), Intent::Record);
        // "oitenta" must not be read as the filler "oi" followed by "tenta".
        assert_eq!(intent("oitenta por cento dos casos passam"), Intent::Record);
    }

    #[test]
    fn a_filler_is_matched_as_a_whole_word() {
        // "taxa" must not be read as the filler "ta" followed by "xa", and
        // "eu" must not be read as the filler "e" followed by "u" — both would
        // shift the opener and could flip the route.
        assert_eq!(intent("taxa de acerto ficou em 90%"), Intent::Record);
        assert_eq!(intent("eu gravo tudo em markdown"), Intent::Record);
    }

    #[test]
    fn routing_is_case_insensitive() {
        assert_eq!(intent("GRAVA que o deploy saiu"), Intent::Record);
        assert_eq!(intent("Por Que a gente não foi de OpenSearch"), Intent::Ask);
    }

    #[test]
    fn silence_falls_back_to_record() {
        // Never reached in practice — `IngestUtterance` rejects empty text
        // before the network — but the router must not panic on it.
        assert_eq!(intent(""), Intent::Record);
        assert_eq!(intent("   "), Intent::Record);
    }

    #[test]
    fn the_wire_form_matches_the_frontend() {
        assert_eq!(Intent::Record.as_str(), "record");
        assert_eq!(Intent::Ask.as_str(), "ask");
    }
}
