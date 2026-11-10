//! Runs the published grader inside OutLayer's TDX enclave through
//! `request_execution` and returns the grader's JSON response.

use near_api::types::{AccountId, NearGas, NearToken};
use near_api::{Contract, NetworkConfig, Signer};
use serde::Serialize;
use serde_json::{json, Value};
use sphinx_protocol::{Code, SphinxError};
use std::sync::Arc;

/// Resource limits for one grader run. See `docs/LIMITS.md`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Limits {
    pub max_instructions: u64,
    pub max_memory_mb: u32,
    pub max_execution_seconds: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_instructions: 2_000_000_000, max_memory_mb: 512, max_execution_seconds: 60 }
    }
}

/// An OutLayer secret the run may read (a hidden suite).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecretsRef {
    pub profile: String,
    pub account_id: String,
}

/// The published grader build to run.
#[derive(Debug, Clone)]
pub struct Grader {
    pub outlayer: AccountId,
    pub wasm_url: String,
    pub wasm_sha256: String,
    pub limits: Limits,
}

impl Grader {
    /// Runs `request` (a `sphinx.grader/1` request) as `caller` and returns the
    /// grader's response object.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` if the call fails, and
    /// `SPHINX_E_RELAY_NO_SCORE` if the enclave returned no response.
    pub async fn run(
        &self,
        network: &NetworkConfig,
        caller: AccountId,
        signer: Arc<Signer>,
        request: &Value,
        secrets: Option<&SecretsRef>,
    ) -> Result<Value, SphinxError> {
        let input = request.to_string();
        let cost: String = Contract(self.outlayer.clone())
            .call_function(
                "estimate_execution_cost",
                json!({ "resource_limits": self.limits, "input_len": input.len() }),
            )
            .read_only::<String>()
            .fetch_from(network)
            .await
            .map(|d| d.data)
            .map_err(|e| crate::decode_failure("estimate_execution_cost", &e.to_string()))?;
        // An estimate that does not parse stops the run instead of guessing a deposit.
        let deposit = NearToken::from_yoctonear(cost.parse().map_err(|_| {
            SphinxError::new(
                Code::RelayRpcFailed,
                format!("estimate_execution_cost returned {cost:?}, not a yoctoNEAR amount"),
            )
        })?);
        let args = json!({
            "source": { "WasmUrl": { "url": self.wasm_url, "hash": self.wasm_sha256, "build_target": "wasm32-wasip2" } },
            "resource_limits": self.limits,
            "input_data": input,
            "secrets_ref": secrets,
            "response_format": "Json",
            "payer_account_id": Value::Null,
            "params": Value::Null,
        });
        let result = Contract(self.outlayer.clone())
            .call_function("request_execution", args)
            .transaction()
            .deposit(deposit)
            // A run burns about 8 Tgas, but the sender must hold all prepaid
            // gas at a pessimistic price first (0.1 NEAR per 100 Tgas on
            // mainnet). OutLayer needs room for its callback: 50 is too little.
            .gas(NearGas::from_tgas(100))
            .with_signer(caller, signer)
            .send_to(network)
            .await
            .map_err(|e| crate::decode_failure("request_execution", &e.to_string()))?;
        let outcome = crate::outcome_value("request_execution", result)?;
        match outcome.value {
            Some(v) if v.is_object() => Ok(v),
            _ => {
                Err(SphinxError::new(Code::RelayNoScore, "the enclave returned no grader response")
                    .ctx("tx_hash", outcome.tx_hash))
            }
        }
    }
}
