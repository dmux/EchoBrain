//! Answering a spoken question from the memories that already exist.
//!
//! Search, then compose. The search half is [`SearchMemories`] unchanged — this
//! adds no retrieval of its own, because a second way to search would be a
//! second thing to keep honest.
//!
//! # The similarity floor is the point of this module
//!
//! A COSINE index always returns *something*. Ask about a topic you never
//! recorded and it hands back the least-unrelated memories in the namespace,
//! ranked, with no signal that they are junk — [`SearchMemories`] passes
//! `min_similarity` straight through and by default there is none.
//!
//! That was survivable when results went on screen as cards with a similarity
//! meter: 12% is visibly 12%. It stops being survivable the moment a model is
//! asked to write a sentence about them and the sentence is *spoken*, where
//! there is no meter and no skimming. So the floor lives here, applied before
//! the model is ever called, and below it the answer is
//! [`Answer::nothing_found`] — composed by nobody, so wrong by nobody.

use std::fmt;
use std::sync::Arc;

use agent_memory_core::TopK;

use crate::error::EchoError;
use crate::model::Answer;
use crate::ports::{Answerer, SentenceSink};
use crate::usecase::search::{SearchMemories, SearchRequest};

/// How many memories are offered to the answerer.
///
/// Five rather than the eight the search view shows: everything here is paid
/// for twice, once as input tokens and once as the model's attention. Past
/// about five the extra memories are the weak tail anyway.
const ANSWER_TOP_K: u32 = 5;

/// The default floor, on `0.0..=1.0`.
///
/// This was 0.30 and never once fired. The calibration behind that number
/// measured only the *hit* side — a paraphrase of a stored memory scores 67% —
/// and assumed a miss would land below 0.30. It does not. `similarity` is
/// `1.0 - distance / 2.0` (see [`SearchMemories`]), so a cosine distance would
/// have to reach 1.4 to score that low, and against a populated namespace
/// nothing does.
///
/// Measured against the live index, asking about something never recorded
/// ("qual a receita do bolo de cenoura da minha avó"), the nearest memory was
/// a wedding date at distance 0.887 — **similarity 0.557**. The observed range
/// is roughly:
///
/// | what                                    | distance | similarity |
/// |-----------------------------------------|----------|------------|
/// | unrelated, different subject entirely   | ~0.89    | ~0.56      |
/// | unrelated, but same language and domain | ~0.76    | ~0.62      |
/// | the right memory, verbose               | ~0.61    | ~0.70      |
/// | a close paraphrase                      | ~0.24    | ~0.88      |
///
/// So the old floor sat *below the metric's own floor*: every miss cleared it,
/// the model was called on five unrelated memories, and it composed a fluent
/// sentence that was then spoken aloud — the one failure this module exists to
/// prevent.
///
/// The band that matters is the second row against the third — 0.620 to 0.696
/// — because the misses that actually reach the top of a result set are not
/// cake recipes. They are notes in the same language about the same kind of
/// thing, and against "por que o app não funciona no Linux?" an unrelated
/// preference about git branches scores 0.620. 0.65 sits in that gap.
///
/// Two things widen the gap, and both matter more than the number here. The
/// weak end of it is a *verbose* memory: a diluted multi-topic embedding is
/// what drags a genuine hit down toward an unrelated one. And the near-misses
/// crowding the top are questions the intent router filed as memories (see
/// [`crate::intent`]), which are near-duplicates of the next question by
/// construction.
///
/// It is still settable, and still shown as a number in the settings screen,
/// because the right value depends on how much is stored — hiding the one dial
/// that decides whether the app can make things up would be hiding the wrong
/// thing.
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.65;

/// Where the answer is going to land, which is the only thing that decides its
/// shape.
///
/// # Why the caller says this rather than the prompt guessing
///
/// The three-sentence, no-markdown answer this module started with was not a
/// house style — it was a constraint of the medium. Spoken aloud there is no
/// skimming, no scrollback and no way to jump to the part you wanted, so a long
/// answer is worse than a short one even when it is more complete. On screen
/// every one of those is false, and the same limit throws away detail the
/// memories actually contain.
///
/// Only the caller knows which one it is: the frontend is the side that knows
/// whether the question was dictated or typed. The safety rules do not vary
/// with this — see [`crate::ports::Answerer`] and the prompt module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnswerStyle {
    /// Read out loud. Short, plain prose.
    #[default]
    Spoken,
    /// Rendered on screen. Markdown, as long as the memories warrant.
    Written,
}

/// A question, as the voice layer asks it.
#[derive(Debug, Clone)]
pub struct AnswerRequest {
    pub question: String,
    /// Drop hits weaker than this. `None` uses [`DEFAULT_MIN_SIMILARITY`];
    /// this is not an "unfiltered" escape hatch, because there is no sensible
    /// reason to compose an answer from arbitrarily bad matches.
    pub min_similarity: Option<f32>,
    /// Defaults to [`AnswerStyle::Spoken`], because speaking is the medium
    /// that cannot recover from a wrong guess: a written answer read aloud is
    /// unbearable, while a spoken answer shown on screen is merely terse.
    pub style: AnswerStyle,
}

impl AnswerRequest {
    pub fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            min_similarity: None,
            style: AnswerStyle::Spoken,
        }
    }

    pub fn with_style(mut self, style: AnswerStyle) -> Self {
        self.style = style;
        self
    }
}

pub struct AnswerQuestion {
    search: Arc<SearchMemories>,
    answerer: Arc<dyn Answerer>,
    min_similarity: f32,
}

impl fmt::Debug for AnswerQuestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnswerQuestion")
            .field("min_similarity", &self.min_similarity)
            .finish_non_exhaustive()
    }
}

impl AnswerQuestion {
    pub fn new(search: Arc<SearchMemories>, answerer: Arc<dyn Answerer>) -> Self {
        Self {
            search,
            answerer,
            min_similarity: DEFAULT_MIN_SIMILARITY,
        }
    }

    /// Override the floor for every request this instance serves. Values
    /// outside `0.0..=1.0` are clamped rather than rejected: the setting
    /// arrives from a slider, and a slider cannot usefully fail.
    pub fn with_min_similarity(mut self, min_similarity: f32) -> Self {
        self.min_similarity = min_similarity.clamp(0.0, 1.0);
        self
    }

    pub async fn execute(
        &self,
        request: AnswerRequest,
        sink: &mut dyn SentenceSink,
    ) -> Result<Answer, EchoError> {
        let question = request.question.trim();
        if question.is_empty() {
            // Same reasoning as `SearchMemories`: embedding whitespace costs
            // money and ranks the whole namespace against nothing.
            return Ok(Answer::nothing_found());
        }

        let floor = request
            .min_similarity
            .map(|value| value.clamp(0.0, 1.0))
            .unwrap_or(self.min_similarity);

        let hits = self
            .search
            .execute(SearchRequest {
                text: question.to_string(),
                top_k: TopK::new(ANSWER_TOP_K).unwrap_or(TopK::DEFAULT),
                kind: None,
                min_similarity: Some(floor),
            })
            .await?;

        if hits.is_empty() {
            // The model is never called. Nothing found is not a hard question
            // to answer — it is the absence of one, and inventing a sentence
            // for it is exactly the failure this whole module is shaped around.
            return Ok(Answer::nothing_found());
        }

        let answer = self
            .answerer
            .answer(question, &hits, request.style, sink)
            .await?;

        // A grounded answer must never reach the UI without its sources, so if
        // the adapter reported none, the hits it was given are attached here.
        //
        // Only when grounded. An answer that declined has no sources *by
        // definition* — it is the model saying these memories do not answer the
        // question — and backfilling there would render cards underneath a
        // refusal, implying the refusal came from them. Which is what this did
        // until a test said otherwise.
        if !answer.grounded {
            return Ok(answer);
        }

        Ok(Answer {
            cited: if answer.cited.is_empty() {
                hits
            } else {
                answer.cited
            },
            ..answer
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    use std::time::Duration;

    use agent_memory_core::testing::{
        FixedClock, InMemoryRepository, SeqIdGenerator, StubEmbedder,
    };
    use agent_memory_core::{LocalMemoryService, MemoryKind, MemoryService, RememberCommand};

    use crate::model::{Hit, caller_placeholder};
    use crate::ports::Discard;

    /// Titan Text Embeddings V2 produces 1024 floats, matching the deployed
    /// index — the same constant `tests/usecases.rs` uses.
    const DIMENSIONS: usize = 1024;

    /// Records what it was handed and answers with a fixed sentence.
    #[derive(Debug, Default)]
    struct SpyAnswerer {
        calls: Mutex<Vec<(String, usize)>>,
        styles: Mutex<Vec<AnswerStyle>>,
    }

    #[async_trait::async_trait]
    impl Answerer for SpyAnswerer {
        async fn answer(
            &self,
            question: &str,
            hits: &[Hit],
            style: AnswerStyle,
            sink: &mut dyn SentenceSink,
        ) -> Result<Answer, EchoError> {
            self.calls
                .lock()
                .expect("not poisoned")
                .push((question.to_string(), hits.len()));
            self.styles.lock().expect("not poisoned").push(style);
            sink.sentence("Foi por causa do custo por byte.");
            Ok(Answer {
                text: "Foi por causa do custo por byte.".into(),
                cited: hits.to_vec(),
                grounded: true,
                consulted: true,
            })
        }
    }

    impl SpyAnswerer {
        fn calls(&self) -> Vec<(String, usize)> {
            self.calls.lock().expect("not poisoned").clone()
        }

        fn styles(&self) -> Vec<AnswerStyle> {
            self.styles.lock().expect("not poisoned").clone()
        }
    }

    /// Collects sentences so a test can assert the streaming contract.
    #[derive(Debug, Default)]
    struct Collect(Vec<String>);

    impl SentenceSink for Collect {
        fn sentence(&mut self, sentence: &str) {
            self.0.push(sentence.to_string());
        }
    }

    async fn service_with(texts: &[&str]) -> Arc<dyn MemoryService> {
        let service = LocalMemoryService::new(
            InMemoryRepository::default(),
            StubEmbedder::new(DIMENSIONS),
            FixedClock::new(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            SeqIdGenerator::default(),
        );
        let user = caller_placeholder().expect("valid");
        for text in texts {
            service
                .remember(RememberCommand {
                    user_id: user.clone(),
                    kind: MemoryKind::Episode,
                    text: (*text).to_string(),
                    ttl: None,
                    source: Some("hotkey".into()),
                    github_login: None,
                    github_repo: None,
                    rating: None,
                    active: true,
                })
                .await
                .expect("remembered");
        }
        Arc::new(service)
    }

    fn answering(
        memory: Arc<dyn MemoryService>,
        answerer: Arc<SpyAnswerer>,
        floor: f32,
    ) -> AnswerQuestion {
        AnswerQuestion::new(Arc::new(SearchMemories::new(memory)), answerer)
            .with_min_similarity(floor)
    }

    #[tokio::test]
    async fn an_empty_question_never_reaches_the_model() {
        let spy = Arc::new(SpyAnswerer::default());
        let answering = answering(service_with(&["algo"]).await, Arc::clone(&spy), 0.0);

        let answer = answering
            .execute(AnswerRequest::new("   "), &mut Discard)
            .await
            .expect("no error");

        assert!(!answer.grounded);
        assert_eq!(answer.text, Answer::NOTHING_FOUND);
        assert!(spy.calls().is_empty(), "the model must not be called");
    }

    #[tokio::test]
    async fn nothing_above_the_floor_means_nothing_found_and_no_model_call() {
        let spy = Arc::new(SpyAnswerer::default());
        // A floor of 1.0 admits only an exact-vector match, so nothing clears
        // it — the same shape as asking about a topic never recorded.
        let answering = answering(
            service_with(&["preferimos DynamoDB a OpenSearch"]).await,
            Arc::clone(&spy),
            1.0,
        );

        let answer = answering
            .execute(
                AnswerRequest::new("qual o melhor risoto de Lisboa?"),
                &mut Discard,
            )
            .await
            .expect("no error");

        assert!(!answer.grounded);
        assert!(answer.cited.is_empty());
        assert!(
            !answer.consulted,
            "nothing cleared the floor, so nothing may report as a model call"
        );
        assert!(
            spy.calls().is_empty(),
            "an ungrounded question must not cost a token"
        );
    }

    #[tokio::test]
    async fn a_model_that_declines_is_reported_as_consulted_and_therefore_billed() {
        /// Answers the way the real adapter does when the memories cleared the
        /// floor but do not answer the question.
        #[derive(Debug)]
        struct Declines;

        #[async_trait::async_trait]
        impl Answerer for Declines {
            async fn answer(
                &self,
                _question: &str,
                _hits: &[Hit],
                _style: AnswerStyle,
                _sink: &mut dyn SentenceSink,
            ) -> Result<Answer, EchoError> {
                Ok(Answer::declined())
            }
        }

        let answering = AnswerQuestion::new(
            Arc::new(SearchMemories::new(
                service_with(&["preferimos DynamoDB a OpenSearch"]).await,
            )),
            Arc::new(Declines),
        )
        .with_min_similarity(0.0);

        let answer = answering
            .execute(AnswerRequest::new("qual o melhor risoto?"), &mut Discard)
            .await
            .expect("no error");

        // Identical to `nothing_found` in every visible way…
        assert!(!answer.grounded);
        assert!(answer.cited.is_empty());
        assert_eq!(answer.text, Answer::NOTHING_FOUND);
        // …except the one that says it cost money. Without this, anything
        // reporting cost has to guess, and the first thing that did guessed
        // wrong.
        assert!(answer.consulted);
    }

    #[tokio::test]
    async fn a_grounded_question_reaches_the_model_and_streams_sentences() {
        let spy = Arc::new(SpyAnswerer::default());
        let answering = answering(
            service_with(&["preferimos DynamoDB a OpenSearch pelo custo por byte"]).await,
            Arc::clone(&spy),
            0.0,
        );

        let mut collected = Collect::default();
        let answer = answering
            .execute(
                AnswerRequest::new("por que não OpenSearch?"),
                &mut collected,
            )
            .await
            .expect("no error");

        assert!(answer.grounded);
        assert!(
            !answer.cited.is_empty(),
            "a grounded answer carries its sources"
        );
        assert_eq!(collected.0, vec!["Foi por causa do custo por byte."]);

        let calls = spy.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "por que não OpenSearch?");
    }

    #[tokio::test]
    async fn an_answer_that_names_no_sources_still_carries_the_hits_it_saw() {
        /// Answers without filling `cited` — the shape of an adapter that
        /// could not parse its own citations.
        #[derive(Debug)]
        struct Uncited;

        #[async_trait::async_trait]
        impl Answerer for Uncited {
            async fn answer(
                &self,
                _question: &str,
                _hits: &[Hit],
                _style: AnswerStyle,
                _sink: &mut dyn SentenceSink,
            ) -> Result<Answer, EchoError> {
                Ok(Answer {
                    text: "Alguma coisa.".into(),
                    cited: Vec::new(),
                    grounded: true,
                    consulted: true,
                })
            }
        }

        let answering = AnswerQuestion::new(
            Arc::new(SearchMemories::new(
                service_with(&["preferimos DynamoDB a OpenSearch"]).await,
            )),
            Arc::new(Uncited),
        )
        .with_min_similarity(0.0);

        let answer = answering
            .execute(AnswerRequest::new("por que não OpenSearch?"), &mut Discard)
            .await
            .expect("no error");

        assert!(
            !answer.cited.is_empty(),
            "a grounded answer must never reach the UI without its sources"
        );
    }

    #[tokio::test]
    async fn the_floor_is_clamped_rather_than_rejected() {
        let spy = Arc::new(SpyAnswerer::default());
        let answering = answering(service_with(&["algo"]).await, spy, 0.0);

        // Arrives from a slider; a slider cannot usefully return an error.
        let answer = answering
            .execute(
                AnswerRequest {
                    question: "algo".into(),
                    min_similarity: Some(9.0),
                    style: AnswerStyle::Spoken,
                },
                &mut Discard,
            )
            .await;

        assert!(
            answer.is_ok(),
            "an out-of-range floor clamps instead of failing"
        );
    }

    #[tokio::test]
    async fn the_requested_style_reaches_the_answerer() {
        // The use case has no opinion on form — it is the caller that knows
        // whether the answer is going to be heard or read, so this only has to
        // carry the word across without editing it.
        for style in [AnswerStyle::Spoken, AnswerStyle::Written] {
            let spy = Arc::new(SpyAnswerer::default());
            let answering = answering(service_with(&["algo"]).await, spy.clone(), 0.0);

            answering
                .execute(
                    AnswerRequest::new("por que não OpenSearch?").with_style(style),
                    &mut Discard,
                )
                .await
                .expect("no error");

            assert_eq!(spy.styles(), vec![style]);
        }
    }

    #[tokio::test]
    async fn a_request_built_without_a_style_is_spoken() {
        // Speaking is the medium that cannot recover from the wrong guess: a
        // written answer read out loud is unusable, while a spoken answer shown
        // on screen is merely terse.
        assert_eq!(
            AnswerRequest::new("por que não OpenSearch?").style,
            AnswerStyle::Spoken
        );
    }

    /// The regression guard for the floor that never fired.
    ///
    /// A stub embedder cannot reproduce the real index's geometry, so this
    /// pins the two measurements the constant was chosen from instead. Both
    /// bounds matter: drop the floor back toward 0.30 and the first assert
    /// fails, raise it past a real-but-verbose match and the second does.
    #[test]
    fn the_default_floor_separates_a_miss_from_a_hit() {
        // similarity = 1 - distance / 2, per `SearchMemories`.
        let similarity = |distance: f32| 1.0 - distance / 2.0;

        // Measured against the live index, and deliberately the *tightest* miss
        // observed rather than the most obvious one: asking "por que o
        // aplicativo não funciona no Linux?" pulls an unrelated preference about
        // git branching in at 0.760. A question about something entirely
        // foreign — "qual a receita do bolo de cenoura da minha avó" — only
        // reaches 0.887, so pinning against that would leave the floor free to
        // drift down into the range that actually bites.
        let unrelated = similarity(0.760);

        // "por que o aplicativo não funciona no Linux?" against the memory that
        // actually answers it. Verbose memories score worse than short ones,
        // so this is the weak end of a genuine hit, not the typical one.
        let genuine = similarity(0.609);

        assert!(
            DEFAULT_MIN_SIMILARITY > unrelated,
            "a floor at or below {unrelated} lets every miss through to the \
             model, which then composes a sentence and speaks it aloud"
        );
        assert!(
            DEFAULT_MIN_SIMILARITY <= genuine,
            "a floor above {genuine} rejects memories that do answer the \
             question, and the app says it knows nothing when it does"
        );
    }
}
