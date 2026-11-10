//! Who signs grader results.
//!
//! - `outlayer`: the ed25519 key OutLayer derives inside the TDX enclave for
//!   this exact wasm build and caller (`manifest.json`, `bind: wasm`).
//! - `dev`: a local key from `SPHINX_DEV_GRADER_SEED`, for development only.
//!   The contract records which kind a job's key is, and clients label DEV jobs.
//! - none: results are returned unsigned (local checks).
//!
//! Each signer also holds the grader's X25519 box key, which submissions are
//! encrypted to (`sphinx_box`). It is derived from a key-bound secret and never
//! leaves the process.

use serde::Serialize;
use sphinx_box::BoxSecret;
use sphinx_protocol::SphinxError;

/// A signer's identity, reported in every response.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SignerInfo {
    pub kind: &'static str,
    pub public_key: String,
}

pub trait GraderSigner {
    /// The signer's kind and public key.
    ///
    /// # Errors
    /// `SPHINX_E_GRADER_SIGNER_UNAVAILABLE` if the key cannot be read.
    fn info(&self) -> Result<SignerInfo, SphinxError>;

    /// Signs bytes the grader composed itself; never caller-supplied bytes.
    ///
    /// # Errors
    /// `SPHINX_E_GRADER_SIGNER_UNAVAILABLE` if the key cannot sign or the
    /// message is outside the Sphinx signing domain.
    fn sign(&self, message: &str) -> Result<String, SphinxError>;

    /// The box key submissions are encrypted to.
    ///
    /// # Errors
    /// `SPHINX_E_GRADER_SIGNER_UNAVAILABLE` if this signer has no box key.
    fn box_secret(&self) -> Result<BoxSecret, SphinxError> {
        Err(SphinxError::new(
            sphinx_protocol::Code::GraderSignerUnavailable,
            "this signer has no box key",
        ))
    }
}

/// Returns the signer this build was compiled with, if any.
pub fn configured() -> Option<Box<dyn GraderSigner>> {
    #[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
    {
        return Some(Box::new(outlayer::OutLayerSigner));
    }
    #[cfg(all(feature = "dev-signer", not(all(feature = "outlayer", target_arch = "wasm32"))))]
    {
        return dev::DevSigner::from_env().map(|s| Box::new(s) as Box<dyn GraderSigner>);
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(any(feature = "dev-signer", all(feature = "outlayer", target_arch = "wasm32")))]
fn unavailable(reason: &str) -> SphinxError {
    SphinxError::new(
        sphinx_protocol::Code::GraderSignerUnavailable,
        format!("signing key is unavailable: {reason}"),
    )
}

#[cfg(feature = "dev-signer")]
pub mod dev {
    use super::{unavailable, BoxSecret, GraderSigner, SignerInfo};
    use ed25519_dalek::{Signer, SigningKey};
    use sphinx_protocol::SphinxError;

    pub const SEED_VAR: &str = "SPHINX_DEV_GRADER_SEED";

    #[derive(Debug)]
    pub struct DevSigner(SigningKey);

    impl DevSigner {
        pub fn from_seed_hex(seed: &str) -> Option<Self> {
            let bytes: [u8; 32] = hex::decode(seed.trim()).ok()?.try_into().ok()?;
            Some(Self(SigningKey::from_bytes(&bytes)))
        }

        pub fn from_env() -> Option<Self> {
            std::env::var(SEED_VAR).ok().and_then(|s| Self::from_seed_hex(&s))
        }
    }

    impl GraderSigner for DevSigner {
        fn info(&self) -> Result<SignerInfo, SphinxError> {
            Ok(SignerInfo {
                kind: "dev",
                public_key: hex::encode(self.0.verifying_key().to_bytes()),
            })
        }

        fn sign(&self, message: &str) -> Result<String, SphinxError> {
            if !message.starts_with(sphinx_protocol::message::DOMAIN) {
                return Err(unavailable("refusing to sign a message outside the sphinx domain"));
            }
            Ok(hex::encode(self.0.sign(message.as_bytes()).to_bytes()))
        }

        /// HKDF of HMAC-SHA256(seed, the box KDF message): the enclave's
        /// derivation with the seed in place of the host's key-bound MAC.
        fn box_secret(&self) -> Result<BoxSecret, SphinxError> {
            Ok(BoxSecret::derive(&BoxSecret::dev_ikm(&self.0.to_bytes())))
        }
    }
}

#[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
mod outlayer {
    use super::{unavailable, BoxSecret, GraderSigner, SignerInfo};
    use crate::host::{encryption_keys as enc, signing_keys as keys};
    use sphinx_protocol::SphinxError;

    const KEY_PATH: &str = "grader";
    const BOX_PATH: &str = "box";

    #[derive(Debug)]
    pub struct OutLayerSigner;

    impl GraderSigner for OutLayerSigner {
        fn info(&self) -> Result<SignerInfo, SphinxError> {
            let pk = keys::public_key(KEY_PATH, None).map_err(|e| unavailable(&e))?;
            Ok(SignerInfo { kind: "outlayer", public_key: hex::encode(pk) })
        }

        fn sign(&self, message: &str) -> Result<String, SphinxError> {
            if !message.starts_with(sphinx_protocol::message::DOMAIN) {
                return Err(unavailable("refusing to sign a message outside the sphinx domain"));
            }
            let sig =
                keys::sign(KEY_PATH, None, message.as_bytes()).map_err(|e| unavailable(&e))?;
            Ok(hex::encode(sig))
        }

        /// The host's MAC is HMAC-SHA256 under a key bound to this build and
        /// caller, and deterministic, so every run derives the same box key.
        fn box_secret(&self) -> Result<BoxSecret, SphinxError> {
            let ikm = enc::mac(BOX_PATH, None, sphinx_box::KDF_MESSAGE)
                .map_err(|e| unavailable(&format!("box key: {e}")))?;
            Ok(BoxSecret::derive(&ikm))
        }
    }
}
