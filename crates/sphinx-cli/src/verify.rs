//! `sphinx verify`: checks a recorded job from its evidence bundle alone
//! (`deployments/runs/*.json`, written by `scripts/record-run.sh`).
//!
//! What it proves, offline and without trusting the operator:
//! - the grader registration: the enclave run's exact output hashes to the
//!   attested output hash, the attested caller is the relayer, and that
//!   output names the key frozen into the job, which the contract's grader
//!   register lists for the attested build (keys rotate with new builds; a
//!   job keeps the one it was committed with);
//! - every signed result: its fields match the job and submission it is for,
//!   and its ed25519 signature verifies under the job's frozen grader key;
//! - the payout: the job was paid to the earliest passing submission.
//!
//! - the hardware: the TDX quote carries Intel's signature (checked against
//!   Intel's collateral saved in the bundle), the TD is not in debug mode,
//!   and the quote's report data is OutLayer's task hash over this exact run
//!   (build, input, output, caller), so the quote vouches for the output that
//!   names the keys; and its five measurements (MRTD, RTMR0-3) are an entry
//!   of the list OutLayer's worker contract approves, read at a recorded block.
//!
//! What it does not prove: that a later score was produced inside the
//! enclave rather than by anyone else holding the key. OutLayer's keystore
//! derives the key only for this build and caller; the quote shows that the
//! registration ran there, and every score is checked against that key.

use std::fmt::Write as _;

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::Value;
use sphinx_protocol::{Hash32, Score, SuiteCheck};

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Skip,
}

/// Every check, in order, with a one-line description.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<(Status, String)>,
}

impl Report {
    fn push(&mut self, ok: bool, what: impl Into<String>) {
        self.lines.push((if ok { Status::Ok } else { Status::Fail }, what.into()));
    }

    fn skip(&mut self, what: impl Into<String>) {
        self.lines.push((Status::Skip, what.into()));
    }

    pub fn failed(&self) -> usize {
        self.lines.iter().filter(|(s, _)| *s == Status::Fail).count()
    }

    /// Exit code for `failed` failed checks: 0 verified, 1 a check failed.
    pub const fn exit_code(failed: usize) -> u8 {
        if failed > 0 {
            1
        } else {
            0
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (status, what) in &self.lines {
            let tag = match status {
                Status::Ok => "ok  ",
                Status::Fail => "FAIL",
                Status::Skip => "skip",
            };
            let _ = writeln!(out, "{tag}  {what}");
        }
        let failed = self.failed();
        let checked = self.lines.iter().filter(|(s, _)| *s == Status::Ok).count();
        let _ = writeln!(
            out,
            "{}: {checked} checks passed, {failed} failed, {} not checked here",
            if failed == 0 { "VERIFIED" } else { "NOT VERIFIED" },
            self.lines.iter().filter(|(s, _)| *s == Status::Skip).count()
        );
        out
    }
}

fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut cur = v;
    for key in path {
        cur = &cur[*key];
    }
    cur.as_str().unwrap_or("")
}

fn verifies(public_key_hex: &str, message: &str, signature_hex: &str) -> bool {
    let key = hex::decode(public_key_hex)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .and_then(|b| VerifyingKey::from_bytes(&b).ok());
    let sig = hex::decode(signature_hex)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
        .map(|b| Signature::from_bytes(&b));
    match (key, sig) {
        (Some(k), Some(sig)) => k.verify(message.as_bytes(), &sig).is_ok(),
        _ => false,
    }
}

/// Checks a bundle. Never panics on malformed input; anything missing fails.
///
/// Offline, the anchors are Intel's signature over the quote and the
/// signatures of the grader key; the contract views (job, submissions, key
/// register) and the approved list are as recorded. `online_checks` re-reads
/// those from chain.
pub fn verify_bundle(bundle: &Value) -> Report {
    let mut r = Report::default();
    r.push(bundle["schema"] == "sphinx.run/1", "bundle format sphinx.run/1");
    let contract = s(bundle, &["contract"]);
    let job = &bundle["job"];
    let frozen = &job["commit"]["grader"];
    check_attestation_shape(&mut r, &bundle["grader"]["attestation"]);
    check_registration(&mut r, bundle, frozen);
    let passes = check_results(&mut r, bundle, contract, job, frozen);
    check_payout(&mut r, bundle, job, &passes);
    check_quote(&mut r, &bundle["grader"], contract);
    r
}

fn is_hex64(v: &Value) -> bool {
    v.as_str().is_some_and(|h| {
        h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// The attestation must have exactly the shape of a genuine OutLayer run:
/// fixed-length hashes, integer counters and none of the optional text
/// fields. `task_hash` joins fields without separators, so only a fixed shape
/// makes the hashed bytes mean one thing.
fn check_attestation_shape(r: &mut Report, a: &Value) {
    let account = a["caller_account_id"].as_str().is_some_and(|c| {
        (2..=64).contains(&c.len())
            && c.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    });
    let shape = a["task_type"] == "execute"
        && a["build_target"] == "wasm32-wasip2"
        && ["task_id", "block_height", "timestamp"].iter().all(|k| a[*k].is_u64())
        && ["wasm_hash", "input_hash", "output_hash"].iter().all(|k| is_hex64(&a[*k]))
        && account
        && a["attached_usd"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        && ["repo_url", "commit_hash", "project_id", "secrets_ref"].iter().all(|k| a[*k].is_null())
        && a["executed_wasm_sha256"].as_str().is_none_or(|e| Some(e) == a["wasm_hash"].as_str());
    r.push(
        shape,
        "the attestation has the exact shape of an OutLayer run, so its task hash means one thing",
    );
}

/// OutLayer's task hash: SHA-256 over the run's fields, in this order, with
/// integers as 8-byte little-endian and absent fields skipped. The enclave
/// puts it in the first 32 bytes of the quote's report data.
pub fn task_hash(a: &Value) -> Hash32 {
    let mut bytes = Vec::new();
    let text = |bytes: &mut Vec<u8>, k: &str| {
        if let Some(v) = a[k].as_str().filter(|v| !v.is_empty()) {
            bytes.extend_from_slice(v.as_bytes());
        }
    };
    let int = |bytes: &mut Vec<u8>, k: &str| {
        if let Some(v) = a[k].as_i64() {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    };
    text(&mut bytes, "task_type");
    int(&mut bytes, "task_id");
    for k in ["repo_url", "commit_hash", "build_target", "wasm_hash", "input_hash", "output_hash"] {
        text(&mut bytes, k);
    }
    int(&mut bytes, "block_height");
    for k in ["caller_account_id", "project_id", "secrets_ref"] {
        text(&mut bytes, k);
    }
    int(&mut bytes, "timestamp");
    match &a["attached_usd"] {
        Value::String(v) => bytes.extend_from_slice(v.as_bytes()),
        Value::Number(n) => bytes.extend_from_slice(n.to_string().as_bytes()),
        _ => {}
    }
    Hash32::digest(&bytes)
}

fn check_quote(r: &mut Report, grader: &Value, contract: &str) {
    let attestation = &grader["attestation"];
    let quote = base64::engine::general_purpose::STANDARD
        .decode(s(attestation, &["tdx_quote"]))
        .unwrap_or_default();
    let collateral: Option<dcap_qvl::QuoteCollateralV3> =
        serde_json::from_value(grader["collateral"].clone()).ok();
    let (Some(collateral), false) = (collateral, quote.is_empty()) else {
        r.push(false, "Intel signature over the TDX quote (no quote or collateral in the bundle; run `sphinx verify --fetch-collateral`)");
        return;
    };
    let at = grader["collateral_fetched_at"].as_u64().unwrap_or(0);
    let verified = match dcap_qvl::verify::verify(&quote, &collateral, at) {
        Ok(v) => v,
        Err(e) => {
            r.push(false, format!("Intel signature over the TDX quote ({e})"));
            return;
        }
    };
    let tcb_ok = matches!(verified.status.as_str(), "UpToDate" | "SWHardeningNeeded");
    r.push(true, format!("the TDX quote is signed by Intel (collateral as of unix time {at})"));
    let advisories = if verified.advisory_ids.is_empty() {
        String::new()
    } else {
        format!(", advisories {}", verified.advisory_ids.join(" "))
    };
    r.push(tcb_ok, format!("platform TCB status {}{advisories}", verified.status));
    let Some(td) = verified.report.as_td10() else {
        r.push(false, "the quote is a TDX report");
        return;
    };
    r.push(td.td_attributes[0] & 1 == 0, "the TD is not in debug mode");
    r.push(
        Hash32::from_bytes(td.report_data[..32].try_into().unwrap_or([0; 32]))
            == task_hash(attestation),
        "the quote's report data is the task hash of this exact run (build, input, output, caller)",
    );
    r.push(
        hex::encode(td.rt_mr3) == s(attestation, &["worker_measurement"]),
        format!("RTMR3 is OutLayer's worker measurement {}", short(&hex::encode(td.rt_mr3))),
    );
    let measured = [
        ("mrtd", hex::encode(td.mr_td)),
        ("rtmr0", hex::encode(td.rt_mr0)),
        ("rtmr1", hex::encode(td.rt_mr1)),
        ("rtmr2", hex::encode(td.rt_mr2)),
        ("rtmr3", hex::encode(td.rt_mr3)),
    ];
    check_approved(r, &grader["approved_measurements"], &measured, contract);
}

/// The quote's five measurements (the TD image and its boot chain) must be
/// one entry of the list OutLayer's worker contract approves on chain, read
/// at a recorded block, so the enclave ran OutLayer's published worker image
/// and not merely some TDX machine.
fn check_approved(r: &mut Report, approved: &Value, measured: &[(&str, String)], contract: &str) {
    let Some(list) = approved["list"].as_array() else {
        r.push(false, "the measurements are on OutLayer's approved list (no list in the bundle; run `sphinx verify --fetch-collateral`)");
        return;
    };
    let hit = list.iter().position(|entry| measured.iter().all(|(k, v)| s(entry, &[k]) == v));
    let source = s(approved, &["account"]);
    let block = approved["block_height"].as_u64().unwrap_or(0);
    r.push(
        hit.is_some() && source == worker_account(contract) && block > 0,
        format!(
            "MRTD and RTMR0-3 match entry {} of {} in OutLayer's approved list as recorded from {source} at block {block} (`--online` re-reads it)",
            hit.map_or_else(|| "none".to_owned(), |i| (i + 1).to_string()),
            list.len()
        ),
    );
}

/// A NEAR account on mainnet: its top-level name is `near`.
fn is_mainnet(account: &str) -> bool {
    account.rsplit('.').next() == Some("near")
}

/// The OutLayer worker contract that lists approved measurements, for the
/// network the job ran on. `OUTLAYER_WORKER` overrides it.
fn worker_account(contract: &str) -> String {
    std::env::var("OUTLAYER_WORKER").unwrap_or_else(|_| {
        if is_mainnet(contract) { "worker.outlayer.near" } else { "worker.outlayer.testnet" }
            .to_owned()
    })
}

/// The RPC for `account`'s network, or `NEAR_RPC_URL`; with `archival`, an
/// archival node (`NEAR_ARCHIVAL_RPC_URL`) that can answer at old blocks.
fn rpc_for(account: &str, archival: bool) -> String {
    let (var, net) =
        if archival { ("NEAR_ARCHIVAL_RPC_URL", "archival-rpc") } else { ("NEAR_RPC_URL", "rpc") };
    std::env::var(var).unwrap_or_else(|_| {
        format!(
            "https://{net}.{}.fastnear.com",
            if is_mainnet(account) { "mainnet" } else { "testnet" }
        )
    })
}

/// A read-only view call, at `block` (a block hash) or at the final block.
/// Returns the decoded JSON result and the block it was read at.
/// `SPHINX_RPC_KEY` is sent only to the `FastNEAR` RPC hosts (`sphinx_near::is_fastnear`).
async fn view_call(
    rpc: &str,
    account: &str,
    method: &str,
    args: &Value,
    block: Option<&str>,
) -> Result<(Value, Value), String> {
    let mut params = serde_json::json!({
        "request_type": "call_function", "account_id": account, "method_name": method,
        "args_base64": base64::engine::general_purpose::STANDARD.encode(args.to_string()),
    });
    match block {
        Some(hash) => params["block_id"] = hash.into(),
        None => params["finality"] = "final".into(),
    }
    let body =
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "query", "params": params });
    let mut req = reqwest::Client::new().post(rpc).json(&body);
    if let Ok(key) = std::env::var("SPHINX_RPC_KEY") {
        if sphinx_near::is_fastnear(rpc) && !key.is_empty() {
            req = req.bearer_auth(key);
        }
    }
    let reply: Value = req
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("{method} on {account} via {rpc}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("{method} on {account}: {e}"))?;
    let result = &reply["result"];
    let bytes: Vec<u8> = serde_json::from_value(result["result"].clone())
        .map_err(|_| format!("{method} on {account} returned no result: {reply}"))?;
    let value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok((
        value,
        serde_json::json!({ "height": result["block_height"], "hash": result["block_hash"] }),
    ))
}

/// Reads the approved measurement list from the worker contract, with the
/// block it was read at.
async fn fetch_approved(contract: &str) -> Result<Value, String> {
    let account = worker_account(contract);
    let rpc = rpc_for(&account, false);
    let (list, at) =
        view_call(&rpc, &account, "get_approved_measurements", &serde_json::json!({}), None)
            .await?;
    Ok(serde_json::json!({
        "account": account,
        "block_height": at["height"],
        "block_hash": at["hash"],
        "rpc": rpc,
        "list": list,
    }))
}

/// Re-reads from chain what the offline checks take from the bundle: the
/// job, its submissions, the grader key register and the dev-key setting
/// from the contract now, and OutLayer's approved list at the block it was
/// recorded at (archival RPC). Each must match the bundle.
///
/// # Errors
/// Text describing an RPC failure.
pub async fn online_checks(bundle: &Value) -> Result<Report, String> {
    let mut r = Report::default();
    let contract = s(bundle, &["contract"]);
    // A contract closed after the run is read from an archival node at the
    // block its last signed result executed in.
    let (rpc, at) = match closed_at(contract, bundle).await? {
        None => (rpc_for(contract, false), None),
        Some(block) => {
            r.push(
                true,
                format!("{contract} has since been closed or redeployed; this job's state is read from an archival node at block {block}, where its last signed result executed"),
            );
            (rpc_for(contract, true), Some(block))
        }
    };
    let at = at.as_deref();
    let job_id = s(&bundle["job"], &["id"]);
    let (job, _) =
        view_call(&rpc, contract, "get_job", &serde_json::json!({ "job_id": job_id }), at).await?;
    let (subs, _) = view_call(
        &rpc,
        contract,
        "list_submissions",
        &serde_json::json!({ "job_id": job_id, "limit": 50 }),
        at,
    )
    .await?;
    if bundle["job"]["assessment"].is_object() {
        // An assessment stays open, so later attempts change its counts: compare
        // what cannot change, and the recorded attempts.
        let fixed = ["buyer", "reward", "bond", "assessment", "commit", "runtime", "spec_uri"];
        r.push(
            fixed.iter().all(|k| job[k] == bundle["job"][k]),
            format!("assessment {job_id} on {contract} has the recorded terms and tests"),
        );
        let live = subs.as_array().cloned().unwrap_or_default();
        let recorded = bundle["submissions"].as_array().cloned().unwrap_or_default();
        r.push(
            recorded.iter().all(|x| live.contains(x)),
            "its recorded attempts are on chain as recorded",
        );
        check_badges(&mut r, &rpc, contract, job_id, &recorded, at).await?;
    } else {
        r.push(job == bundle["job"], format!("job {job_id} on {contract} is as recorded"));
        r.push(subs == bundle["submissions"], "its submissions on chain are as recorded");
    }
    let (history, _) =
        view_call(&rpc, contract, "get_grader_history", &serde_json::json!({}), at).await?;
    let live = history.as_array().cloned().unwrap_or_default();
    let recorded = bundle["grader_history"].as_array().cloned().unwrap_or_default();
    r.push(
        !recorded.is_empty() && recorded.iter().all(|k| live.contains(k)),
        "every recorded grader key is in the contract's register",
    );
    match bundle["code_sha256"].as_str() {
        Some(recorded) => {
            let live = code_sha256(&rpc, contract, at).await?;
            r.push(
                live == recorded,
                format!(
                    "the contract runs the recorded build {} (code SHA-256 on chain now)",
                    short(recorded)
                ),
            );
        }
        None => r.skip("the deployed code (this recording predates code hashes)".to_owned()),
    }
    let (config, _) = view_call(&rpc, contract, "get_config", &serde_json::json!({}), at).await?;
    r.push(config["allow_dev_keys"] == false, "the contract refuses dev keys now");
    let approved = &bundle["grader"]["approved_measurements"];
    let account = worker_account(contract);
    let block = s(approved, &["block_hash"]);
    let (list, _) = view_call(
        &rpc_for(&account, true),
        &account,
        "get_approved_measurements",
        &serde_json::json!({}),
        Some(block),
    )
    .await?;
    r.push(
        !block.is_empty() && list == approved["list"],
        format!(
            "OutLayer's approved list re-read from {account} at block {} matches the recorded one",
            approved["block_height"]
        ),
    );
    Ok(r)
}

/// Every recorded pass of an assessment has its badge on chain, pointing back at
/// that submission; a revoked badge is reported as revoked.
async fn check_badges(
    r: &mut Report,
    rpc: &str,
    contract: &str,
    job_id: &str,
    recorded: &[Value],
    at: Option<&str>,
) -> Result<(), String> {
    for sub in recorded.iter().filter(|x| s(x, &["status"]) == "Passed") {
        let agent = s(sub, &["agent"]);
        let (badges, _) =
            view_call(rpc, contract, "get_badges", &serde_json::json!({ "account": agent }), at)
                .await?;
        let found = badges.as_array().and_then(|list| {
            list.iter()
                .find(|b| s(b, &["assessment_job_id"]) == job_id && b["sub_id"] == sub["sub_id"])
                .cloned()
        });
        let state = match &found {
            Some(b) if !b["revoked_at_ns"].is_null() => "revoked by its issuer since",
            Some(b) if b["current"] == true => "current",
            Some(_) => "expired",
            None => "",
        };
        r.push(found.is_some(), format!("{agent} holds the badge this pass wrote ({state})"));
    }
    Ok(())
}

/// The SHA-256 of the code `account` runs now, as lowercase hex (the chain
/// reports it in base58).
/// `None` while `contract` still holds the recorded job; once the contract
/// has been deleted, or redeployed under the same name so that the job
/// number now belongs to another job, the hash of the block in which the
/// bundle's last signed result executed.
async fn closed_at(contract: &str, bundle: &Value) -> Result<Option<String>, String> {
    let rpc = rpc_for(contract, false);
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "query",
        "params": { "request_type": "view_account", "finality": "final", "account_id": contract } });
    let reply: Value = reqwest::Client::new()
        .post(&rpc)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("view_account {contract}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("view_account {contract}: {e}"))?;
    if reply["error"]["cause"]["name"] != "UNKNOWN_ACCOUNT" {
        // Same job only if it was created at the same moment.
        let job_id = s(&bundle["job"], &["id"]);
        let live =
            view_call(&rpc, contract, "get_job", &serde_json::json!({ "job_id": job_id }), None)
                .await
                .map_or(Value::Null, |(v, _)| v);
        if live["created_at_ns"] == bundle["job"]["created_at_ns"] {
            return Ok(None);
        }
    }
    let last = bundle["results"].as_array().and_then(|v| v.last()).cloned().unwrap_or(Value::Null);
    let (tx, signer) = (s(&last, &["tx_hash"]), s(&last, &["signer"]));
    if tx.is_empty() {
        return Err(format!(
            "{contract} no longer exists and the bundle names no result transaction"
        ));
    }
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "EXPERIMENTAL_tx_status",
        "params": { "tx_hash": tx, "sender_account_id": signer, "wait_until": "FINAL" } });
    let status: Value = reqwest::Client::new()
        .post(rpc_for(contract, true))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("tx {tx}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("tx {tx}: {e}"))?;
    status["result"]["receipts_outcome"]
        .as_array()
        .and_then(|r| r.last())
        .and_then(|o| o["block_hash"].as_str())
        .map(|b| Some(b.to_owned()))
        .ok_or_else(|| format!("tx {tx}: no receipt block on the archival node"))
}

async fn code_sha256(rpc: &str, account: &str, at: Option<&str>) -> Result<String, String> {
    let mut params = serde_json::json!({ "request_type": "view_account", "account_id": account });
    match at {
        Some(hash) => params["block_id"] = hash.into(),
        None => params["finality"] = "final".into(),
    }
    let body =
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "query", "params": params });
    let reply: Value = reqwest::Client::new()
        .post(rpc)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("view_account {account}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("view_account {account}: {e}"))?;
    let b58 = reply["result"]["code_hash"]
        .as_str()
        .ok_or_else(|| format!("view_account {account}: no code hash"))?;
    let bytes = bs58::decode(b58).into_vec().map_err(|e| format!("code hash {b58}: {e}"))?;
    Ok(hex::encode(bytes))
}

/// Fetches Intel's collateral for the bundle's quote and OutLayer's approved
/// measurement list, and stores both in the bundle, with the time and block,
/// so later checks need no network.
///
/// # Errors
/// Text describing a missing quote or a failed fetch.
pub async fn fetch_collateral(bundle: &mut Value) -> Result<(), String> {
    let quote = base64::engine::general_purpose::STANDARD
        .decode(s(&bundle["grader"]["attestation"], &["tdx_quote"]))
        .map_err(|e| format!("the bundle has no readable TDX quote: {e}"))?;
    let pccs = std::env::var("PCCS_URL")
        .unwrap_or_else(|_| dcap_qvl::collateral::PHALA_PCCS_URL.to_owned());
    let client = dcap_qvl::collateral::CollateralClient::with_default_http(pccs.clone())
        .map_err(|e| e.to_string())?;
    let collateral =
        client.fetch(&quote).await.map_err(|e| format!("fetching collateral from {pccs}: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    bundle["grader"]["collateral"] =
        serde_json::to_value(&collateral).map_err(|e| e.to_string())?;
    bundle["grader"]["collateral_fetched_at"] = now.into();
    bundle["grader"]["collateral_source"] = pccs.into();
    let contract = s(bundle, &["contract"]).to_owned();
    bundle["grader"]["approved_measurements"] = fetch_approved(&contract).await?;
    Ok(())
}

fn check_registration(r: &mut Report, bundle: &Value, frozen: &Value) {
    let grader = &bundle["grader"];
    let attestation = &grader["attestation"];
    let output = base64::engine::general_purpose::STANDARD
        .decode(s(grader, &["identity_output_base64"]))
        .unwrap_or_default();
    let attested = s(attestation, &["output_hash"]);
    r.push(
        !output.is_empty() && Hash32::digest(&output).to_string() == attested,
        format!("identity output hashes to the attested output hash {}", short(attested)),
    );
    // The build inside the task hash, the one Intel's quote covers.
    let executed = s(attestation, &["wasm_hash"]);
    r.push(
        !executed.is_empty() && executed == s(frozen, &["wasm_sha256"]),
        format!("attested build {} is the build of the job's grader key", short(executed)),
    );
    let caller = s(attestation, &["caller_account_id"]);
    r.push(
        !caller.is_empty() && caller == s(grader, &["caller"]),
        format!("attested caller {caller} is the relayer that grades"),
    );
    let identity: Value = serde_json::from_slice(&output).unwrap_or(Value::Null);
    let signing = s(&identity, &["signing_public_key"]);
    // The contract's register of every grader key, as recorded with the run.
    let registered = bundle["grader_history"].as_array().is_some_and(|keys| {
        keys.iter().any(|k| {
            s(k, &["public_key"]) == signing
                && s(k, &["wasm_sha256"]) == executed
                && s(k, &["box_public_key"]) == s(&identity, &["box_public_key"])
        })
    });
    r.push(
        !signing.is_empty() && registered,
        format!(
            "the enclave's signing key {} is registered on the contract for that build",
            short(signing)
        ),
    );
    r.push(
        !signing.is_empty() && signing == s(frozen, &["public_key"]),
        "the same key was frozen into the job when its suite was committed",
    );
    let boxk = s(&identity, &["box_public_key"]);
    r.push(
        !boxk.is_empty() && boxk == s(frozen, &["box_public_key"]),
        format!("the enclave's box key {} is the one submissions were encrypted to", short(boxk)),
    );
    r.push(
        s(frozen, &["mode"]) == "outlayer_tdx" && bundle["config"]["allow_dev_keys"] == false,
        "the job's key is registered as an enclave key and the contract refuses dev keys",
    );
}

/// Checks every signed result and returns the submissions a verified score
/// passed. Every graded submission needs exactly one score signed by the job's
/// key, and the job needs exactly one valid suite check.
fn check_results(
    r: &mut Report,
    bundle: &Value,
    contract: &str,
    job: &Value,
    frozen: &Value,
) -> Vec<u64> {
    let key = s(frozen, &["public_key"]);
    let commit = &job["commit"];
    let job_id = s(job, &["id"]);
    let empty = Vec::new();
    let results = bundle["results"].as_array().unwrap_or(&empty);
    let subs = bundle["submissions"].as_array().unwrap_or(&empty);
    r.push(
        !job_id.is_empty() && bundle["job_id"].to_string().trim_matches('"') == job_id,
        format!("bundle is for job {job_id}"),
    );
    r.push(!results.is_empty(), format!("{} signed results recorded", results.len()));
    let mut checks = 0;
    let mut scored: Vec<u64> = Vec::new();
    let mut passes = Vec::new();
    for res in results {
        let sig = s(res, &["args", "signature_hex"]);
        match s(res, &["kind"]) {
            "suite_check" => {
                let Ok(check) = serde_json::from_value::<SuiteCheck>(res["args"]["check"].clone())
                else {
                    r.push(false, "suite check is well formed");
                    continue;
                };
                let fields = check.contract == contract
                    && check.job_id.to_string() == job_id
                    && check.suite_hash.to_string() == s(commit, &["suite_hash"])
                    && check.ref_solution_hash.to_string() == s(commit, &["ref_solution_hash"])
                    && check.nonce.to_string() == s(commit, &["suite_check_nonce"]);
                r.push(fields, "suite check names this contract, job, suite, reference and nonce");
                let signed = verifies(key, &check.signing_message(), sig);
                r.push(
                    signed,
                    format!(
                        "suite check (valid: {}) is signed by the job's grader key",
                        check.valid
                    ),
                );
                if signed && fields && check.valid {
                    checks += 1;
                }
            }
            "score" => {
                let Ok(score) = serde_json::from_value::<Score>(res["args"]["score"].clone())
                else {
                    r.push(false, "score is well formed");
                    continue;
                };
                let id = u64::from(score.sub_id);
                let sub = subs.iter().find(|x| x["sub_id"].as_u64() == Some(id));
                let sub = sub.unwrap_or(&Value::Null);
                let fields = !sub.is_null()
                    && score.contract == contract
                    && score.job_id.to_string() == job_id
                    && score.runtime == s(job, &["runtime"])
                    && score.suite_hash.to_string() == s(commit, &["suite_hash"])
                    && score.code_hash.to_string() == s(sub, &["code_hash"])
                    && score.nonce.to_string() == s(sub, &["nonce"])
                    && sub["passed"].as_u64() == Some(u64::from(score.passed))
                    && sub["total"].as_u64() == Some(u64::from(score.total));
                let status = if score.is_pass() { "Passed" } else { "Failed" };
                r.push(
                    fields && s(sub, &["status"]) == status && !scored.contains(&id),
                    format!(
                        "score for submission {} ({}/{}) matches its job, code hash, nonce and status {status}",
                        score.sub_id, score.passed, score.total
                    ),
                );
                let signed = verifies(key, &score.signing_message(), sig);
                r.push(
                    signed,
                    format!(
                        "score for submission {} is signed by the job's grader key",
                        score.sub_id
                    ),
                );
                scored.push(id);
                if signed && fields && score.is_pass() {
                    passes.push(id);
                }
            }
            other => r.push(false, format!("unknown result kind {other:?}")),
        }
    }
    r.push(checks == 1, "exactly one valid suite check, signed by the job's key");
    let graded: Vec<u64> = subs
        .iter()
        .filter(|x| x["status"] == "Passed" || x["status"] == "Failed")
        .filter_map(|x| x["sub_id"].as_u64())
        .collect();
    r.push(
        graded.iter().all(|id| scored.contains(id)),
        format!("every graded submission ({}) has a signed score", graded.len()),
    );
    passes
}

/// The winner must be the earliest submission a verified signed score passed.
fn check_payout(r: &mut Report, bundle: &Value, job: &Value, passes: &[u64]) {
    let empty = Vec::new();
    let subs = bundle["submissions"].as_array().unwrap_or(&empty);
    let first_pass = passes.iter().min().copied();
    if job["assessment"].is_object() {
        // A skill assessment pays nothing: each full pass writes a badge instead.
        let marked: Vec<u64> = subs
            .iter()
            .filter(|x| s(x, &["status"]) == "Passed")
            .filter_map(|x| x["sub_id"].as_u64())
            .collect();
        let mut signed = passes.to_vec();
        signed.sort_unstable();
        r.push(
            s(job, &["reward"]) == "0" && marked == signed,
            format!(
                "skill assessment ({}): no reward, and the {} signed full pass(es) are exactly the submissions recorded as Passed, each of which wrote a badge (`--online` reads the badges)",
                s(&job["assessment"], &["skill"]),
                signed.len()
            ),
        );
    } else if s(job, &["status"]) == "Paid" {
        let winner = job["winning_sub_id"].as_u64();
        let agent = first_pass
            .and_then(|id| subs.iter().find(|x| x["sub_id"].as_u64() == Some(id)))
            .map_or("", |x| s(x, &["agent"]));
        r.push(
            winner.is_some() && winner == first_pass && s(job, &["winner"]) == agent,
            format!(
                "the recorded payout went to the earliest signed pass ({agent}); the account and amount are recorded, not signed (`--online` re-reads them from chain)"
            ),
        );
    } else {
        r.skip(format!("payout (job is {})", s(job, &["status"])));
    }
}

fn short(h: &str) -> String {
    if h.len() > 12 {
        format!("{}…", &h[..12])
    } else {
        h.to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn bundle() -> Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deployments/runs/testnet-job1.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_recorded_job_verifies() {
        let r = verify_bundle(&bundle());
        assert_eq!(r.failed(), 0, "{}", r.render());
        assert!(
            r.lines.iter().all(|(st, _)| *st != Status::Skip),
            "the Intel quote is checked too: {}",
            r.render()
        );
    }

    fn flip(hex_or_text: &str) -> String {
        let mut c: Vec<char> = hex_or_text.chars().collect();
        c[0] = if c[0] == 'a' { 'b' } else { 'a' };
        c.into_iter().collect()
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one case per tampered fact")]
    fn any_tampering_fails() {
        type Change = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Change)> = vec![
            (
                "a signature",
                Box::new(|b| {
                    let s = b["results"][1]["args"]["signature_hex"].as_str().unwrap().to_owned();
                    b["results"][1]["args"]["signature_hex"] = flip(&s).into();
                }),
            ),
            ("a pass count", Box::new(|b| b["results"][1]["args"]["score"]["passed"] = 12.into())),
            (
                "the identity output",
                Box::new(|b| {
                    let s = b["grader"]["identity_output_base64"].as_str().unwrap().to_owned();
                    b["grader"]["identity_output_base64"] = format!("A{}", &s[1..]).into();
                }),
            ),
            (
                "the job's grader build",
                Box::new(|b| {
                    b["job"]["commit"]["grader"]["wasm_sha256"] = "00".repeat(32).into();
                }),
            ),
            (
                "the grader register",
                Box::new(|b| {
                    for k in b["grader_history"].as_array_mut().unwrap() {
                        k["wasm_sha256"] = "00".repeat(32).into();
                    }
                }),
            ),
            ("the caller", Box::new(|b| b["grader"]["caller"] = "mallory.testnet".into())),
            (
                "the frozen key",
                Box::new(|b| {
                    b["job"]["commit"]["grader"]["public_key"] = "11".repeat(32).into();
                }),
            ),
            ("the winner", Box::new(|b| b["job"]["winning_sub_id"] = 0.into())),
            (
                "a code hash",
                Box::new(|b| b["submissions"][0]["code_hash"] = "22".repeat(32).into()),
            ),
            ("dev keys allowed", Box::new(|b| b["config"]["allow_dev_keys"] = true.into())),
            (
                "the TDX quote",
                Box::new(|b| {
                    let q = b["grader"]["attestation"]["tdx_quote"].as_str().unwrap().to_owned();
                    let mut raw = base64::engine::general_purpose::STANDARD.decode(q).unwrap();
                    raw[600] ^= 1;
                    b["grader"]["attestation"]["tdx_quote"] =
                        base64::engine::general_purpose::STANDARD.encode(raw).into();
                }),
            ),
            (
                "the attested caller",
                Box::new(|b| {
                    b["grader"]["attestation"]["caller_account_id"] = "mallory.testnet".into();
                    b["grader"]["caller"] = "mallory.testnet".into();
                }),
            ),
            (
                "the quote, removed",
                Box::new(|b| b["grader"]["attestation"]["tdx_quote"] = "".into()),
            ),
            ("Intel's collateral, removed", Box::new(|b| b["grader"]["collateral"] = Value::Null)),
            (
                "the approved list, removed",
                Box::new(|b| b["grader"]["approved_measurements"] = Value::Null),
            ),
            (
                "the build, swapped everywhere the quote does not cover",
                Box::new(|b| {
                    let fake: Value = "ab".repeat(32).into();
                    b["grader"]["attestation"]["executed_wasm_sha256"] = fake.clone();
                    b["job"]["commit"]["grader"]["wasm_sha256"] = fake.clone();
                    for k in b["grader_history"].as_array_mut().unwrap() {
                        k["wasm_sha256"] = fake.clone();
                    }
                }),
            ),
            (
                "the output hash, moved into the input hash",
                Box::new(|b| {
                    let a = &mut b["grader"]["attestation"];
                    let joined = format!(
                        "{}{}",
                        a["input_hash"].as_str().unwrap(),
                        a["output_hash"].as_str().unwrap()
                    );
                    a["input_hash"] = joined.into();
                    a.as_object_mut().unwrap().remove("output_hash");
                    a["nested"] = serde_json::json!({ "output_hash": "cd".repeat(32) });
                }),
            ),
            (
                "a score, removed and its submission marked passed",
                Box::new(|b| {
                    b["results"].as_array_mut().unwrap().retain(|r| r["kind"] != "score");
                    b["submissions"][0]["status"] = "Passed".into();
                    b["job"]["winning_sub_id"] = 0.into();
                    let agent = b["submissions"][0]["agent"].clone();
                    b["job"]["winner"] = agent;
                }),
            ),
            (
                "the job id",
                Box::new(|b| {
                    b["job"]["id"] = "99".into();
                    b["job_id"] = 99.into();
                }),
            ),
            (
                "the approved measurement list",
                Box::new(|b| {
                    for e in b["grader"]["approved_measurements"]["list"].as_array_mut().unwrap() {
                        e["rtmr2"] = "44".repeat(48).into();
                    }
                }),
            ),
            (
                "the worker measurement",
                Box::new(|b| {
                    b["grader"]["attestation"]["worker_measurement"] = "33".repeat(48).into();
                }),
            ),
        ];
        for (what, change) in cases {
            let mut b = bundle();
            change(&mut b);
            assert!(verify_bundle(&b).failed() > 0, "tampering with {what} must fail");
        }
    }
}
