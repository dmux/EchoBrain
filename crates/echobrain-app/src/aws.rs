//! Everything that depends on which AWS account the app is talking to.
//!
//! # Why this is a struct rather than five fields on `AppState`
//!
//! `endpoint`, `region` and `profile` are editable from the settings screen, and
//! a change to any of them has to reach the memory API, Bedrock *and* Polly. If
//! those lived as independent fields the app could end up half moved: searching
//! against one account while speaking through another, with credentials that
//! resolve for one and not the other. Naming the group makes the swap atomic —
//! callers take one snapshot and everything in it agrees.
//!
//! What is deliberately *not* in here: the journal, the microphone, the
//! transcriber and the system voice. None of them know about AWS, and rebuilding
//! the journal in particular would drop the record of what this device captured.

use std::sync::Arc;

use agent_memory_client::RemoteMemoryService;
use echobrain_core::{
    AnswerQuestion, Answerer, CueClassifier, IngestUtterance, Journal, MemoryService,
    SearchMemories,
};
use echobrain_llm::BedrockAnswerer;

use crate::config::Config;

/// Provenance stamped onto every memory this app writes, so a later audit can
/// tell dictated captures from ones an agent wrote over MCP.
pub const SOURCE: &str = "echobrain-macos";

/// The account-dependent half of the application, rebuilt as a unit.
pub struct AwsStack {
    /// Shared with [`AwsStack::answer`], which searches through the very same
    /// instance — the answering path must not be able to drift from the search
    /// path on filters or ordering.
    pub search: Arc<SearchMemories>,
    pub ingest: IngestUtterance,
    /// Held directly for `forget`, which is a deletion rather than a use case.
    pub memory: Arc<dyn MemoryService>,

    /// `None` when no answering model is configured.
    ///
    /// Answering is the one part of EchoBrain that spends tokens, so it has to
    /// be possible to run without it: capture and search keep working, and the
    /// UI hides the ask path rather than offering a question that always errors.
    pub answer: Option<Arc<AnswerQuestion>>,

    /// Decides the utterances the cue router declined. `None` when no answering
    /// model is configured, in which case the cue default stands.
    pub intent_judge: Option<Arc<dyn echobrain_core::IntentJudge>>,

    /// Handed to the speaker rather than held for its own sake — [`PollySpeaker`]
    /// keeps the client it synthesises with, and a rebuild has to replace it or
    /// the voice would keep signing as the previous account.
    ///
    /// [`PollySpeaker`]: echobrain_audio::PollySpeaker
    pub polly_client: aws_sdk_polly::Client,

    pub reconciler: Arc<echobrain_core::usecase::reconcile::Reconciler>,
}

impl std::fmt::Debug for AwsStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsStack").finish_non_exhaustive()
    }
}

/// Resolve credentials for `config` and wire everything that needs them.
///
/// # What this does not prove
///
/// Resolving an `SdkConfig` is local: it reads `~/.aws/config` and prepares a
/// credential chain. A profile that does not exist, or whose SSO session has
/// expired, fails on the *first call*, not here. So a successful return means
/// "the settings are well-formed", not "the account is reachable" — the honest
/// way to find out is the next search, and the SSO button is the remedy.
///
/// Proving it here would mean making a call, and the only calls available cost
/// either an embedding or a token on every save.
pub async fn build(config: &Config, journal: Arc<dyn Journal>) -> AwsStack {
    tracing::info!(
        endpoint = %config.endpoint,
        region = %config.region,
        profile = config.profile.as_deref().unwrap_or("<default chain>"),
        "reaching the memory API"
    );

    // Region is set explicitly rather than taken from the profile: it must be
    // the *API's* region, because that is what SigV4 signs for. A profile in
    // sa-east-1 signing for an API in us-east-1 fails in a way that reads like
    // a credentials problem and is not one.
    let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.region.clone()));
    if let Some(profile) = &config.profile {
        loader = loader.profile_name(profile);
    }
    let sdk_config = loader.load().await;

    // Launching the bundle from a terminal drags that terminal's AWS variables
    // in with it, and this machine's shell exports LocalStack's. Two of them
    // land below the config layer, where `with_env_overrides` cannot see them:
    // `AWS_ENDPOINT_URL` redirects every SDK client at localhost, and
    // `AWS_ACCESS_KEY_ID` outranks the named profile in the credential chain.
    // The result is an app that looks configured and cannot reach AWS, so say
    // so once, by name, rather than leaving it to a connection error.
    for leaked in ["AWS_ENDPOINT_URL", "AWS_ACCESS_KEY_ID"] {
        if let Ok(value) = std::env::var(leaked)
            && !value.trim().is_empty()
        {
            tracing::warn!(
                variable = leaked,
                %value,
                "inherited from the launching shell and it outranks config.toml — \
                 launch from the Finder, or strip it as `make run` does"
            );
        }
    }

    // `with_config` rather than `new`: a `.app` launched from the Finder
    // inherits no shell profile, so the ambient-environment path cannot work.
    // Cloned rather than moved: the answering adapter signs against the same
    // resolved credentials, which is the whole reason answering adds no secret
    // of its own.
    let memory: Arc<dyn MemoryService> = Arc::new(RemoteMemoryService::with_config(
        sdk_config.clone(),
        &config.endpoint,
        config.github_login.clone(),
        None, // github_repo is inferred by MCP, not the app
    ));

    // Pinned to the real regional endpoint instead of `Client::new`, which
    // inherits whatever `AWS_ENDPOINT_URL` says. There is no such thing as a
    // local Polly — synthesis is the one call here that can only be answered by
    // AWS — so honouring that variable can only ever send speech to a LocalStack
    // that will refuse it, which is exactly how this broke.
    let polly_client = aws_sdk_polly::Client::from_conf(
        aws_sdk_polly::config::Builder::from(&sdk_config)
            .endpoint_url(format!("https://polly.{}.amazonaws.com", config.region))
            .build(),
    );

    // The only part of EchoBrain that spends tokens, and the only part that
    // produces text nobody dictated. Off by default is not the right posture —
    // it is what makes voice search work at all — but it must be *absent*
    // gracefully, because model access on Bedrock is granted per account and
    // per region and cannot be checked from here without making a call.
    let search = Arc::new(SearchMemories::new(Arc::clone(&memory)));
    let answer = config.voice.answer_model.as_ref().map(|model_id| {
        tracing::info!(model = %model_id, "answering ready");
        let answerer: Arc<dyn Answerer> = Arc::new(
            BedrockAnswerer::new(&sdk_config, model_id)
                .with_max_tokens(config.voice.answer_max_tokens),
        );
        Arc::new(
            AnswerQuestion::new(Arc::clone(&search), answerer)
                .with_min_similarity(config.voice.min_similarity),
        )
    });
    if answer.is_none() {
        tracing::info!("no `answer_model` configured; voice search will not answer");
    }

    // Rides on the same model as answering rather than adding a setting: an
    // account entitled to one is entitled to the other, and a second model id
    // would be a second thing to get wrong. It only sees the utterances the cue
    // router declined, so the cost is bounded by how ambiguous the speech is.
    let intent_judge: Option<Arc<dyn echobrain_core::IntentJudge>> =
        config.voice.answer_model.as_ref().map(|model_id| {
            tracing::info!(model = %model_id, "intent routing ready");
            Arc::new(echobrain_llm::BedrockIntentJudge::new(
                &sdk_config,
                model_id,
            )) as Arc<dyn echobrain_core::IntentJudge>
        });

    let reconciler = Arc::new(echobrain_core::usecase::reconcile::Reconciler::new(
        journal.clone(),
        memory.clone(),
    ));

    AwsStack {
        ingest: IngestUtterance::new(
            Arc::clone(&memory),
            Arc::new(CueClassifier::new()),
            journal,
            SOURCE,
        ),
        // The same instance the answering path searches through, not a second
        // one built from the same service: one search, one set of rules.
        search,
        memory,
        answer,
        intent_judge,
        polly_client,
        reconciler,
    }
}
