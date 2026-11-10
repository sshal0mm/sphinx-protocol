//! NEP-413 signed messages, used to prove which account is asking the job API
//! for code it may read (its own submissions, or a solution it paid for).
//!
//! The header `x-sphinx-auth` carries base64 of the JSON
//! `{account_id, public_key, signature, message, nonce, recipient}`, where
//! `message` is `sphinx:code:<code hash>:<unix time in ms>`, `nonce` is 32
//! random bytes in base64, and `recipient` is [`RECIPIENT`]. Wallets that
//! support NEP-413 `signMessage` produce exactly this signature, so a browser
//! can ask too.

use base64::Engine as _;
use borsh::BorshSerialize;
use near_api::types::crypto::public_key::PublicKey;
use near_api::types::crypto::secret_key::SecretKey;
use near_api::types::crypto::signature::Signature;
use near_api::types::CryptoHash;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The header name.
pub const HEADER: &str = "x-sphinx-auth";
/// Who the messages are addressed to.
pub const RECIPIENT: &str = "sphinx-api";
/// How old a message may be when it arrives.
pub const MAX_AGE_MS: u64 = 5 * 60 * 1000;
/// NEP-413's prefix: 2^31 + 413, so a signed message can never be a transaction.
const TAG: u32 = (1 << 31) + 413;

#[derive(BorshSerialize)]
struct Payload<'a> {
    message: &'a str,
    nonce: [u8; 32],
    recipient: &'a str,
    callback_url: Option<String>,
}

/// The hash a NEP-413 signature covers.
pub fn payload_hash(message: &str, nonce: [u8; 32], recipient: &str) -> CryptoHash {
    let mut bytes = borsh::to_vec(&TAG).unwrap_or_default();
    bytes.extend(
        borsh::to_vec(&Payload { message, nonce, recipient, callback_url: None })
            .unwrap_or_default(),
    );
    CryptoHash(Sha256::digest(&bytes).into())
}

/// A signed message as the header carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage {
    pub account_id: String,
    pub public_key: String,
    pub signature: String,
    pub message: String,
    /// 32 bytes, base64.
    pub nonce: String,
    pub recipient: String,
}

/// The message for reading code with hash `code_hash` at `now_ms`.
pub fn code_message(code_hash: &str, now_ms: u64) -> String {
    format!("sphinx:code:{code_hash}:{now_ms}")
}

/// Signs `message` as `account` and returns the header value.
pub fn sign_header(account: &str, key: &SecretKey, message: &str, nonce: [u8; 32]) -> String {
    let signature = key.sign(payload_hash(message, nonce, RECIPIENT));
    let signed = SignedMessage {
        account_id: account.to_owned(),
        public_key: key.public_key().to_string(),
        signature: signature.to_string(),
        message: message.to_owned(),
        nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
        recipient: RECIPIENT.to_owned(),
    };
    base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&signed).unwrap_or_default())
}

/// A signature as NEAR tools write it (`ed25519:<base58>`) or as wallets'
/// `signMessage` returns it (base64 of the 64 bytes).
fn parse_signature(text: &str) -> Option<Signature> {
    if let Ok(signature) = text.parse::<Signature>() {
        return Some(signature);
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(text).ok()?;
    Signature::from_parts(near_api::types::crypto::KeyType::ED25519, &bytes).ok()
}

/// Why a header was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Malformed,
    WrongRecipient,
    BadSignature,
    WrongMessage,
    Stale,
}

/// Parses and checks a header: well formed, addressed to [`RECIPIENT`],
/// signed by its public key, asking for `code_hash`, and fresh at `now_ms`.
/// Returns the message; the caller still checks that the public key is one of
/// the account's access keys.
///
/// # Errors
/// Why the header was refused.
pub fn verify_code_header(
    value: &str,
    code_hash: &str,
    now_ms: u64,
) -> Result<SignedMessage, Refused> {
    let raw =
        base64::engine::general_purpose::STANDARD.decode(value).map_err(|_| Refused::Malformed)?;
    let m: SignedMessage = serde_json::from_slice(&raw).map_err(|_| Refused::Malformed)?;
    if m.recipient != RECIPIENT {
        return Err(Refused::WrongRecipient);
    }
    let nonce: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(&m.nonce)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(Refused::Malformed)?;
    let key: PublicKey = m.public_key.parse().map_err(|_| Refused::Malformed)?;
    let signature = parse_signature(&m.signature).ok_or(Refused::Malformed)?;
    if !signature.verify(payload_hash(&m.message, nonce, &m.recipient), key) {
        return Err(Refused::BadSignature);
    }
    let rest = m
        .message
        .strip_prefix("sphinx:code:")
        .and_then(|r| r.split_once(':'))
        .ok_or(Refused::WrongMessage)?;
    if rest.0 != code_hash {
        return Err(Refused::WrongMessage);
    }
    let at: u64 = rest.1.parse().map_err(|_| Refused::WrongMessage)?;
    if at > now_ms.saturating_add(60_000) || now_ms.saturating_sub(at) > MAX_AGE_MS {
        return Err(Refused::Stale);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    fn key() -> SecretKey {
        "ed25519:3D4YudUahN1nawWogh8pAKSj92sUNMdbZGjn7kERKzYoTy8tnFQuwoGUC51DowKqorvkr2pytJSnwuSbsNVfqygr"
            .parse()
            .unwrap()
    }

    #[test]
    fn a_signed_header_verifies_and_every_change_is_refused() {
        let hash = "ab".repeat(32);
        let now = 1_800_000_000_000;
        let h = sign_header("agent.testnet", &key(), &code_message(&hash, now), [7; 32]);
        let m = verify_code_header(&h, &hash, now + 1_000).unwrap();
        assert_eq!(m.account_id, "agent.testnet");
        assert_eq!(verify_code_header(&h, &"cd".repeat(32), now), Err(Refused::WrongMessage));
        assert_eq!(verify_code_header(&h, &hash, now + MAX_AGE_MS + 1), Err(Refused::Stale));
        assert_eq!(verify_code_header("not base64!", &hash, now), Err(Refused::Malformed));
        // A different message under the same signature fails.
        let mut forged: SignedMessage =
            serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(&h).unwrap())
                .unwrap();
        forged.message = code_message(&hash, now + 5);
        let forged =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(verify_code_header(&forged, &hash, now), Err(Refused::BadSignature));
    }

    #[test]
    fn a_wallet_signature_in_base64_is_accepted() {
        let hash = "ab".repeat(32);
        let now = 1_800_000_000_000;
        let h = sign_header("agent.testnet", &key(), &code_message(&hash, now), [7; 32]);
        let mut m: SignedMessage =
            serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(&h).unwrap())
                .unwrap();
        let Signature::ED25519(raw) = m.signature.parse::<Signature>().unwrap() else {
            panic!("ed25519")
        };
        m.signature = base64::engine::general_purpose::STANDARD.encode(raw.to_bytes());
        let h = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&m).unwrap());
        assert_eq!(verify_code_header(&h, &hash, now).unwrap().account_id, "agent.testnet");
    }

    #[test]
    fn the_payload_hash_follows_nep413() {
        // The tag is 2^31 + 413, little endian, ahead of the borsh payload.
        let mut expected = 2_147_484_061u32.to_le_bytes().to_vec();
        expected.extend(
            borsh::to_vec(&Payload {
                message: "m",
                nonce: [0; 32],
                recipient: "r",
                callback_url: None,
            })
            .unwrap(),
        );
        let digest: [u8; 32] = Sha256::digest(&expected).into();
        assert_eq!(payload_hash("m", [0; 32], "r"), CryptoHash(digest));
    }
}
