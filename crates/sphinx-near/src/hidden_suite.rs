//! Storing a buyer's hidden suite as an encrypted OutLayer secret.
//!
//! The secret is encrypted on the buyer's machine to a key OutLayer's keystore
//! derives inside its enclave for one grader build, and stored on-chain by the
//! buyer. Its access rule admits only runs of that exact build submitted by
//! the Sphinx relayer, so neither the relayer nor anyone else sees plaintext;
//! inside the run each JSON key becomes an environment variable (`SUITE`,
//! `REFERENCE`). The keystore is only told the secret's key names.
//!
//! The secret also names the contract and job it is for (`CONTRACT`,
//! `JOB_ID`); the enclave grader refuses to use it for anything else.
//!
//! Wire format of the ciphertext (`outlayer-keystore-v1`, [`sphinx_box`]):
//! `base64( 0x01 | ephemeral X25519 public key (32) | nonce (12) | ciphertext | tag (16) )`,
//! key = HKDF-SHA256(X25519(ephemeral, keystore), info = "outlayer-keystore-v1").
//! Submissions are encrypted to the grader's box key with the same construction
//! ([`encrypt_submission`]).

use near_api::types::{AccountId, NearGas, NearToken};
use near_api::{Contract, NetworkConfig, Signer};
use serde_json::{json, Value};
use sphinx_protocol::{hidden, Code, SphinxError};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{outcome_value, CallOutcome};

/// Where OutLayer lives on one network.
#[derive(Debug, Clone)]
pub struct Keystore {
    /// Coordinator API, e.g. `https://testnet-api.outlayer.ai`.
    pub api: String,
    /// `OutLayer` contract, e.g. `outlayer.testnet`.
    pub contract: AccountId,
}

/// Which run may read a job's hidden suite: the grader build and the account
/// whose enclave runs hold the grader key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteTarget {
    pub wasm_sha256: String,
    pub caller: AccountId,
    pub attestation_ref: String,
    /// The key submissions to this build are encrypted to.
    pub box_public_key: String,
}

/// Reads a grader key's attestation and returns which run may read a suite stored for it. Pass
/// the key frozen into a job (`job.commit.grader`) so the suite is readable by
/// the build that will grade that job. The attestation must report the same
/// build as the key.
///
/// # Errors
/// `SPHINX_E_CLI_GRADER_UNVERIFIED` if the key is not an enclave key, has no
/// box key (its submissions could not reach the enclave unpublished), or its
/// attestation cannot be read or does not match the key's build.
pub async fn grader_target(grader: &crate::views::GraderKey) -> Result<SuiteTarget, SphinxError> {
    let unverified = |why: &str| SphinxError::new(Code::CliGraderUnverified, why.to_owned());
    if grader.mode != "outlayer_tdx" {
        return Err(unverified("the grader key is not an enclave key").ctx("mode", &grader.mode));
    }
    if sphinx_box::parse_public_key(&grader.box_public_key).is_err() {
        return Err(unverified("the grader key has no box key for encrypting submissions"));
    }
    let wasm = grader.wasm_sha256.to_string();
    if !grader.attestation_ref.starts_with("https://") {
        return Err(unverified("the grader's attestation is not an https URL")
            .ctx("attestation_ref", &grader.attestation_ref));
    }
    let attestation: Value = reqwest::get(&grader.attestation_ref)
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| unverified(&format!("cannot read the attestation: {e}")))?
        .json()
        .await
        .map_err(|e| unverified(&format!("the attestation is not JSON: {e}")))?;
    // The build inside the task hash the quote covers; a separate
    // `executed_wasm_sha256`, if present, must agree with it.
    let executed = attestation["wasm_hash"].as_str().unwrap_or_default();
    let reported = attestation["executed_wasm_sha256"].as_str().unwrap_or(executed);
    if executed != wasm || reported != executed {
        return Err(unverified("the attestation reports a different grader build")
            .ctx("key_wasm_sha256", &wasm)
            .ctx("attested_wasm_sha256", executed));
    }
    let caller = attestation["caller_account_id"]
        .as_str()
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| unverified("the attestation names no caller account"))?;
    Ok(SuiteTarget {
        wasm_sha256: wasm,
        caller,
        attestation_ref: grader.attestation_ref.clone(),
        box_public_key: grader.box_public_key.clone(),
    })
}

/// The values of a job's hidden-suite secret: the suite and reference, bound
/// to the escrow contract and job they are for.
pub fn secret_values(
    contract: &str,
    job_id: u64,
    suite: String,
    reference: String,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (hidden::SUITE.to_owned(), suite),
        (hidden::REFERENCE.to_owned(), reference),
        (hidden::CONTRACT.to_owned(), contract.to_owned()),
        (hidden::JOB_ID.to_owned(), job_id.to_string()),
    ])
}

/// Encrypts submission code to a grader's box key (`job.commit.grader.box_public_key`),
/// for the `submission_box` field of a grade request.
///
/// # Errors
/// `SPHINX_E_RELAY_NO_BOX_KEY` if the key is missing or not a usable 32-byte hex key.
pub fn encrypt_submission(box_public_key: &str, code: &[u8]) -> Result<String, SphinxError> {
    sphinx_box::encrypt(box_public_key, code, sphinx_box::SUBMISSION_INFO).map_err(|e| {
        SphinxError::new(Code::RelayNoBoxKey, format!("cannot encrypt the submission: {e}"))
    })
}

/// The profile name used for a job's hidden suite.
pub fn profile_for_job(job_id: u64) -> String {
    format!("sphinx-job-{job_id}")
}

/// Access rule: only `relayer` (as transaction signer and direct caller)
/// running `wasm_sha256` may read the secret.
pub fn access_rule(wasm_sha256: &str, relayer: &AccountId) -> Value {
    json!({ "Logic": { "operator": "And", "conditions": [
        { "Whitelist": { "accounts": [relayer] } },
        { "WasmHash": { "hash": wasm_sha256 } },
        { "Predecessor": { "condition": { "Whitelist": { "accounts": [relayer] } } } },
    ]}})
}

/// Encrypts `plaintext` to the keystore's X25519 public key.
///
/// # Errors
/// `SPHINX_E_API_BAD_REQUEST` if the public key is not 32 hex bytes.
pub fn encrypt(keystore_pubkey_hex: &str, plaintext: &[u8]) -> Result<String, SphinxError> {
    if keystore_pubkey_hex.len() != 64 {
        return Err(bad_keystore_key());
    }
    sphinx_box::encrypt(keystore_pubkey_hex, plaintext, sphinx_box::KEYSTORE_INFO).map_err(|e| {
        match e {
            sphinx_box::BoxError::BadPublicKey => bad_keystore_key(),
            _ => internal("encryption failed"),
        }
    })
}

fn bad_keystore_key() -> SphinxError {
    SphinxError::new(Code::ApiBadRequest, "keystore public key is not 32 hex bytes")
}

impl Keystore {
    /// OutLayer testnet.
    pub fn testnet() -> Self {
        Self {
            api: "https://testnet-api.outlayer.ai".into(),
            contract: "outlayer.testnet".parse().unwrap_or_else(|_| unreachable!()),
        }
    }

    /// OutLayer mainnet.
    pub fn mainnet() -> Self {
        Self {
            api: "https://api.outlayer.ai".into(),
            contract: "outlayer.near".parse().unwrap_or_else(|_| unreachable!()),
        }
    }

    /// OutLayer on the network `account` lives on (see [`crate::is_mainnet`]).
    pub fn for_account(account: &str) -> Self {
        if crate::is_mainnet(account) {
            Self::mainnet()
        } else {
            Self::testnet()
        }
    }

    /// Asks the keystore for the public key of (build, owner). Only the key
    /// names are sent, never the values.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` if the coordinator does not answer with a key.
    pub async fn public_key(
        &self,
        wasm_sha256: &str,
        owner: &AccountId,
        profile: &str,
        names: &[&str],
    ) -> Result<String, SphinxError> {
        let names_only: BTreeMap<&str, &str> = names.iter().map(|n| (*n, "")).collect();
        let body = json!({
            "accessor": { "type": "WasmHash", "hash": wasm_sha256 },
            "owner": owner, "profile": profile,
            "secrets_json": serde_json::to_string(&names_only).unwrap_or_default(),
        });
        let resp: Value = reqwest::Client::new()
            .post(format!("{}/secrets/pubkey", self.api.trim_end_matches('/')))
            .json(&body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| rpc("secrets/pubkey", &e))?
            .json()
            .await
            .map_err(|e| rpc("secrets/pubkey", &e))?;
        resp["pubkey"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| rpc("secrets/pubkey", &"response has no pubkey"))
    }

    /// The `store_secrets` arguments and deposit for `values`, encrypted for
    /// `buyer`'s keystore entry: everything a wallet needs to store a job's
    /// hidden suite without ever seeing it in the clear.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` on network failures.
    pub async fn prepare_store(
        &self,
        network: &NetworkConfig,
        buyer: &AccountId,
        profile: &str,
        wasm_sha256: &str,
        relayer: &AccountId,
        values: &BTreeMap<String, String>,
    ) -> Result<(Value, NearToken), SphinxError> {
        let names: Vec<&str> = values.keys().map(String::as_str).collect();
        let pubkey = self.public_key(wasm_sha256, buyer, profile, &names).await?;
        let plaintext = serde_json::to_vec(values).map_err(|e| internal(&e.to_string()))?;
        let encrypted = encrypt(&pubkey, &plaintext)?;
        let accessor = json!({ "WasmHash": { "hash": wasm_sha256 } });
        let access = access_rule(wasm_sha256, relayer);
        let cost: String = Contract(self.contract.clone())
            .call_function("estimate_storage_cost", json!({
                "accessor": accessor, "profile": profile, "owner": buyer,
                "encrypted_secrets_base64": encrypted, "access": access, "vault_id": Value::Null,
            }))
            .read_only::<String>()
            .fetch_from(network)
            .await
            .map(|d| d.data)
            .map_err(|e| rpc("estimate_storage_cost", &e))?;
        let deposit = NearToken::from_yoctonear(
            cost.parse().map_err(|_| rpc("estimate_storage_cost", &"not a number"))?,
        );
        let args = json!({
            "accessor": accessor, "profile": profile, "encrypted_secrets_base64": encrypted,
            "access": access, "vault_id": Value::Null,
        });
        Ok((args, deposit))
    }

    /// Encrypts `values` (e.g. `SUITE`, `REFERENCE`) for one job and stores them,
    /// signed and paid for by the buyer. The deposit is refunded by [`Keystore::delete`].
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` on network failures, or the contract's error.
    #[allow(clippy::too_many_arguments, reason = "one argument per independent input")]
    pub async fn store(
        &self,
        network: &NetworkConfig,
        buyer: AccountId,
        signer: Arc<Signer>,
        profile: &str,
        wasm_sha256: &str,
        relayer: &AccountId,
        values: &BTreeMap<String, String>,
    ) -> Result<CallOutcome, SphinxError> {
        let (args, deposit) =
            self.prepare_store(network, &buyer, profile, wasm_sha256, relayer, values).await?;
        let result = Contract(self.contract.clone())
            .call_function("store_secrets", args)
            .transaction()
            .deposit(deposit)
            .gas(NearGas::from_tgas(100))
            .with_signer(buyer, signer)
            .send_to(network)
            .await
            .map_err(|e| crate::decode_failure("store_secrets", &e.to_string()))?;
        outcome_value("store_secrets", result)
    }

    /// Whether `owner` has a secret stored under `profile` for grader build
    /// `wasm_sha256`.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` on network failures.
    pub async fn is_stored(
        &self,
        network: &NetworkConfig,
        owner: &AccountId,
        profile: &str,
        wasm_sha256: &str,
    ) -> Result<bool, SphinxError> {
        let args = json!({
            "accessor": { "WasmHash": { "hash": wasm_sha256 } },
            "profile": profile,
            "owner": owner,
        });
        let row: Value = Contract(self.contract.clone())
            .call_function("get_secrets", args)
            .read_only::<Value>()
            .fetch_from(network)
            .await
            .map(|d| d.data)
            .map_err(|e| crate::decode_failure("get_secrets", &e.to_string()))?;
        Ok(!row.is_null())
    }

    /// Deletes a job's stored secret and refunds its storage deposit.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` on network failures, or the contract's error.
    pub async fn delete(
        &self,
        network: &NetworkConfig,
        buyer: AccountId,
        signer: Arc<Signer>,
        profile: &str,
        wasm_sha256: &str,
    ) -> Result<CallOutcome, SphinxError> {
        let result = Contract(self.contract.clone())
            .call_function(
                "delete_secrets",
                json!({ "accessor": { "WasmHash": { "hash": wasm_sha256 } }, "profile": profile }),
            )
            .transaction()
            .gas(NearGas::from_tgas(50))
            .with_signer(buyer, signer)
            .send_to(network)
            .await
            .map_err(|e| crate::decode_failure("delete_secrets", &e.to_string()))?;
        outcome_value("delete_secrets", result)
    }
}

fn internal(what: &str) -> SphinxError {
    SphinxError::new(Code::InternalInvariant, format!("storing the hidden suite failed: {what}"))
}

fn rpc(what: &str, e: &dyn std::fmt::Display) -> SphinxError {
    SphinxError::new(Code::RelayRpcFailed, format!("{what} failed: {e}")).ctx("method", what)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sphinx_box::BoxSecret;

    #[test]
    fn ciphertext_opens_with_the_keystore_key() {
        let sk = BoxSecret::derive(&[7u8; 32]);
        let msg = br#"{"SUITE":"{\"tests\":[]}"}"#;
        let b64 = encrypt(&sk.public_key_hex(), msg).unwrap();
        assert_eq!(sk.open(&b64, sphinx_box::KEYSTORE_INFO).unwrap(), msg);
        assert_ne!(
            encrypt(&sk.public_key_hex(), msg).unwrap(),
            b64,
            "fresh ephemeral key and nonce each time"
        );
    }

    #[test]
    fn an_encrypted_submission_opens_only_as_a_submission() {
        let sk = BoxSecret::derive(&[9u8; 32]);
        let b64 = encrypt_submission(&sk.public_key_hex(), b"function solve(){}").unwrap();
        assert_eq!(sk.open(&b64, sphinx_box::SUBMISSION_INFO).unwrap(), b"function solve(){}");
        assert!(sk.open(&b64, sphinx_box::KEYSTORE_INFO).is_err());
        assert_eq!(encrypt_submission("", b"x").unwrap_err().code(), Code::RelayNoBoxKey);
    }

    #[test]
    fn the_secret_names_its_job() {
        let v = secret_values("escrow.testnet", 7, "s".into(), "r".into());
        let keys: Vec<&str> = v.keys().map(String::as_str).collect();
        assert_eq!(keys, ["CONTRACT", "JOB_ID", "REFERENCE", "SUITE"]);
        assert_eq!(v["JOB_ID"], "7");
        assert_eq!(v["CONTRACT"], "escrow.testnet");
    }

    #[test]
    fn access_rule_names_build_and_relayer() {
        let rule = access_rule("ab", &"relayer.testnet".parse().unwrap());
        let text = rule.to_string();
        assert!(text.contains("\"WasmHash\":{\"hash\":\"ab\"}") && text.contains("Predecessor"));
        assert_eq!(profile_for_job(7), "sphinx-job-7");
    }

    #[test]
    fn rejects_bad_keys() {
        assert_eq!(encrypt("zz", b"x").unwrap_err().code(), Code::ApiBadRequest);
        assert_eq!(encrypt(&"zz".repeat(32), b"x").unwrap_err().code(), Code::ApiBadRequest);
    }

    #[tokio::test]
    async fn a_grader_key_without_a_box_key_is_not_a_target() {
        let key = crate::views::GraderKey {
            public_key: "11".repeat(32),
            box_public_key: String::new(),
            mode: "outlayer_tdx".into(),
            wasm_sha256: sphinx_protocol::Hash32::digest(b"w"),
            attestation_ref: "https://example.invalid/a".into(),
            registered_at_ns: "0".into(),
        };
        let e = grader_target(&key).await.unwrap_err();
        assert_eq!(e.code(), Code::CliGraderUnverified);
    }
}
