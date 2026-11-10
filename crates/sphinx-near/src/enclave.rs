//! Gathering a grading run's enclave proof for the escrow's `confirm_run`.
//!
//! After OutLayer runs the grader, the relayer collects:
//! - the run's attestation from OutLayer's API, found by the hash of the
//!   `request_execution` transaction;
//! - the run's exact output bytes, the transaction's return value, whose
//!   SHA-256 the attestation names;
//! - Intel's collateral for the quote, from OutLayer's registry contract,
//!   converted to the borsh form NEAR's DCAP verifier takes.
//!
//! Before paying for the on-chain check it asks the verifier, as a free view
//! call, whether the quote verifies with that collateral, and tries OutLayer's
//! copies newest first.

use base64::Engine;
use serde_json::{json, Value};
use sphinx_protocol::{Code, SphinxError};

use crate::near_api::NetworkConfig;

/// NEAR's DCAP verifier contract and OutLayer's registry, per network.
#[derive(Debug, Clone)]
pub struct EnclaveAccounts {
    pub quote_verifier: String,
    pub registry: String,
    /// OutLayer's API, which serves attestations by transaction.
    pub api: String,
}

impl EnclaveAccounts {
    #[must_use]
    pub fn mainnet() -> Self {
        Self {
            quote_verifier: "tee-verifier-2026-08-04.near".into(),
            registry: "worker.outlayer.near".into(),
            api: "https://api.outlayer.ai".into(),
        }
    }

    #[must_use]
    pub fn testnet() -> Self {
        Self {
            quote_verifier: "tee-verifier-2026-11-02.testnet".into(),
            registry: "worker.outlayer.testnet".into(),
            api: "https://testnet-api.outlayer.ai".into(),
        }
    }
}

/// Everything `confirm_run` takes, ready to send.
#[derive(Debug, Clone)]
pub struct RunProof {
    pub attestation: Value,
    pub output: Vec<u8>,
    pub quote: Vec<u8>,
    pub collateral: Vec<u8>,
}

impl RunProof {
    /// The `confirm_run` arguments for `job_id` (and `sub_id`, none for a
    /// suite check).
    #[must_use]
    pub fn args(&self, job_id: u64, sub_id: Option<u32>) -> Value {
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        json!({
            "job_id": job_id.to_string(),
            "sub_id": sub_id,
            "attestation": self.attestation,
            "output": b64(&self.output),
            "quote": b64(&self.quote),
            "collateral": b64(&self.collateral),
        })
    }
}

fn failed(what: &str, e: &impl std::fmt::Display) -> SphinxError {
    SphinxError::new(Code::RelayRpcFailed, format!("{what}: {e}")).ctx("step", what)
}

fn rpc_url(network: &NetworkConfig) -> String {
    network
        .rpc_endpoints
        .first()
        .map_or_else(|| crate::TESTNET_RPC[0].to_owned(), |e| e.url.to_string())
}

async fn rpc(
    http: &reqwest::Client,
    network: &NetworkConfig,
    method: &str,
    params: Value,
) -> Result<Value, SphinxError> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let v: Value = http
        .post(rpc_url(network))
        .json(&body)
        .send()
        .await
        .map_err(|e| failed(method, &e))?
        .json()
        .await
        .map_err(|e| failed(method, &e))?;
    if let Some(e) = v.get("error") {
        return Err(failed(method, e));
    }
    Ok(v["result"].clone())
}

/// A view call with raw argument bytes; returns the raw result bytes.
async fn view_raw(
    http: &reqwest::Client,
    network: &NetworkConfig,
    account: &str,
    method: &str,
    args: &[u8],
) -> Result<Vec<u8>, SphinxError> {
    let r = rpc(
        http,
        network,
        "query",
        json!({
            "request_type": "call_function", "finality": "final", "account_id": account,
            "method_name": method,
            "args_base64": base64::engine::general_purpose::STANDARD.encode(args),
        }),
    )
    .await?;
    r["result"]
        .as_array()
        .map(|a| a.iter().filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect())
        .ok_or_else(|| failed(method, &"no result bytes"))
}

fn borsh_vec(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(b);
}

/// dcap-qvl JSON collateral (byte fields as hex) to the borsh `Collateral`
/// NEAR's `verify_quote` takes.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` when a field is missing or not hex.
pub fn collateral_borsh(c: &Value) -> Result<Vec<u8>, SphinxError> {
    let text = |k: &str| c[k].as_str().ok_or_else(|| failed("collateral", &format!("no {k}")));
    let hex = |k: &str| hex::decode(text(k)?).map_err(|e| failed("collateral", &e));
    let mut out = Vec::new();
    borsh_vec(&mut out, text("pck_crl_issuer_chain")?.as_bytes());
    borsh_vec(&mut out, &hex("root_ca_crl")?);
    borsh_vec(&mut out, &hex("pck_crl")?);
    borsh_vec(&mut out, text("tcb_info_issuer_chain")?.as_bytes());
    borsh_vec(&mut out, text("tcb_info")?.as_bytes());
    borsh_vec(&mut out, &hex("tcb_info_signature")?);
    borsh_vec(&mut out, text("qe_identity_issuer_chain")?.as_bytes());
    borsh_vec(&mut out, text("qe_identity")?.as_bytes());
    borsh_vec(&mut out, &hex("qe_identity_signature")?);
    match c["pck_certificate_chain"].as_str() {
        Some(p) => {
            out.push(1);
            borsh_vec(&mut out, p.as_bytes());
        }
        None => out.push(0),
    }
    Ok(out)
}

/// Asks the verifier, without a transaction, whether `quote` verifies with
/// `collateral`. Returns whether it did, and the TCB status or the reason.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` if the view call fails.
pub async fn verify_view(
    http: &reqwest::Client,
    network: &NetworkConfig,
    verifier: &str,
    quote: &[u8],
    collateral: &[u8],
) -> Result<(bool, String), SphinxError> {
    let mut args = Vec::new();
    borsh_vec(&mut args, quote);
    args.extend_from_slice(collateral);
    let r = view_raw(http, network, verifier, "verify_quote", &args).await?;
    let read_str = |b: &[u8]| -> String {
        if b.len() < 4 {
            return String::new();
        }
        let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
        String::from_utf8_lossy(&b[4..(4 + n).min(b.len())]).into_owned()
    };
    match r.first() {
        Some(0) => Ok((true, read_str(&r[1..]))),
        Some(1) => Ok((false, read_str(&r[2..]))),
        _ => Err(failed("verify_quote", &"unexpected result")),
    }
}

/// The run's attestation, by the hash of its `request_execution`
/// transaction. OutLayer publishes it a few seconds after the run.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` if it does not appear within about a minute.
pub async fn attestation(
    http: &reqwest::Client,
    api: &str,
    tx_hash: &str,
) -> Result<Value, SphinxError> {
    let url = format!("{}/attestations/by-tx/{tx_hash}", api.trim_end_matches('/'));
    for _ in 0..20 {
        if let Ok(res) = http.get(&url).send().await {
            if let Ok(v) = res.json::<Value>().await {
                if v["output_hash"].is_string() && v["tdx_quote"].is_string() {
                    return Ok(v);
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    Err(failed("attestation", &format!("not published at {url}")))
}

/// The exact bytes the run returned: the transaction's return value.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` if the transaction cannot be read.
pub async fn run_output(
    http: &reqwest::Client,
    network: &NetworkConfig,
    tx_hash: &str,
    sender: &str,
) -> Result<Vec<u8>, SphinxError> {
    let r = rpc(
        http,
        network,
        "EXPERIMENTAL_tx_status",
        json!({ "tx_hash": tx_hash, "sender_account_id": sender, "wait_until": "FINAL" }),
    )
    .await?;
    let value = r["status"]["SuccessValue"]
        .as_str()
        .ok_or_else(|| failed("run output", &"the run returned nothing"))?;
    base64::engine::general_purpose::STANDARD.decode(value).map_err(|e| failed("run output", &e))
}

/// Collects the proof of the run sent in `tx_hash` by `caller`, with a
/// collateral NEAR's verifier accepts for its quote.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` if any piece cannot be read, or no collateral
/// verifies the quote.
pub async fn collect(
    network: &NetworkConfig,
    accounts: &EnclaveAccounts,
    tx_hash: &str,
    caller: &str,
) -> Result<RunProof, SphinxError> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| failed("http", &e))?;
    let mut attestation = attestation(&http, &accounts.api, tx_hash).await?;
    let quote = base64::engine::general_purpose::STANDARD
        .decode(attestation["tdx_quote"].as_str().unwrap_or_default())
        .map_err(|e| failed("quote", &e))?;
    if let Some(a) = attestation.as_object_mut() {
        // The contract takes only the hashed fields; the quote travels apart.
        for k in ["tdx_quote", "id", "request_id", "transaction_hash", "worker_measurement"] {
            a.remove(k);
        }
    }
    let output = run_output(&http, network, tx_hash, caller).await?;
    let raw = view_raw(&http, network, &accounts.registry, "get_collaterals", b"{}").await?;
    let list: Value = serde_json::from_slice(&raw).map_err(|e| failed("collaterals", &e))?;
    let mut tried = Vec::new();
    for c in list.as_array().into_iter().flatten().rev() {
        let c: Value = match c.as_str() {
            Some(s) => serde_json::from_str(s).map_err(|e| failed("collateral", &e))?,
            None => c.clone(),
        };
        let collateral = collateral_borsh(&c)?;
        let (ok, status) =
            verify_view(&http, network, &accounts.quote_verifier, &quote, &collateral).await?;
        if ok {
            return Ok(RunProof { attestation, output, quote, collateral });
        }
        tried.push(status);
    }
    Err(failed(
        "collateral",
        &format!("no collateral OutLayer holds verifies this quote ({})", tried.join("; ")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gathers the proof of mainnet job 2's real grading run and has NEAR's
    /// mainnet verifier check it, as a free view call. Network only.
    #[tokio::test]
    #[ignore = "reads mainnet and OutLayer's API"]
    async fn a_real_mainnet_run_verifies() {
        let network = crate::mainnet_network(None, None);
        let proof = collect(
            &network,
            &EnclaveAccounts::mainnet(),
            "2xC49RzTSNHHXKPvp3W2QfMD7RtrRW5rY67W9tTQ2Pfr",
            "relayer.sphinxescrow.near",
        )
        .await
        .unwrap();
        assert_eq!(proof.attestation["secrets_ref"], "buyer.sphinxescrow.near/sphinx-job-2");
        assert!(String::from_utf8_lossy(&proof.output)
            .contains("sphinx:v1:score:escrow.sphinxescrow.near:2:0"));
    }
}
