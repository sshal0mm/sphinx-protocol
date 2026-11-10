//! The relay only pays gas for correctly signed calls to agent methods.
#![allow(clippy::unwrap_used, reason = "test")]

use ed25519_dalek::{Signer as _, SigningKey};
use near_api::types::crypto::public_key::ED25519PublicKey;
use near_api::types::signable_message::{SignableMessage, SignableMessageType};
use near_api::types::transaction::actions::{Action, FunctionCallAction, TransferAction};
use near_api::types::transaction::delegate_action::{
    DelegateAction, NonDelegateAction, SignedDelegateAction,
};
use near_api::types::{AccountId, CryptoHash, NearGas, NearToken, PublicKey, Signature};
use sphinx_near::relay::{check, decode, encode, preflight, Preflight};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn call(method: &str, gas_tgas: u64) -> NonDelegateAction {
    NonDelegateAction::try_from(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: method.into(),
        args: br#"{"job_id":"1"}"#.to_vec(),
        gas: NearGas::from_tgas(gas_tgas),
        deposit: NearToken::from_yoctonear(0),
    })))
    .unwrap()
}

fn signed(
    receiver: &str,
    actions: Vec<NonDelegateAction>,
    key: &SigningKey,
) -> SignedDelegateAction {
    let da = DelegateAction {
        sender_id: acct("agent.testnet"),
        receiver_id: acct(receiver),
        actions,
        nonce: 1,
        max_block_height: 1_000,
        public_key: PublicKey::ED25519(ED25519PublicKey(key.verifying_key().to_bytes())),
    };
    let payload =
        borsh::to_vec(&SignableMessage::new(&da, SignableMessageType::DelegateAction)).unwrap();
    let sig = key.sign(&CryptoHash::hash(&payload).0);
    SignedDelegateAction { delegate_action: da, signature: Signature::ED25519(sig) }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[3u8; 32])
}

fn code_of(sda: &SignedDelegateAction) -> String {
    check(sda, &acct("escrow.testnet"), &acct("relayer.testnet"))
        .unwrap_err()
        .code()
        .as_str()
        .to_owned()
}

#[test]
fn a_signed_submit_is_relayable_and_round_trips() {
    let sda = signed("escrow.testnet", vec![call("submit", 50)], &key());
    check(&sda, &acct("escrow.testnet"), &acct("relayer.testnet")).unwrap();
    let back = decode(&encode(&sda)).unwrap();
    check(&back, &acct("escrow.testnet"), &acct("relayer.testnet")).unwrap();
}

#[test]
fn directory_registration_is_relayable_but_owner_upkeep_is_not() {
    let k = key();
    for method in ["register_agent", "unregister_agent"] {
        let sda = signed("escrow.testnet", vec![call(method, 30)], &k);
        check(&sda, &acct("escrow.testnet"), &acct("relayer.testnet")).unwrap();
    }
    assert_eq!(
        code_of(&signed("escrow.testnet", vec![call("backfill_agent_stats", 30)], &k)),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
}

#[test]
fn a_deposit_is_never_relayed_because_the_relay_would_pay_it() {
    let paid = NonDelegateAction::try_from(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: "submit".into(),
        args: br#"{"job_id":"1"}"#.to_vec(),
        gas: NearGas::from_tgas(50),
        deposit: NearToken::from_yoctonear(1),
    })))
    .unwrap();
    assert_eq!(
        code_of(&signed("escrow.testnet", vec![paid], &key())),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
}

#[test]
fn everything_else_is_refused() {
    let k = key();
    assert_eq!(
        code_of(&signed("other.testnet", vec![call("submit", 50)], &k)),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
    assert_eq!(
        code_of(&signed("escrow.testnet", vec![call("set_grader_key", 50)], &k)),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
    assert_eq!(
        code_of(&signed("escrow.testnet", vec![call("submit", 300)], &k)),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
    assert_eq!(code_of(&signed("escrow.testnet", vec![], &k)), "SPHINX_E_API_RELAY_NOT_ALLOWED");
    let transfer = NonDelegateAction::try_from(Action::Transfer(TransferAction {
        deposit: NearToken::from_near(1),
    }))
    .unwrap();
    assert_eq!(
        code_of(&signed("escrow.testnet", vec![transfer], &k)),
        "SPHINX_E_API_RELAY_NOT_ALLOWED"
    );
}

#[test]
fn tampering_breaks_the_signature() {
    let mut sda = signed("escrow.testnet", vec![call("submit", 50)], &key());
    sda.delegate_action.nonce = 2;
    let err = check(&sda, &acct("escrow.testnet"), &acct("relayer.testnet")).unwrap_err();
    assert!(err.message().contains("signature"));
    let mut forged = signed("escrow.testnet", vec![call("submit", 50)], &key());
    let other = SigningKey::from_bytes(&[9u8; 32]);
    forged.delegate_action.public_key =
        PublicKey::ED25519(ED25519PublicKey(other.verifying_key().to_bytes()));
    assert_eq!(code_of(&forged), "SPHINX_E_API_RELAY_NOT_ALLOWED");
}

#[test]
fn garbage_is_a_bad_request() {
    assert_eq!(decode("not base64!!").unwrap_err().code().as_str(), "SPHINX_E_API_BAD_REQUEST");
    assert_eq!(decode("aGVsbG8=").unwrap_err().code().as_str(), "SPHINX_E_API_BAD_REQUEST");
}

#[test]
fn submit_and_reclaim_are_previewed_with_the_real_sender_and_deposit() {
    let reclaim = NonDelegateAction::try_from(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: "reclaim_bond".into(),
        args: br#"{"job_id":"1","sub_id":0}"#.to_vec(),
        gas: NearGas::from_tgas(30),
        deposit: NearToken::from_yoctonear(0),
    })))
    .unwrap();
    let sda = signed("escrow.testnet", vec![call("submit", 50), reclaim], &key());
    assert_eq!(
        preflight(&sda).unwrap(),
        vec![
            Preflight {
                view: "check_submit",
                args: serde_json::json!({
                    "job_id": "1", "agent": "agent.testnet", "deposit": "0"
                }),
            },
            Preflight {
                view: "check_reclaim_bond",
                args: serde_json::json!({ "job_id": "1", "sub_id": 0, "caller": "agent.testnet" }),
            },
        ]
    );
}

#[test]
fn methods_without_a_preview_are_skipped_and_bad_args_are_refused() {
    let sda = signed("escrow.testnet", vec![call("withdraw", 30)], &key());
    assert_eq!(preflight(&sda).unwrap(), vec![]);
    let bad = NonDelegateAction::try_from(Action::FunctionCall(Box::new(FunctionCallAction {
        method_name: "submit".into(),
        args: b"[1,2]".to_vec(),
        gas: NearGas::from_tgas(30),
        deposit: NearToken::from_yoctonear(0),
    })))
    .unwrap();
    let sda = signed("escrow.testnet", vec![bad], &key());
    assert_eq!(preflight(&sda).unwrap_err().code().as_str(), "SPHINX_E_API_BAD_REQUEST");
}

mod keys {
    use super::*;
    use sphinx_near::near_api::types::json::U64;
    use sphinx_near::near_api::types::transaction::actions::{
        AccessKey, AccessKeyPermission, FunctionCallPermission,
    };
    use sphinx_near::relay::check_key;

    fn key(nonce: u64, permission: AccessKeyPermission) -> AccessKey {
        AccessKey { nonce: U64(nonce), permission }
    }

    fn code(r: Result<(), sphinx_protocol::SphinxError>) -> String {
        r.unwrap_err().to_report().code
    }

    #[test]
    fn only_a_live_key_of_the_sender_that_may_make_the_call_is_relayed() {
        let sda = signed("escrow.testnet", vec![call("submit", 50)], &super::key());
        let escrow = acct("escrow.testnet");
        let full = key(0, AccessKeyPermission::FullAccess);
        check_key(&sda, &escrow, Some(&full), 10).unwrap();
        assert_eq!(code(check_key(&sda, &escrow, None, 10)), "SPHINX_E_API_RELAY_NOT_ALLOWED");
        let used = key(sda.delegate_action.nonce, AccessKeyPermission::FullAccess);
        assert_eq!(
            code(check_key(&sda, &escrow, Some(&used), 10)),
            "SPHINX_E_API_RELAY_NOT_ALLOWED"
        );
        let expired = sda.delegate_action.max_block_height;
        assert_eq!(
            code(check_key(&sda, &escrow, Some(&full), expired)),
            "SPHINX_E_API_RELAY_NOT_ALLOWED"
        );
        let elsewhere = key(
            0,
            AccessKeyPermission::FunctionCall(FunctionCallPermission {
                allowance: None,
                receiver_id: "other.testnet".into(),
                method_names: vec![],
            }),
        );
        assert_eq!(
            code(check_key(&sda, &escrow, Some(&elsewhere), 10)),
            "SPHINX_E_API_RELAY_NOT_ALLOWED"
        );
        let narrow = key(
            0,
            AccessKeyPermission::FunctionCall(FunctionCallPermission {
                allowance: None,
                receiver_id: "escrow.testnet".into(),
                method_names: vec!["reclaim_bond".into()],
            }),
        );
        assert_eq!(
            code(check_key(&sda, &escrow, Some(&narrow), 10)),
            "SPHINX_E_API_RELAY_NOT_ALLOWED"
        );
    }
}
