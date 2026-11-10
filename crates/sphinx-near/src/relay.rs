//! Gasless submission (NEP-366 meta-transactions).
//!
//! An agent signs a delegate action with its own key; Sphinx wraps it in a
//! transaction it pays gas for. Only calls to the escrow's agent-facing methods
//! are relayed, and the agent's signature is verified before any gas is spent.
//!
//! Only calls that attach no deposit are relayed. NEAR charges a delegate
//! action's attached deposits to the relayer that sends it, not to the
//! sender, so a relayed bond or reward would be paid by the relay and could
//! be reclaimed by the sender. Calls that lock NEAR (posting a job,
//! submitting, a first registration) are sent by the user directly.

use base64::Engine as _;
use near_api::types::signable_message::{SignableMessage, SignableMessageType};
use near_api::types::transaction::actions::Action;
use near_api::types::transaction::actions::{AccessKey, AccessKeyPermission};
use near_api::types::transaction::delegate_action::SignedDelegateAction;
use near_api::types::{AccountId, CryptoHash, NearGas};
use near_api::{Account, Signer, Transaction};
use sphinx_protocol::{Code, SphinxError};
use std::sync::Arc;

use near_api::types::transaction::result::ValueOrReceiptId;

use crate::{CallOutcome, Escrow};

/// Escrow methods a relayer will pay gas for.
pub const RELAYABLE_METHODS: &[&str] = &[
    "post_job",
    "commit_suite",
    "cancel_job",
    "submit",
    "reclaim_bond",
    "refund_on_timeout",
    "settle",
    "withdraw",
    "register_agent",
    "unregister_agent",
];

/// Most gas one relayed action may ask for.
pub const MAX_ACTION_GAS: NearGas = NearGas::from_tgas(100);

/// Decodes a base64 Borsh `SignedDelegateAction`.
///
/// # Errors
/// `SPHINX_E_API_BAD_REQUEST` if the text is not one.
pub fn decode(b64: &str) -> Result<SignedDelegateAction, SphinxError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| bad(&format!("not base64: {e}")))?;
    borsh::from_slice(&bytes).map_err(|e| bad(&format!("not a Borsh SignedDelegateAction: {e}")))
}

/// Encodes a `SignedDelegateAction` for transport.
pub fn encode(sda: &SignedDelegateAction) -> String {
    base64::engine::general_purpose::STANDARD.encode(borsh::to_vec(sda).unwrap_or_default())
}

/// Checks that `sda` is a correctly signed call to an agent-facing escrow method.
///
/// # Errors
/// `SPHINX_E_API_RELAY_NOT_ALLOWED` naming the rule that failed.
pub fn check(
    sda: &SignedDelegateAction,
    escrow: &AccountId,
    relayer: &AccountId,
) -> Result<(), SphinxError> {
    let da = &sda.delegate_action;
    if &da.receiver_id != escrow {
        return Err(not_allowed(&format!(
            "receiver is {}; only {escrow} is relayed",
            da.receiver_id
        )));
    }
    if &da.sender_id == relayer {
        return Err(not_allowed("the relayer does not relay its own actions"));
    }
    if da.actions.is_empty() || da.actions.len() > 2 {
        return Err(not_allowed(&format!("{} actions; 1 or 2 are allowed", da.actions.len())));
    }
    for action in &da.actions {
        let Action::FunctionCall(call) = &**action else {
            return Err(not_allowed("only function calls are relayed"));
        };
        if !RELAYABLE_METHODS.contains(&call.method_name.as_str()) {
            return Err(not_allowed(&format!("method {} is not relayed", call.method_name)));
        }
        if call.args.len() > MAX_ARGS_BYTES {
            return Err(not_allowed(&format!(
                "an action carries {} bytes of arguments; the maximum is {MAX_ARGS_BYTES}",
                call.args.len()
            )));
        }
        if call.deposit.as_yoctonear() > 0 {
            return Err(not_allowed(&format!(
                "{} attaches {} yoctoNEAR; the relay would pay it, so calls with a deposit must be sent by the account itself",
                call.method_name,
                call.deposit.as_yoctonear()
            )));
        }
        if call.gas > MAX_ACTION_GAS {
            return Err(not_allowed(&format!(
                "an action asks for {} gas; the maximum is {MAX_ACTION_GAS}",
                call.gas
            )));
        }
    }
    let payload = borsh::to_vec(&SignableMessage::new(da, SignableMessageType::DelegateAction))
        .map_err(|e| bad(&format!("cannot encode the delegate action: {e}")))?;
    if !sda.signature.verify(CryptoHash::hash(&payload), da.public_key) {
        return Err(not_allowed(
            "the delegate action's signature does not verify against its public key",
        ));
    }
    Ok(())
}

/// Largest arguments a relayed call may carry; every escrow method takes a few hundred bytes.
pub const MAX_ARGS_BYTES: usize = 16 * 1024;

/// Checks that the delegate action's key is really an access key of its
/// sender that may make these calls, at a nonce and height the chain will
/// still accept. Without this, anyone could sign with a fresh key, name any
/// sender, and make the relay pay for a call the chain then rejects.
///
/// `key` is the sender's access key for that public key as the chain holds
/// it (`None` when there is none) and `height` the block height it was read at.
///
/// # Errors
/// `SPHINX_E_API_RELAY_NOT_ALLOWED` naming the rule that failed.
pub fn check_key(
    sda: &SignedDelegateAction,
    escrow: &AccountId,
    key: Option<&AccessKey>,
    height: u64,
) -> Result<(), SphinxError> {
    let da = &sda.delegate_action;
    let key = key.ok_or_else(|| {
        not_allowed(&format!("{} is not an access key of {}", da.public_key, da.sender_id))
    })?;
    if da.nonce <= key.nonce.0 {
        return Err(not_allowed(&format!(
            "nonce {} was already used; the key is at {}",
            da.nonce, key.nonce.0
        )));
    }
    if da.max_block_height <= height {
        return Err(not_allowed(&format!(
            "the delegate action expired at block {}; the chain is at {height}",
            da.max_block_height
        )));
    }
    if let AccessKeyPermission::FunctionCall(p) = &key.permission {
        if p.receiver_id != escrow.as_str() {
            return Err(not_allowed(&format!("the key may only call {}", p.receiver_id)));
        }
        for action in &da.actions {
            if let Action::FunctionCall(call) = &**action {
                if !p.method_names.is_empty() && !p.method_names.contains(&call.method_name) {
                    return Err(not_allowed(&format!("the key may not call {}", call.method_name)));
                }
                if call.deposit.as_yoctonear() > 0 {
                    return Err(not_allowed(
                        "a function-call key cannot attach a deposit; sign with a full-access key",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The sender's access key for the delegate action's public key and the
/// block height it was read at; `None` when the account has no such key.
///
/// # Errors
/// `SPHINX_E_RELAY_RPC_FAILED` when the chain cannot be read.
pub async fn access_key(
    escrow: &Escrow,
    sda: &SignedDelegateAction,
) -> Result<(Option<AccessKey>, u64), SphinxError> {
    let da = &sda.delegate_action;
    match Account(da.sender_id.clone()).access_key(da.public_key).fetch_from(escrow.network()).await
    {
        Ok(found) => Ok((Some(found.data), found.block_height)),
        Err(e) => {
            let text = e.to_string();
            if text.contains("UnknownAccessKey")
                || text.contains("UnknownAccount")
                || text.contains("does not exist")
            {
                Ok((None, 0))
            } else {
                Err(SphinxError::new(
                    Code::RelayRpcFailed,
                    format!("view_access_key failed: {text}"),
                ))
            }
        }
    }
}

/// A read-only escrow view that previews one relayed call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    /// The view to call, e.g. `check_submit`.
    pub view: &'static str,
    pub args: serde_json::Value,
}

/// The escrow views that preview the calls in `sda`, so a relay can refuse a
/// call the contract would refuse before paying gas for it. Methods without a
/// preview view are not listed.
///
/// # Errors
/// `SPHINX_E_API_BAD_REQUEST` if a previewed call's arguments are not the
/// JSON object the method takes.
pub fn preflight(sda: &SignedDelegateAction) -> Result<Vec<Preflight>, SphinxError> {
    let sender = &sda.delegate_action.sender_id;
    let mut checks = Vec::new();
    for action in &sda.delegate_action.actions {
        if let Action::FunctionCall(call) = &**action {
            checks.extend(preflight_call(
                &call.method_name,
                &call.args,
                sender,
                call.deposit.as_yoctonear(),
            )?);
        }
    }
    Ok(checks)
}

/// The escrow view that previews one call of `method` by `sender`, if any.
///
/// # Errors
/// `SPHINX_E_API_BAD_REQUEST` if `args` is not the JSON object the method takes.
pub fn preflight_call(
    method: &str,
    args: &[u8],
    sender: &AccountId,
    deposit_yocto: u128,
) -> Result<Option<Preflight>, SphinxError> {
    let view = match method {
        "submit" => "check_submit",
        "reclaim_bond" => "check_reclaim_bond",
        _ => return Ok(None),
    };
    let serde_json::Value::Object(mut map) = serde_json::from_slice(args)
        .map_err(|e| bad(&format!("{method} arguments are not JSON: {e}")))?
    else {
        return Err(bad(&format!("{method} arguments are not a JSON object")));
    };
    if view == "check_submit" {
        map.insert("agent".into(), sender.to_string().into());
        map.insert("deposit".into(), deposit_yocto.to_string().into());
    } else {
        map.insert("caller".into(), sender.to_string().into());
    }
    Ok(Some(Preflight { view, args: serde_json::Value::Object(map) }))
}

/// Wraps `sda` in a transaction signed and paid for by `relayer`.
///
/// # Errors
/// The escrow's own `SPHINX_E_*` error if the inner call fails, otherwise
/// `SPHINX_E_RELAY_RPC_FAILED`.
pub async fn send(
    escrow: &Escrow,
    relayer: AccountId,
    signer: Arc<Signer>,
    sda: SignedDelegateAction,
) -> Result<CallOutcome, SphinxError> {
    let sender = sda.delegate_action.sender_id.clone();
    let result = Transaction::construct(relayer, sender)
        .add_action(Action::Delegate(Box::new(sda)))
        .with_signer(signer)
        .send_to(escrow.network())
        .await
        .map_err(|e| crate::decode_failure("relay", &e.to_string()))?;
    relayed_outcome(escrow.contract_id(), result)
}

/// Reads the outcome of a relayed call.
///
/// The relayer's own transaction succeeds as soon as the delegate action is
/// delivered, so its final status says nothing about the agent's call. This
/// checks every receipt the transaction produced: the first failure is returned
/// as its catalog error, and on success the value is the escrow call's own
/// return value.
fn relayed_outcome(
    contract: &AccountId,
    result: near_api::types::transaction::result::TransactionResult,
) -> Result<CallOutcome, SphinxError> {
    let full = result.into_full().ok_or_else(|| {
        SphinxError::new(Code::RelayRpcFailed, "relay did not reach a final outcome")
            .ctx("method", "relay")
    })?;
    let tx_hash =
        full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
    if let Some(failed) = full.failures().first() {
        return Err(crate::decode_failure("relay", &format!("{failed:?}")).ctx("tx_hash", tx_hash));
    }
    let value = full.receipt_outcomes().iter().find(|o| &o.executor_id == contract).and_then(|o| {
        match o.clone().into_result() {
            Ok(ValueOrReceiptId::Value(v)) => v.json::<serde_json::Value>().ok(),
            _ => None,
        }
    });
    Ok(CallOutcome { tx_hash, value })
}

fn bad(detail: &str) -> SphinxError {
    SphinxError::new(Code::ApiBadRequest, format!("signed_delegate_action is invalid: {detail}"))
}

fn not_allowed(detail: &str) -> SphinxError {
    SphinxError::new(Code::ApiRelayNotAllowed, format!("not relayed: {detail}"))
}
