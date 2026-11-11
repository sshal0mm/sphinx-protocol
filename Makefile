# Sphinx developer entry points. Run `make help`.
SHELL := /bin/bash
.DEFAULT_GOAL := help


.PHONY: release-grader attacks attacks-check house sandbox gas doctor quick
.PHONY: help setup build test check fmt lint docs grader-wasm contract-wasm test-wasm demo-local demo demo-verify demo-verify-online tamper record-run api relayer agent concierge mcp deploy register-dev-key

help: ## Show targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-18s %s\n", $$1, $$2}'

setup: ## Install toolchain targets and helper tools
	rustup show active-toolchain
	cargo install --locked cargo-near wasmtime-cli

build: grader-wasm contract-wasm ## Build every artifact
	cargo build --release -p sphinx-cli

test: ## Run all unit and integration tests
	cargo test --workspace --all-features
	cargo test -p sphinx-contract

check: fmt lint test test-wasm ## Everything CI runs
	cargo run -q -p sphinx-protocol --bin gen-error-docs | diff -u docs/ERRORS.md -

fmt: ## Check formatting
	cargo fmt --all -- --check

lint: ## Clippy with warnings as errors, every build variant
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo clippy -p sphinx-contract --all-targets -- -D warnings
	cargo clippy -p sphinx-contract --target wasm32-unknown-unknown -- -D warnings
	cargo clippy -p sphinx-grader --target wasm32-wasip2 --features outlayer -- -D warnings

docs: ## Regenerate docs/ERRORS.md from the error catalog
	cargo run -q -p sphinx-protocol --bin gen-error-docs > docs/ERRORS.md

release-grader: ## Build, upload, attest and register a new enclave grader build (testnet; owner key in the keychain)
	scripts/release-grader.sh

grader-wasm: ## Build the enclave grader (wasm32-wasip2, OutLayer signer, path-free)
	scripts/build-grader.sh

contract-wasm: ## Build the NEAR contract
	cd crates/sphinx-contract && cargo near build non-reproducible-wasm

test-wasm: ## Run the hostile corpus under the enclave's instruction allowance
	cargo build -q --release -p sphinx-grader --target wasm32-wasip2
	scripts/test-wasm.sh

demo-local: ## The grading story offline: no network, no keys
	scripts/demo-local.sh

demo-verify: ## Check every recorded job in deployments/runs offline: registration, signed results, payout (no keys, no network)
	cargo run -q -p sphinx-cli -- verify deployments/runs/*.json

record-replay: ## Record one paid job step by step from the chain for the /replay page: make record-replay RUN=mainnet2-job0
	python3 scripts/record-replay.py deployments/runs/$(RUN).json > deployments/replays/$(RUN).json

runs-doc: ## Regenerate docs/RUNS.md (every recorded submission, its result, seconds to settle and its transaction) from deployments/runs
	scripts/runs-doc.sh

demo-verify-online: ## The same checks, plus re-reading the job, submissions, key register and OutLayer's approved list from chain
	cargo run -q -p sphinx-cli -- verify --online deployments/runs/*.json

doctor: ## Say what this machine can run and what is missing
	scripts/doctor.sh

quick: ## Everything that needs no keys or accounts: grading offline, every recorded job verified, tampering caught
	$(MAKE) demo-local demo-verify tamper

sandbox: ## The compiled contract on a local NEAR sandbox node at the 64-submission cap (downloads the node once)
	$(MAKE) contract-wasm
	cargo test -p sphinx-sandbox -- --ignored --nocapture

gas: ## Measure each escrow method's gas from its testnet transactions into docs/GAS.md
	scripts/gas-table.sh > docs/GAS.md

house: ## Repost any catalog job (data/catalog, outside git) that has no open copy, above a balance floor (needs make api)
	scripts/house.sh

attacks: ## Run real attacks against the testnet escrow and job API; records them and writes docs/ATTACKS.md (needs make api)
	scripts/attacks.sh

attacks-check: ## Re-check the recorded attacks from the network, no keys
	scripts/attacks-check.sh

tamper: ## Show that changing a fact the signatures or the quote cover makes verification fail (who was paid and how much: verify --online)
	cargo test -p sphinx-cli --bin sphinx verify::tests -- --nocapture 2>&1 | grep -E "^test |test result"

record-run: ## Save a job's evidence bundle for offline checks: make record-run JOB=1
	scripts/record-run.sh $(JOB)

demo: ## The full loop on NEAR testnet (needs .env and keychain keys)
	scripts/e2e-testnet.sh

api: ## Run the job API (gasless relay when the relayer key is available)
	scripts/serve.sh api

relayer: ## Run the relayer: grade jobs in the enclave, post signed results (ARGS=--once for one pass)
	scripts/serve.sh relayer $(ARGS)

agent: ## Run the earning agent (NEAR AI Cloud key in the keychain; ARGS=--once for one job)
	scripts/agent.sh $(ARGS)

concierge: ## Draft, check and post a job from a plain request: make concierge ARGS='"what you want" --reward 0.5'
	scripts/concierge.sh $(ARGS)

mcp: ## Build the MCP server and print its path for an MCP client config
	cargo build -q --release -p sphinx-mcp && echo "$(CURDIR)/target/release/sphinx-mcp"

deploy: ## Deploy the contract to $SPHINX_CONTRACT
	scripts/deploy.sh

register-dev-key: ## Register the DEV grader key (requires SPHINX_MODE=dev)
	scripts/register-dev-key.sh
