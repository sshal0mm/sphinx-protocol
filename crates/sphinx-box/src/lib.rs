//! Encrypted boxes: encrypt to an X25519 public key so only its holder can read.
//!
//! One construction, used twice:
//!
//! - hidden suites, encrypted to OutLayer's keystore (`info = "outlayer-keystore-v1"`);
//! - submissions, encrypted to the grader's box key (`info = "sphinx:v1:box:submission"`).
//!
//! Wire format: `base64( 0x01 | ephemeral X25519 public key (32) | nonce (12) | ciphertext | tag (16) )`,
//! key = HKDF-SHA256(salt = none, ikm = X25519(ephemeral, recipient), info), AEAD
//! ChaCha20-Poly1305 with no associated data.
//!
//! The grader's box key is derived inside the enclave
//! ([`BoxSecret::derive`]): secret = HKDF-SHA256(ikm, info = "sphinx box v1"),
//! where `ikm` is a deterministic key-bound MAC of [`KDF_MESSAGE`]. The secret
//! never leaves the process that derived it.

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// HKDF info of hidden suites encrypted to OutLayer's keystore.
pub const KEYSTORE_INFO: &[u8] = b"outlayer-keystore-v1";
/// HKDF info of submissions encrypted to the grader's box key.
pub const SUBMISSION_INFO: &[u8] = b"sphinx:v1:box:submission";
/// The message whose key-bound MAC seeds the grader's box key.
pub const KDF_MESSAGE: &[u8] = b"sphinx:v1:kdf:box-v1";
/// HKDF info that turns that MAC into the box secret.
pub const BOX_KEY_INFO: &[u8] = b"sphinx box v1";

const VERSION_BYTE: u8 = 1;
const HEADER: usize = 1 + 32 + 12;
const TAG: usize = 16;

/// Why a box could not be made or opened. Deliberately coarse: a failure to
/// open never says whether the key, the bytes or the format was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxError {
    /// The public key is not 32 bytes of lowercase or uppercase hex.
    BadPublicKey,
    /// The box is not base64 of a version-1 box.
    Malformed,
    /// The box does not open under this key.
    Open,
}

impl std::fmt::Display for BoxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadPublicKey => "the public key is not 32 hex bytes",
            Self::Malformed => "the box is not base64 of a version-1 box",
            Self::Open => "the box does not open with this key",
        })
    }
}

impl std::error::Error for BoxError {}

/// A box key's secret half. Zeroed when dropped; never serialised.
pub struct BoxSecret(StaticSecret);

impl std::fmt::Debug for BoxSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BoxSecret").field(&self.public_key_hex()).finish()
    }
}

impl BoxSecret {
    /// The box secret derived from key material `ikm` (in the enclave, the
    /// host's MAC of [`KDF_MESSAGE`]).
    pub fn derive(ikm: &[u8]) -> Self {
        let mut secret = [0u8; 32];
        // A 32-byte output is always within HKDF-SHA256's limit.
        let _ = Hkdf::<Sha256>::new(None, ikm).expand(BOX_KEY_INFO, &mut secret);
        Self(StaticSecret::from(secret))
    }

    /// Key material for a development box key: HMAC-SHA256(seed, [`KDF_MESSAGE`]),
    /// mirroring the enclave's key-bound MAC.
    pub fn dev_ikm(seed: &[u8]) -> [u8; 32] {
        // HKDF-Extract(salt = seed, ikm = m) is exactly HMAC-SHA256(seed, m).
        let (prk, _) = Hkdf::<Sha256>::extract(Some(seed), KDF_MESSAGE);
        prk.into()
    }

    pub fn public_key(&self) -> [u8; 32] {
        PublicKey::from(&self.0).to_bytes()
    }

    /// The public key as 64 lowercase hex characters, as registered on chain.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key())
    }

    /// Opens a base64 box encrypted to this key under `info`.
    ///
    /// # Errors
    /// [`BoxError::Malformed`] for bytes that are not a version-1 box, and
    /// [`BoxError::Open`] when it does not authenticate under this key.
    pub fn open(&self, box_b64: &str, info: &[u8]) -> Result<Vec<u8>, BoxError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(box_b64.trim())
            .map_err(|_| BoxError::Malformed)?;
        if raw.len() < HEADER + TAG || raw[0] != VERSION_BYTE {
            return Err(BoxError::Malformed);
        }
        let mut eph = [0u8; 32];
        eph.copy_from_slice(&raw[1..33]);
        let shared = self.0.diffie_hellman(&PublicKey::from(eph));
        if !shared.was_contributory() {
            return Err(BoxError::Open);
        }
        let key = aead_key(shared.as_bytes(), info);
        ChaCha20Poly1305::new(&key.into())
            .decrypt(Nonce::from_slice(&raw[33..HEADER]), &raw[HEADER..])
            .map_err(|_| BoxError::Open)
    }
}

fn aead_key(shared: &[u8; 32], info: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    let _ = Hkdf::<Sha256>::new(None, shared).expand(info, &mut key);
    key
}

/// Parses a 32-byte public key from 64 hex characters.
///
/// # Errors
/// [`BoxError::BadPublicKey`] for anything else.
pub fn parse_public_key(hex_str: &str) -> Result<[u8; 32], BoxError> {
    let bytes = hex::decode(hex_str.trim()).map_err(|_| BoxError::BadPublicKey)?;
    bytes.try_into().map_err(|_| BoxError::BadPublicKey)
}

/// Encrypts `plaintext` to `recipient` with caller-chosen ephemeral secret and
/// nonce. Both must be fresh and secret for every box; prefer [`encrypt`].
///
/// # Errors
/// [`BoxError::BadPublicKey`] if `recipient` is not 32 hex bytes or is a
/// low-order point.
pub fn encrypt_with(
    recipient_hex: &str,
    plaintext: &[u8],
    info: &[u8],
    ephemeral: [u8; 32],
    nonce: [u8; 12],
) -> Result<String, BoxError> {
    let pk = PublicKey::from(parse_public_key(recipient_hex)?);
    let eph = StaticSecret::from(ephemeral);
    let eph_pub = PublicKey::from(&eph);
    let shared = eph.diffie_hellman(&pk);
    if !shared.was_contributory() {
        return Err(BoxError::BadPublicKey);
    }
    let key = aead_key(shared.as_bytes(), info);
    let ct = ChaCha20Poly1305::new(&key.into())
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| BoxError::Malformed)?;
    let mut out = Vec::with_capacity(HEADER + ct.len());
    out.push(VERSION_BYTE);
    out.extend_from_slice(eph_pub.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// Encrypts `plaintext` to `recipient` with a fresh ephemeral key and nonce.
///
/// # Errors
/// [`BoxError::BadPublicKey`] if `recipient` is not a usable 32-byte hex key.
#[cfg(feature = "rand")]
pub fn encrypt(recipient_hex: &str, plaintext: &[u8], info: &[u8]) -> Result<String, BoxError> {
    use rand_core::{OsRng, RngCore};
    let mut ephemeral = [0u8; 32];
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut ephemeral);
    OsRng.fill_bytes(&mut nonce);
    encrypt_with(recipient_hex, plaintext, info, ephemeral, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> BoxSecret {
        BoxSecret::derive(&[n; 32])
    }

    fn encrypted(to: &BoxSecret, msg: &[u8]) -> String {
        encrypt_with(&to.public_key_hex(), msg, SUBMISSION_INFO, [9; 32], [3; 12]).unwrap()
    }

    #[test]
    fn a_box_opens_with_its_key() {
        let k = key(1);
        let b = encrypted(&k, b"function solve() { return 1 }");
        assert_eq!(k.open(&b, SUBMISSION_INFO).unwrap(), b"function solve() { return 1 }");
    }

    #[test]
    fn another_key_cannot_open_it() {
        let b = encrypted(&key(1), b"code");
        assert_eq!(key(2).open(&b, SUBMISSION_INFO), Err(BoxError::Open));
    }

    #[test]
    fn another_purpose_cannot_open_it() {
        let k = key(1);
        let b = encrypted(&k, b"code");
        assert_eq!(k.open(&b, KEYSTORE_INFO), Err(BoxError::Open));
    }

    #[test]
    fn a_tampered_box_does_not_open() {
        let k = key(1);
        let b = encrypted(&k, b"function solve() { return 1 }");
        let mut raw = base64::engine::general_purpose::STANDARD.decode(&b).unwrap();
        for i in [1, 34, HEADER, raw.len() - 1] {
            raw[i] ^= 1;
            let t = base64::engine::general_purpose::STANDARD.encode(&raw);
            assert_eq!(k.open(&t, SUBMISSION_INFO), Err(BoxError::Open), "byte {i}");
            raw[i] ^= 1;
        }
        raw[0] = 2;
        let t = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert_eq!(k.open(&t, SUBMISSION_INFO), Err(BoxError::Malformed));
        assert_eq!(k.open("not base64!", SUBMISSION_INFO), Err(BoxError::Malformed));
        assert_eq!(k.open("AQID", SUBMISSION_INFO), Err(BoxError::Malformed));
    }

    #[test]
    fn derivation_is_deterministic_and_keyed() {
        assert_eq!(key(1).public_key(), key(1).public_key());
        assert_ne!(key(1).public_key(), key(2).public_key());
        assert_eq!(BoxSecret::dev_ikm(&[7; 32]), BoxSecret::dev_ikm(&[7; 32]));
        assert_ne!(BoxSecret::dev_ikm(&[7; 32]), BoxSecret::dev_ikm(&[8; 32]));
        assert!(!format!("{:?}", key(1)).contains(&hex::encode([1u8; 32])));
    }

    #[test]
    fn bad_public_keys_are_refused() {
        assert_eq!(
            encrypt_with("zz", b"x", SUBMISSION_INFO, [1; 32], [0; 12]),
            Err(BoxError::BadPublicKey)
        );
        assert_eq!(
            encrypt_with(&"00".repeat(32), b"x", SUBMISSION_INFO, [1; 32], [0; 12]),
            Err(BoxError::BadPublicKey),
            "a low-order point gives no shared secret"
        );
    }

    /// The keystore format: a box encrypted for the keystore opens as the keystore would.
    #[test]
    fn keystore_boxes_use_the_same_layout() {
        let sk = StaticSecret::from([7u8; 32]);
        let pk_hex = hex::encode(PublicKey::from(&sk).as_bytes());
        let b = encrypt_with(&pk_hex, b"{}", KEYSTORE_INFO, [5; 32], [6; 12]).unwrap();
        assert_eq!(BoxSecret(sk).open(&b, KEYSTORE_INFO).unwrap(), b"{}");
    }

    #[cfg(feature = "rand")]
    #[test]
    fn encryption_is_fresh_each_time() {
        let k = key(1);
        let a = encrypt(&k.public_key_hex(), b"x", SUBMISSION_INFO).unwrap();
        let b = encrypt(&k.public_key_hex(), b"x", SUBMISSION_INFO).unwrap();
        assert_ne!(a, b);
        assert_eq!(k.open(&a, SUBMISSION_INFO).unwrap(), b"x");
    }
}
