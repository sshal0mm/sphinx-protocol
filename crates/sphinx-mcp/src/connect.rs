//! Self-serve signing connectors.
//!
//! Anyone with a NEAR wallet can get a connector address for their own
//! account, to paste into any chat app or agent tool that takes MCP
//! connectors, without handing over their wallet's key:
//!
//! 1. `POST /connect/start {account}` makes a new ed25519 key and returns its
//!    public half, the escrow contract, the methods it may call, the gas
//!    allowance to grant, and a one-time `claim` secret that only this caller
//!    sees (the server keeps only its SHA-256).
//! 2. The owner's own wallet adds that public key to their account as a
//!    function-call key limited to the escrow and those methods, and
//!    deposits a prepaid balance with `deposit_balance`. Only the owner can
//!    add a key to their account, so this proves ownership.
//! 3. `POST /connect/finish {account, public_key, claim}` checks the claim,
//!    reads the key from the chain and accepts it only with exactly that
//!    limit and an allowance no larger than the one asked for, then returns a
//!    private connector address, `/mcp/<network>/<token>`. The account and
//!    public key are public once the key is on chain; the claim is what keeps
//!    someone watching the chain from finishing first. Each claim is used
//!    once, and each key gets at most one connector.
//!
//! The server keeps only the limited key. It can submit work paid from the
//! prepaid balance, collect, settle and reclaim; it cannot move the account's
//! money anywhere else. The owner revokes it by deleting the key in their
//! wallet, and withdraws the balance with `withdraw_balance`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, post};
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sphinx_near::near_api::types::crypto::secret_key::SecretKey;
use sphinx_near::near_api::types::AccountId;
use sphinx_near::near_api::Signer;
use sphinx_near::Escrow;
use tower::ServiceExt;

use crate::{Identity, Settings, SphinxMcp};

/// The only methods a connector key may call: nothing here sends the
/// account's money anywhere but back to the account.
pub const ALLOWED_METHODS: &[&str] = &[
    "submit_from_balance",
    "withdraw_balance",
    "withdraw",
    "reclaim_bond",
    "settle",
    "register_agent",
    "unregister_agent",
];
/// Gas the key may burn before the owner tops it up (0.25 NEAR).
pub const ALLOWANCE_YOCTO: u128 = 250_000_000_000_000_000_000_000;
/// How long a started connection waits for the wallet.
const PENDING_FOR: Duration = Duration::from_secs(3600);
/// Most connections waiting for a wallet at once.
const MAX_PENDING: usize = 1000;

type Service = StreamableHttpService<SphinxMcp, LocalSessionManager>;

#[derive(Serialize, Deserialize, Clone)]
struct Saved {
    account: String,
    secret_key: String,
    public_key: String,
}

struct Pending {
    account: String,
    secret_key: String,
    claim_sha256: [u8; 32],
    at: Instant,
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// Compares two digests in time that does not depend on where they differ.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Shared state of the connect server.
pub struct Connect {
    contract: AccountId,
    network: &'static str,
    api: Option<String>,
    public_base: String,
    hosts: Vec<String>,
    file: PathBuf,
    rpc: String,
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    saved: Mutex<HashMap<String, Saved>>,
    services: Mutex<HashMap<String, Service>>,
}

impl std::fmt::Debug for Connect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connect")
            .field("contract", &self.contract)
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct StartBody {
    account: String,
}

#[derive(Deserialize)]
struct FinishBody {
    account: String,
    public_key: String,
    #[serde(default)]
    claim: String,
}

fn refuse(status: StatusCode, why: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": why }))).into_response()
}

fn token() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..40).map(|_| ALPHABET[rand::random::<usize>() % ALPHABET.len()] as char).collect()
}

/// A new ed25519 key in NEAR's text form: (secret, public).
fn new_key() -> (String, String) {
    let signing = ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    let public = signing.verifying_key().to_bytes();
    let mut both = signing.to_bytes().to_vec();
    both.extend_from_slice(&public);
    (
        format!("ed25519:{}", bs58::encode(both).into_string()),
        format!("ed25519:{}", bs58::encode(public).into_string()),
    )
}

/// Whether a key's on-chain permission is exactly a connector's: limited to
/// `contract`, to a non-empty subset of [`ALLOWED_METHODS`], with a gas
/// allowance of at most [`ALLOWANCE_YOCTO`] (an unlimited allowance is
/// refused).
fn permission_ok(permission: &Value, contract: &str) -> bool {
    let Some(fc) = permission.get("FunctionCall") else { return false };
    let receiver_ok = fc["receiver_id"].as_str() == Some(contract);
    let allowance_ok = fc["allowance"]
        .as_str()
        .and_then(|a| a.parse::<u128>().ok())
        .is_some_and(|a| a > 0 && a <= ALLOWANCE_YOCTO);
    let methods: Vec<&str> = fc["method_names"]
        .as_array()
        .map(|m| m.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    receiver_ok
        && allowance_ok
        && !methods.is_empty()
        && methods.iter().all(|m| ALLOWED_METHODS.contains(m))
}

impl Connect {
    /// Loads saved connectors from `dir/connectors.json`.
    ///
    /// # Errors
    /// When the directory cannot be created or the file cannot be read.
    pub fn new(
        contract: AccountId,
        api: Option<String>,
        public_base: &str,
        hosts: Vec<String>,
        dir: &std::path::Path,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let file = dir.join("connectors.json");
        let saved: HashMap<String, Saved> = match std::fs::read(&file) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display()))?
            }
            Err(_) => HashMap::new(),
        };
        let escrow = Escrow::for_contract(contract.clone());
        let network = if escrow.is_mainnet() { "mainnet" } else { "testnet" };
        let rpc = escrow
            .network()
            .rpc_endpoints
            .first()
            .map_or_else(|| sphinx_near::TESTNET_RPC[0].to_owned(), |e| e.url.to_string());
        Ok(Self {
            contract,
            network,
            api,
            public_base: public_base.trim_end_matches('/').to_owned(),
            hosts,
            file,
            rpc,
            http: reqwest::Client::new(),
            pending: Mutex::new(HashMap::new()),
            saved: Mutex::new(saved),
            services: Mutex::new(HashMap::new()),
        })
    }

    /// The routes: `/connect/start`, `/connect/finish` and the connectors.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/connect/start", post(start))
            .route("/connect/finish", post(finish))
            .route("/mcp/{network}/{token}", any(serve))
            .with_state(self)
    }

    fn persist(&self, saved: &HashMap<String, Saved>) -> Result<(), String> {
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(saved).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())
    }

    /// The key's permission on chain, or `None` if the account has no such key.
    async fn access_key(&self, account: &str, public_key: &str) -> Option<Value> {
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "query",
            "params": { "request_type": "view_access_key", "finality": "final",
                        "account_id": account, "public_key": public_key }
        });
        let v: Value =
            self.http.post(&self.rpc).json(&body).send().await.ok()?.json().await.ok()?;
        v.get("result").map(|r| r["permission"].clone())
    }

    /// Whether the contract has prepaid balances (`get_agent_balance`
    /// answers). A contract deployed before them cannot take a connector's
    /// deposit, so sign-up stays closed until it is upgraded.
    async fn has_balances(&self) -> bool {
        let args = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            json!({ "account": self.contract }).to_string(),
        );
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "query",
            "params": { "request_type": "call_function", "finality": "final",
                        "account_id": self.contract, "method_name": "get_agent_balance",
                        "args_base64": args }
        });
        let Ok(resp) = self.http.post(&self.rpc).json(&body).send().await else {
            return false;
        };
        let v: Value = resp.json().await.unwrap_or_default();
        v["result"]["result"].is_array()
    }

    /// Takes the waiting connection for `public_key` if `account` and `claim`
    /// match, removing it under the same lock, so a claim works once even
    /// when two requests race.
    fn take_pending(&self, account: &str, public_key: &str, claim: &str) -> Option<Pending> {
        let digest = sha256(claim.as_bytes());
        let mut pending = self.pending.lock().ok()?;
        let p = pending.get(public_key)?;
        if p.account != account || p.at.elapsed() >= PENDING_FOR || !same(&p.claim_sha256, &digest) {
            return None;
        }
        pending.remove(public_key)
    }

    /// Puts a taken connection back, for a retry once the key is on chain.
    fn put_back(&self, public_key: &str, p: Pending) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(public_key.to_owned(), p);
        }
    }

    /// The MCP service of one connector, built on first use.
    fn service(&self, token: &str) -> Option<Service> {
        if let Some(s) = self.services.lock().ok()?.get(token) {
            return Some(s.clone());
        }
        let saved = self.saved.lock().ok()?.get(token).cloned()?;
        let account: AccountId = saved.account.parse().ok()?;
        let key: SecretKey = saved.secret_key.parse().ok()?;
        let signer = Signer::from_secret_key(key.clone()).ok()?;
        let settings = Settings {
            escrow: Escrow::for_contract(self.contract.clone()),
            api: self.api.clone(),
            identity: Some(Identity { account, signer, key }),
            model: None,
            drafts: std::env::temp_dir(),
            // Nothing is attached from the account: bonds come from the
            // prepaid balance.
            max_spend: 0,
            pay_from_balance: true,
        };
        let server = SphinxMcp::agent_only(settings);
        let mut config = StreamableHttpServerConfig::default();
        if !self.hosts.is_empty() {
            config = config.with_allowed_hosts(
                ["localhost", "127.0.0.1", "::1"]
                    .map(str::to_owned)
                    .into_iter()
                    .chain(self.hosts.clone()),
            );
        }
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        self.services.lock().ok()?.insert(token.to_owned(), service.clone());
        Some(service)
    }
}

async fn start(State(c): State<Arc<Connect>>, Json(b): Json<StartBody>) -> Response {
    if b.account.parse::<AccountId>().is_err() {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "not a NEAR account id");
    }
    if !c.has_balances().await {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "connectors are not open on this network yet: its contract has no prepaid balances",
        );
    }
    let (secret_key, public_key) = new_key();
    let claim = hex::encode(rand::random::<[u8; 32]>());
    {
        let Ok(mut pending) = c.pending.lock() else {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, "busy");
        };
        // One waiting connection per account: a new start replaces the old.
        pending.retain(|_, p| p.at.elapsed() < PENDING_FOR && p.account != b.account);
        if pending.len() >= MAX_PENDING {
            return refuse(
                StatusCode::TOO_MANY_REQUESTS,
                "too many connections waiting; try again later",
            );
        }
        pending.insert(
            public_key.clone(),
            Pending {
                account: b.account.clone(),
                secret_key,
                claim_sha256: sha256(claim.as_bytes()),
                at: Instant::now(),
            },
        );
    }
    Json(json!({
        "ok": true,
        "account": b.account,
        "network": c.network,
        "public_key": public_key,
        "contract": c.contract,
        "method_names": ALLOWED_METHODS,
        "allowance_yocto": ALLOWANCE_YOCTO.to_string(),
        "min_first_deposit_yocto": "50000000000000000000000",
        "claim": claim,
    }))
    .into_response()
}

async fn finish(State(c): State<Arc<Connect>>, Json(b): Json<FinishBody>) -> Response {
    let Some(p) = c.take_pending(&b.account, &b.public_key, &b.claim) else {
        return refuse(
            StatusCode::NOT_FOUND,
            "no connection waiting for this account, key and claim; start again",
        );
    };
    let Some(permission) = c.access_key(&b.account, &b.public_key).await else {
        c.put_back(&b.public_key, p);
        return refuse(
            StatusCode::CONFLICT,
            "the key is not on the account yet; approve it in your wallet, then try again",
        );
    };
    if !permission_ok(&permission, c.contract.as_str()) {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the key must be a function-call key limited to the Sphinx escrow and its connector methods; a full-access key is refused",
        );
    }
    let token = token();
    let saved =
        Saved { account: b.account.clone(), secret_key: p.secret_key, public_key: b.public_key.clone() };
    {
        let Ok(mut all) = c.saved.lock() else {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, "busy");
        };
        if all.values().any(|s| s.public_key == b.public_key) {
            return refuse(StatusCode::CONFLICT, "this key already has a connector");
        }
        all.insert(token.clone(), saved);
        if let Err(e) = c.persist(&all) {
            all.remove(&token);
            return refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not save the connector: {e}"),
            );
        }
    }
    Json(json!({
        "ok": true,
        "account": b.account,
        "connector": format!("{}/mcp/{}/{token}", c.public_base, c.network),
    }))
    .into_response()
}

async fn serve(
    State(c): State<Arc<Connect>>,
    Path((network, token)): Path<(String, String)>,
    req: Request,
) -> Response {
    if network != c.network {
        return refuse(StatusCode::NOT_FOUND, "no such connector");
    }
    let Some(service) = c.service(&token) else {
        return refuse(StatusCode::NOT_FOUND, "no such connector");
    };
    match service.oneshot(req).await {
        Ok(res) => res.map(axum::body::Body::new).into_response(),
        Err(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_key_limited_to_the_escrow_and_connector_methods_is_accepted() {
        let ok = json!({ "FunctionCall": { "receiver_id": "escrow.sphinxescrow.near", "allowance": "250000000000000000000000",
                                           "method_names": ["submit_from_balance", "withdraw_balance"] } });
        assert!(permission_ok(&ok, "escrow.sphinxescrow.near"));
        let unlimited = json!({ "FunctionCall": { "receiver_id": "escrow.sphinxescrow.near", "allowance": null,
                                                  "method_names": ["submit_from_balance"] } });
        assert!(!permission_ok(&unlimited, "escrow.sphinxescrow.near"));
        let larger = json!({ "FunctionCall": { "receiver_id": "escrow.sphinxescrow.near", "allowance": "250000000000000000000001",
                                               "method_names": ["submit_from_balance"] } });
        assert!(!permission_ok(&larger, "escrow.sphinxescrow.near"));
        assert!(!permission_ok(&json!("FullAccess"), "escrow.sphinxescrow.near"));
        let other = json!({ "FunctionCall": { "receiver_id": "evil.near", "method_names": ["submit_from_balance"] } });
        assert!(!permission_ok(&other, "escrow.sphinxescrow.near"));
        let any_method = json!({ "FunctionCall": { "receiver_id": "escrow.sphinxescrow.near", "method_names": [] } });
        assert!(!permission_ok(&any_method, "escrow.sphinxescrow.near"));
        let extra = json!({ "FunctionCall": { "receiver_id": "escrow.sphinxescrow.near",
                                              "method_names": ["submit_from_balance", "set_grader_key"] } });
        assert!(!permission_ok(&extra, "escrow.sphinxescrow.near"));
    }

    fn server() -> Connect {
        let dir = std::env::temp_dir().join(format!("sphinx-connect-test-{}", token()));
        Connect::new("escrow.sphinx999.testnet".parse().unwrap(), None, "https://x", vec![], &dir)
            .unwrap()
    }

    fn wait(c: &Connect, account: &str, public_key: &str, claim: &str) {
        c.pending.lock().unwrap().insert(
            public_key.to_owned(),
            Pending {
                account: account.to_owned(),
                secret_key: "s".to_owned(),
                claim_sha256: sha256(claim.as_bytes()),
                at: Instant::now(),
            },
        );
    }

    #[test]
    fn a_connection_is_finished_only_with_its_claim_and_only_once() {
        let c = server();
        wait(&c, "kemi.testnet", "ed25519:K", "secret-claim");
        // Someone watching the chain knows the account and the key, not the claim.
        assert!(c.take_pending("kemi.testnet", "ed25519:K", "").is_none());
        assert!(c.take_pending("kemi.testnet", "ed25519:K", "guess").is_none());
        assert!(c.take_pending("other.testnet", "ed25519:K", "secret-claim").is_none());
        // The owner's page finishes it, once; a racing second finish gets nothing.
        assert!(c.take_pending("kemi.testnet", "ed25519:K", "secret-claim").is_some());
        assert!(c.take_pending("kemi.testnet", "ed25519:K", "secret-claim").is_none());
    }

    #[test]
    fn new_keys_parse_and_tokens_are_long() {
        let (secret, public) = new_key();
        let key: SecretKey = secret.parse().unwrap();
        assert_eq!(key.public_key().to_string(), public);
        assert_eq!(token().len(), 40);
        assert_ne!(token(), token());
    }
}
