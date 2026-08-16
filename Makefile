.PHONY: check fmt lint test ui-check run ui site site-build smoke model voice-model transcribe build dmg clean help

# The AWS profile the targets that reach the real API sign with.
#
# A plain `=` rather than `?=` on purpose: `?=` yields to the environment, and
# yielding to the environment is the exact bug the `env -u` flags below exist to
# prevent — a shell exporting LocalStack credentials would silently take over.
#
# Override per invocation with `make run PROFILE=your-profile`, or permanently
# by writing `PROFILE = your-profile` into `local.mk`, which is untracked.
PROFILE = my-profile
-include local.mk

# Where `echobrain-audio` looks for the speech model.
MODEL_DIR  := $(HOME)/Library/Application Support/EchoBrain/models
MODEL_NAME := ggml-large-v3-turbo-q5_0.bin
MODEL_URL  := https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$(MODEL_NAME)

# The speaker-embedding model, used only by voice enrolment and the identity
# filter. Small next to whisper (~26 MB), and separate because the app runs
# fine without it — the filter simply never fires. The filename is what
# `require_model` asks for, so the download is renamed on the way in.
VOICE_MODEL_NAME := wespeaker.onnx
VOICE_MODEL_URL  := https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx

# Mirrors the sibling memory project's `make check`: everything that runs for
# free, with no AWS account and no network.
check: fmt lint test ui-check

fmt:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --workspace --all-features

# `next build` is the lint that matters most here: it fails if anything in the
# UI violates `output: "export"` — a Server Action, a route handler, a dynamic
# route without `generateStaticParams`.
ui-check:
	cd ui && pnpm lint && pnpm build

# Run the app against the deployed API.
#
# The UI is rebuilt first because `tauri.conf.json` deliberately has no
# `devUrl`: the window always loads the bundle embedded from `ui/out`. With a
# `devUrl` set, a debug build points the webview at a dev server instead, and
# launching the binary without one gives a silent white window.
#
# The three `-u` flags are not optional on this machine: the shell exports
# LocalStack credentials (`AWS_ACCESS_KEY_ID=test`, `AWS_ENDPOINT_URL=...`),
# and signing with them produces a 403 from the real API that reads like a
# permissions problem. Requires a live SSO session — run `aws sso login
# --profile $(PROFILE)` first.
run:
	cd ui && pnpm build
	env -u AWS_ENDPOINT_URL -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY \
	    AWS_PROFILE=$(PROFILE) RUST_LOG=echobrain=debug,info \
	    cargo run -p echobrain-app

# UI work with hot reload, in a browser, against FakeGateway. No Rust, no AWS,
# no cost — which is where interface iteration actually belongs.
ui:
	cd ui && pnpm dev

# The landing page (site/), deployed to GitHub Pages by .github/workflows/pages.yml.
#
# Deliberately not part of `check`: it would put a Node build on the Rust inner
# loop, and the Pages workflow already refuses to deploy a site that fails lint
# or build.
site:
	cd site && pnpm dev

# The static export, into site/out. `BASE_PATH` defaults to /EchoBrain because
# the site is served from a project page; serve the result under that exact
# subpath when checking it, or every asset link will look fine and be wrong.
site-build:
	cd site && pnpm build

# End-to-end against the deployed API. Costs a few cents.
smoke:
	env -u AWS_ENDPOINT_URL -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY \
	    AWS_PROFILE=$(PROFILE) cargo run -p echobrain-app --example smoke

# ~570 MB, once. Not bundled with the app: it would dominate the download, it
# changes on a different cadence from the code, and its licence is not this
# project's to redistribute.
model: voice-model
	@mkdir -p "$(MODEL_DIR)"
	@if [ -f "$(MODEL_DIR)/$(MODEL_NAME)" ]; then \
	  echo "já instalado: $(MODEL_DIR)/$(MODEL_NAME)"; \
	else \
	  echo "baixando $(MODEL_NAME) (~570 MB)…"; \
	  curl -L --fail --progress-bar -o "$(MODEL_DIR)/$(MODEL_NAME).part" "$(MODEL_URL)" && \
	  mv "$(MODEL_DIR)/$(MODEL_NAME).part" "$(MODEL_DIR)/$(MODEL_NAME)"; \
	fi

# ~26 MB, once. Without it `stop_and_measure_voice` refuses and enrolment
# cannot start, which is the whole of the voice-identity feature.
voice-model:
	@mkdir -p "$(MODEL_DIR)"
	@if [ -f "$(MODEL_DIR)/$(VOICE_MODEL_NAME)" ]; then \
	  echo "já instalado: $(MODEL_DIR)/$(VOICE_MODEL_NAME)"; \
	else \
	  echo "baixando $(VOICE_MODEL_NAME) (~26 MB)…"; \
	  curl -L --fail --progress-bar -o "$(MODEL_DIR)/$(VOICE_MODEL_NAME).part" "$(VOICE_MODEL_URL)" && \
	  mv "$(MODEL_DIR)/$(VOICE_MODEL_NAME).part" "$(MODEL_DIR)/$(VOICE_MODEL_NAME)"; \
	fi

# Record five seconds and print the transcript — audio only, no Tauri and no
# AWS, so a dictation problem can be pinned on whisper or the microphone rather
# than on everything at once. macOS attaches the permission to the terminal.
transcribe:
	cargo run -p echobrain-audio --example transcribe

# Proves whisper transcribes real Portuguese, using speech from `say` rather
# than the microphone: macOS hands an unpermitted process silence instead of an
# error, and whisper hallucinates confident phrases over silence.
verify-audio:
	cargo test -p echobrain-audio --test transcription -- --ignored --nocapture

# Proves the app can actually speak, by closing the loop: `say` writes a file,
# whisper reads it back, the test asserts the words survived. No human ear in
# the loop, which is the only way this can run unattended.
#
# The audible tests in `speech.rs` are separate and stay ignored — they play
# through the speakers, which no CI should do.
verify-tts:
	cargo test -p echobrain-audio --all-features --test speech_roundtrip -- --ignored --nocapture

# Proves the endpointer finds the edges of real speech, using `say` rather than
# the microphone — same reason as verify-audio.
verify-vad:
	cargo test -p echobrain-audio --all-features --test endpointing -- --ignored --nocapture

# The record-vs-ask routing table. Pure, no I/O, no model — runs in `make check`
# too, and listed here because it is the thing to re-run after touching a cue.
verify-intent:
	cargo test -p echobrain-core --all-features intent -- --nocapture

# One spoken turn end to end, printing the latency of each step and what the
# answer cost. Needs `answer_model` in config.toml and a live SSO session.
conversation:
	env -u AWS_ENDPOINT_URL -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY \
	    AWS_PROFILE=$(PROFILE) RUST_LOG=echobrain=debug,info \
	    cargo run -p echobrain-app --example conversation

build:
	cd ui && pnpm build
	cargo build --release -p echobrain-app

# The installable artifact, which `build` does not produce: that stops at a
# bare binary, and a bare binary has no Info.plist — so it dies the instant it
# opens the microphone, for want of NSMicrophoneUsageDescription.
#
# Run from the crate directory because that is where `tauri.conf.json` lives,
# and via the CLI in `ui/node_modules` because it is a devDependency there
# rather than a `cargo install`. The UI is rebuilt by `beforeBuildCommand`, so
# this does not depend on `build`.
#
# Ad-hoc signed, not notarised: Gatekeeper will refuse it on any Mac but this
# one until `APPLE_SIGNING_IDENTITY` and notarisation credentials are set.
dmg:
	cd crates/echobrain-app && ../../ui/node_modules/.bin/tauri build
	@echo "→ target/release/bundle/dmg/"

clean:
	cargo clean
	rm -rf ui/out ui/.next site/out site/.next

help:
	@echo "check     fmt, clippy -D warnings, Rust tests, UI lint + static export"
	@echo "run       build the UI, then run the app against the deployed API"
	@echo "ui        UI work in a browser against FakeGateway (no Rust, no AWS)"
	@echo "site      the landing page, with hot reload"
	@echo "site-build  the landing page's static export, into site/out"
	@echo "smoke     end-to-end against the deployed API (costs a few cents)"
	@echo "build     release binary plus the static UI bundle"
	@echo "dmg       the installable .app and .dmg, into target/release/bundle"
