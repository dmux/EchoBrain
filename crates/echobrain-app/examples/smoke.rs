//! End-to-end check against the **deployed** API. Costs a few cents.
//!
//! Runs the real composition root — `echobrain_lib::build_state()`, the same
//! function the Tauri app calls — so what it proves is the production wiring,
//! not a copy of it.
//!
//! ```bash
//! env -u AWS_ENDPOINT_URL -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY \
//!   AWS_PROFILE=my-profile cargo run -p echobrain-app --example smoke
//! ```
//!
//! Unsetting the LocalStack variables is not optional on a machine that exports
//! them: signing with `AWS_ACCESS_KEY_ID=test` produces a 403 from the real API
//! that reads like a permissions problem and is not one.

use std::time::{Duration, Instant};

use echobrain_core::{IngestRequest, SearchRequest, TopK};

/// DynamoDB vector search is eventually consistent. The sibling project's own
/// smoke test uses the same budget; asserting immediately is a guaranteed flake.
const CONSISTENCY_TIMEOUT: Duration = Duration::from_secs(90);
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("echobrain=info,warn")
        .init();

    let state = echobrain_lib::build_state().await?;

    // Unique, so a search cannot pass by matching a memory from an earlier run.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let text = format!(
        "Para o teste {nonce}: preferimos índice vetorial no DynamoDB a OpenSearch \
         por causa do custo por byte processado."
    );

    println!("→ gravando: {text}");
    let stored = state
        .aws()
        .ingest
        .execute(IngestRequest {
            text: text.clone(),
            kind: None,
            // An hour, so a failed run cleans itself up instead of leaving
            // debris in a namespace that has no list endpoint to audit it with.
            ttl: Some(Duration::from_secs(3600)),
            github_repo: None,
        })
        .await?;

    println!(
        "✓ gravado  id={} kind={} state={}",
        stored.entry.memory_id,
        stored.entry.kind,
        stored.entry.state.as_str()
    );
    assert!(
        stored.journal_failed.is_none(),
        "the local journal write failed: {:?}",
        stored.journal_failed
    );

    // Deliberately not the stored wording: the point of the whole system is
    // that recall works on meaning, not on remembering the exact phrasing.
    let query = "por que a gente não foi de OpenSearch?";
    println!("→ buscando por sinônimo: {query:?}");

    let started = Instant::now();
    loop {
        let hits = state
            .aws()
            .search
            .execute(SearchRequest {
                text: query.to_string(),
                top_k: TopK::new(10)?,
                kind: None,
                min_similarity: None,
            })
            .await?;

        if let Some(hit) = hits
            .iter()
            .find(|hit| hit.memory.memory_id == stored.entry.memory_id)
        {
            println!(
                "✓ encontrado em {:?}  similaridade={}%",
                started.elapsed(),
                hit.similarity.percent()
            );
            println!("  {}", hit.memory.text);

            println!("→ apagando");
            state
                .aws()
                .memory
                .forget(
                    &echobrain_core::caller_placeholder()?,
                    &stored.entry.memory_id,
                )
                .await?;
            println!("✓ apagado. Tudo certo.");
            return Ok(());
        }

        if started.elapsed() > CONSISTENCY_TIMEOUT {
            return Err(format!(
                "a memória não apareceu na busca em {CONSISTENCY_TIMEOUT:?}; \
                 {} outros resultados voltaram",
                hits.len()
            )
            .into());
        }

        println!("  … índice ainda não alcançou ({:?})", started.elapsed());
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
