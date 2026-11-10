//! `POST /v1/relay` refuses doomed and excessive calls before spending gas.
//! Every case here stops before the network.
#![allow(clippy::unwrap_used, clippy::unused_async_trait_impl, reason = "test fixtures")]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use ed25519_dalek::{Signer as _, SigningKey};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sphinx_api::chain::Chain;
use sphinx_api::code_store::CodeStore;
use sphinx_api::limits::{RelayLimiter, RelayLimits};
use sphinx_api::{router, AppState, Relay};
use sphinx_near::near_api::types::crypto::public_key::ED25519PublicKey;
use sphinx_near::near_api::types::json::U64;
use sphinx_near::near_api::types::signable_message::{SignableMessage, SignableMessageType};
use sphinx_near::near_api::types::transaction::actions::{
    AccessKey, AccessKeyPermission, Action, FunctionCallAction,
};
use sphinx_near::near_api::types::transaction::delegate_action::{
    DelegateAction, NonDelegateAction, SignedDelegateAction,
};
use sphinx_near::near_api::types::{
    AccountId, CryptoHash, NearGas, NearToken, PublicKey, Signature,
};
use sphinx_near::near_api::{signer, Signer};
use sphinx_near::views::{Config, Job, Submission};
use sphinx_near::Escrow;
use sphinx_protocol::{Code, SphinxError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

/// A chain whose `check_*` views refuse job 0 and record what was asked.
#[derive(Default)]
struct Previews {
    seen: Mutex<Vec<(String, Value)>>,
}

/// The chain the app sees; shares its recorder with the test.
struct FakeChain(Arc<Previews>);

impl Chain for FakeChain {
    async fn config(&self) -> Result<Config, SphinxError> {
        Err(SphinxError::new(Code::RelayRpcFailed, "offline"))
    }
    async fn jobs(&self, _: u64, _: u32) -> Result<Vec<Job>, SphinxError> {
        Ok(Vec::new())
    }
    async fn job(&self, id: u64) -> Result<Job, SphinxError> {
        Err(SphinxError::new(Code::JobNotFound, format!("job {id} does not exist")))
    }
    fn contract(&self) -> &'static str {
        "escrow.testnet"
    }
    async fn agents(&self, _: u32) -> Result<Vec<sphinx_near::views::AgentView>, SphinxError> {
        Ok(Vec::new())
    }
    async fn agent(&self, _: &str) -> Result<Option<sphinx_near::views::AgentView>, SphinxError> {
        Ok(None)
    }
    async fn agent_stats(&self, _: &str) -> Result<sphinx_near::views::AgentStats, SphinxError> {
        Ok(sphinx_near::views::AgentStats::default())
    }
    async fn invites(&self, _: u64) -> Result<Vec<String>, SphinxError> {
        Ok(Vec::new())
    }
    async fn submissions(&self, _: u64, _: u32, _: u32) -> Result<Vec<Submission>, SphinxError> {
        Ok(Vec::new())
    }
    async fn submission(&self, job: u64, sub: u32) -> Result<Submission, SphinxError> {
        Err(SphinxError::new(Code::ScoreNoSubmission, format!("job {job} has no submission {sub}")))
    }
    /// Every sender holds the signing key as a full-access key, except
    /// `victim.testnet`, which holds none of the keys these tests sign with.
    async fn access_key(
        &self,
        sda: &SignedDelegateAction,
    ) -> Result<(Option<AccessKey>, u64), SphinxError> {
        if sda.delegate_action.sender_id.as_str() == "victim.testnet" {
            return Ok((None, 10));
        }
        Ok((Some(AccessKey { nonce: U64(0), permission: AccessKeyPermission::FullAccess }), 10))
    }
    async fn is_access_key(&self, _: &str, _: &str) -> Result<bool, SphinxError> {
        Ok(false)
    }
    async fn check(&self, view: &'static str, args: Value) -> Result<(), SphinxError> {
        self.0.seen.lock().unwrap().push((view.to_owned(), args.clone()));
        if args["job_id"] == "0" {
            return Err(SphinxError::new(
                Code::JobWrongState,
                "job 0 is Paid; this call needs Open",
            )
            .ctx("job_id", 0));
        }
        Err(SphinxError::new(Code::RelayRpcFailed, "stop before sending"))
    }
}

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// A signed `reclaim_bond`, the kind of call the relay pays for (no deposit).
fn signed_reclaim(agent: &str, job_id: &str) -> String {
    signed_call(agent, "reclaim_bond", &json!({ "job_id": job_id, "sub_id": 0 }), 0)
}

fn signed_call(agent: &str, method: &str, args: &Value, deposit_millinear: u128) -> String {
    let key = SigningKey::from_bytes(&[5u8; 32]);
    let action = NonDelegateAction::try_from(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: method.into(),
        args: args.to_string().into_bytes(),
        gas: NearGas::from_tgas(50),
        deposit: NearToken::from_millinear(deposit_millinear),
    })))
    .unwrap();
    let da = DelegateAction {
        sender_id: acct(agent),
        receiver_id: acct("escrow.testnet"),
        actions: vec![action],
        nonce: 1,
        max_block_height: 1_000,
        public_key: PublicKey::ED25519(ED25519PublicKey(key.verifying_key().to_bytes())),
    };
    let payload =
        borsh::to_vec(&SignableMessage::new(&da, SignableMessageType::DelegateAction)).unwrap();
    let sig = key.sign(&CryptoHash::hash(&payload).0);
    sphinx_near::relay::encode(&SignedDelegateAction {
        delegate_action: da,
        signature: Signature::ED25519(sig),
    })
}

fn app(previews: Arc<Previews>, limits: RelayLimits) -> axum::Router {
    let escrow = Escrow::testnet(acct("escrow.testnet"));
    let signer: Arc<Signer> =
        Signer::from_secret_key(signer::generate_secret_key().unwrap()).unwrap();
    router(Arc::new(AppState {
        chain: FakeChain(previews),
        code: CodeStore::new(std::env::temp_dir().join("sphinx-relay-test")),
        public_base: "https://api.example".into(),
        relay: Some(Relay {
            escrow,
            relayer: acct("relayer.testnet"),
            signer,
            limiter: RelayLimiter::new(limits, std::time::Instant::now()),
            clients: sphinx_api::limits::ClientLimiter::new(
                100,
                100,
                1_000,
                std::time::Instant::now(),
            ),
            daily: sphinx_api::limits::DailyCap::new(1_000),
        }),
        uids: sphinx_api::uids::UidIndex::default(),
    }))
}

async fn relay(app: axum::Router, sda: String) -> (StatusCode, Option<String>, Value) {
    let body = json!({ "signed_delegate_action": sda }).to_string();
    let res =
        app.oneshot(Request::post("/v1/relay").body(Body::from(body)).unwrap()).await.unwrap();
    let status = res.status();
    let retry = res.headers().get(header::RETRY_AFTER).map(|v| v.to_str().unwrap().to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, retry, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn a_doomed_reclaim_is_refused_with_the_contracts_error() {
    let previews = Arc::new(Previews::default());
    let app = app(previews.clone(), RelayLimits::default());
    let (status, _, body) = relay(app, signed_reclaim("agent.testnet", "0")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "SPHINX_E_JOB_WRONG_STATE");
    let seen = previews.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "check_reclaim_bond");
    assert_eq!(seen[0].1["caller"], "agent.testnet");
}

#[tokio::test]
async fn an_account_over_its_rate_gets_429_with_retry_after() {
    let limits = RelayLimits { burst: 1, per_minute: 1, ..RelayLimits::default() };
    let app = app(Arc::new(Previews::default()), limits);
    let (first, _, _) = relay(app.clone(), signed_reclaim("agent.testnet", "0")).await;
    assert_eq!(first, StatusCode::CONFLICT);
    let (status, retry, body) = relay(app.clone(), signed_reclaim("agent.testnet", "0")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry.as_deref(), Some("60"));
    assert_eq!(body["code"], "SPHINX_E_API_RATE_LIMITED");
    assert_eq!(body["retry"], "wait");
    assert_eq!(body["ctx"]["retry_after_secs"], "60");
    let (other, _, _) = relay(app, signed_reclaim("other.testnet", "0")).await;
    assert_eq!(other, StatusCode::CONFLICT, "other accounts are unaffected");
}

#[tokio::test]
async fn a_forged_signature_does_not_use_the_accounts_allowance() {
    let limits = RelayLimits {
        burst: 1,
        per_minute: 1,
        cooldown: Duration::from_secs(60),
        ..RelayLimits::default()
    };
    let app = app(Arc::new(Previews::default()), limits);
    let mut forged = sphinx_near::relay::decode(&signed_reclaim("agent.testnet", "0")).unwrap();
    forged.delegate_action.nonce = 2;
    let (status, _, body) = relay(app.clone(), sphinx_near::relay::encode(&forged)).await;
    assert_eq!(body["code"], "SPHINX_E_API_RELAY_NOT_ALLOWED", "{status}");
    let (status, _, _) = relay(app, signed_reclaim("agent.testnet", "0")).await;
    assert_eq!(status, StatusCode::CONFLICT, "the real agent still has its allowance");
}

#[tokio::test]
async fn a_key_that_is_not_the_senders_is_refused_before_any_limit_or_gas() {
    let limits = RelayLimits { burst: 1, per_minute: 1, ..RelayLimits::default() };
    let previews = Arc::new(Previews::default());
    let app = app(previews.clone(), limits);
    for _ in 0..3 {
        let (status, _, body) = relay(app.clone(), signed_reclaim("victim.testnet", "1")).await;
        assert_eq!(body["code"], "SPHINX_E_API_RELAY_NOT_ALLOWED", "{status}");
        assert!(body["message"]
            .as_str()
            .unwrap()
            .contains("is not an access key of victim.testnet"));
    }
    assert!(previews.seen.lock().unwrap().is_empty(), "nothing was previewed or sent");
}

#[tokio::test]
async fn a_call_that_attaches_a_deposit_is_never_relayed() {
    let previews = Arc::new(Previews::default());
    let app = app(previews.clone(), RelayLimits::default());
    let submit = signed_call(
        "agent.testnet",
        "submit",
        &json!({ "job_id": "1", "code_uri": "x", "code_hash": "ab".repeat(32) }),
        20,
    );
    let (_, _, body) = relay(app, submit).await;
    assert_eq!(body["code"], "SPHINX_E_API_RELAY_NOT_ALLOWED");
    assert!(body["message"].as_str().unwrap().contains("the relay would pay it"));
    assert!(previews.seen.lock().unwrap().is_empty(), "nothing was previewed or sent");
}
