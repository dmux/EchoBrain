//! One spoken turn, end to end, with the clock running on each step.
//!
//! Costs real money — an embedding, a vector search and a model call — so it is
//! a `make` target rather than a test. What it is for is the numbers: latency
//! is the whole product here, and the only honest way to know where it goes is
//! to measure the real path rather than reason about it.
//!
//! ```text
//! make conversation
//! ```
//!
//! ```text
//! make conversation                                  # ask about what is stored
//! cargo run -p echobrain-app --example conversation \
//!     "por que não OpenSearch?" "preferimos DynamoDB" # seed, then ask
//! ```
//!
//! Needs `answer_model` set in `config.toml` and a live SSO session
//! (`aws sso login --profile my-profile`).

use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use echobrain_core::{AnswerRequest, SentenceSink};

/// Speaks each sentence as it closes, and reports when the first one landed.
///
/// Time-to-first-syllable is the number that decides whether the app feels
/// alive: it is what the streaming exists for, and it is invisible in any
/// measurement of the whole answer.
struct SpeakAndTime {
    speaker: Option<Arc<dyn echobrain_core::Speaker>>,
    started: Instant,
    first: Option<f32>,
    sentences: usize,
}

impl SentenceSink for SpeakAndTime {
    fn sentence(&mut self, sentence: &str) {
        self.sentences += 1;
        if self.first.is_none() {
            let elapsed = self.started.elapsed().as_secs_f32();
            self.first = Some(elapsed);
            println!("  ⏱  primeira frase em {elapsed:.2}s");
        }
        println!("  » {sentence}");
        if let Some(speaker) = self.speaker.clone() {
            let sentence = sentence.to_string();
            // Detached on purpose: the answer keeps streaming while the voice
            // catches up, which is exactly what the app does.
            tauri::async_runtime::spawn(async move {
                let _ = speaker.speak(&sentence).await;
            });
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("echobrain=info,warn")
        .init();

    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "por que a gente não foi de OpenSearch?".to_string());

    println!("→ montando o app…");
    let state = echobrain_lib::build_state().await?;

    // Seed a memory first when asked, so the grounded path can be demonstrated
    // on a namespace that does not happen to contain the answer already.
    // `make smoke` deletes what it writes, which is correct for a smoke test
    // and leaves nothing for this one to find.
    if let Some(text) = std::env::args().nth(2) {
        println!("→ gravando: {text}");
        let stored = state
            .aws()
            .ingest
            .execute(echobrain_core::IngestRequest {
                text,
                kind: None,
                ttl: None,
                github_repo: None,
            })
            .await?;
        println!("✓ gravado id={}", stored.entry.memory_id);
        // The vector index is eventually consistent; the sibling project
        // budgets 90 s. Waiting is cheaper than a confusing "not found".
        println!("→ esperando o índice alcançar…");
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    let Some(answering) = state.aws().answer.clone() else {
        eprintln!(
            "✗ nenhum `answer_model` em config.toml. Descubra o seu com:\n\
             \n    aws bedrock list-inference-profiles --region us-east-1 \\\n\
             \x20     --query 'inferenceProfileSummaries[?contains(inferenceProfileId, `anthropic`)].inferenceProfileId'\n"
        );
        std::process::exit(1);
    };

    // The same routing the hotkey does, on the same text.
    let intent = state.intent.classify(&question);
    println!("→ roteado como: {intent}");
    if intent != echobrain_core::Intent::Ask {
        println!("  (a pergunta seria guardada, não respondida — ajuste a frase)");
    }

    println!("→ perguntando: {question}");
    std::io::stdout().flush().ok();

    let started = Instant::now();
    let mut sink = SpeakAndTime {
        speaker: state.speaker.clone(),
        started,
        first: None,
        sentences: 0,
    };

    let answer = answering
        .execute(AnswerRequest::new(&question), &mut sink)
        .await?;
    let total = started.elapsed().as_secs_f32();

    println!();
    if answer.grounded {
        println!(
            "✓ respondido em {total:.2}s a partir de {} memórias:",
            answer.cited.len()
        );
        for hit in &answer.cited {
            println!("   [{:>3}%] {}", hit.similarity.percent(), hit.memory.text);
        }
    } else if answer.consulted {
        // The memories cleared the floor, the model read them and said they do
        // not answer the question. A billed call, and reporting it as free was
        // the first thing this example got wrong when it ran for real.
        println!("✓ o modelo leu as memórias e recusou, em {total:.2}s — chamada cobrada.");
    } else {
        println!(
            "✓ nada passou do piso de semelhança em {total:.2}s — o modelo não foi chamado, \
             nenhum token gasto."
        );
    }

    println!();
    println!("resposta: {}", answer.text);
    println!();
    println!(
        "latência: {:.2}s até a primeira frase, {total:.2}s no total, {} frases",
        sink.first.unwrap_or(total),
        sink.sentences
    );

    // Let the last sentence finish playing before the process exits and the
    // speaker's `Drop` kills it mid-word.
    if let Some(speaker) = &state.speaker {
        while speaker.is_speaking() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    Ok(())
}
