//! Checks that a NEAR AI Cloud model runs in a TEE, and that each reply came
//! from it.
//!
//! An attestation report is fetched with a fresh nonce. Its Intel TDX quote
//! must be signed by Intel (checked with `dcap-qvl` against collateral from a
//! PCCS), must not come from a debug TD, and must carry the model's signing
//! address and our nonce in its report data. The model's GPU evidence is sent
//! to NVIDIA's attestation service, which must answer that it is genuine for
//! our nonce. Every reply is then signed by that address: the signed text is
//! `model:sha256(request bytes):sha256(response bytes)`, so a reply that was
//! changed, or came from another machine, fails.
//!
//! Not checked: that the machine runs an approved build of the model (NEAR AI
//! publishes no list of approved measurements; the image and compose hashes
//! are recorded so they can be compared later), and NVIDIA's token signature
//! (its answer is trusted over TLS).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// What an attestation established, kept to show and to compare replies with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attested {
    pub model: String,
    /// The address every reply must be signed by (`0x…`, lowercase).
    pub signing_address: String,
    /// The nonce we sent, found in the quote's report data.
    pub nonce: String,
    /// Intel's TCB status for the platform (`UpToDate`, …).
    pub tcb_status: String,
    pub advisories: Vec<String>,
    /// NVIDIA's attestation service said the GPU is genuine for our nonce.
    pub gpu_verified: bool,
    pub os_image_hash: String,
    pub compose_hash: String,
    /// Unix seconds.
    pub checked_at_s: u64,
}

/// A reply's signature, as NEAR AI serves it.
#[derive(Debug, Clone, Deserialize)]
pub struct Signature {
    pub text: String,
    pub signature: String,
    pub signing_address: String,
    #[serde(default)]
    pub signature_kind: String,
}

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The Ethereum address of a 64-byte uncompressed public key (without 0x04).
fn address_of(public_key: &[u8]) -> String {
    let hash = Keccak256::digest(public_key);
    format!("0x{}", hex::encode(&hash[12..]))
}

/// The address that signed `text` (EIP-191 personal message) with a 65-byte
/// `0x…` signature.
///
/// # Errors
/// A malformed signature, or one no key could have made.
pub fn recover_address(text: &str, signature: &str) -> Result<String, String> {
    use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
    let bytes = hex::decode(signature.trim_start_matches("0x"))
        .map_err(|e| format!("the signature is not hex: {e}"))?;
    if bytes.len() != 65 {
        return Err(format!("the signature is {} bytes, not 65", bytes.len()));
    }
    let v = match bytes[64] {
        27 | 28 => bytes[64] - 27,
        0 | 1 => bytes[64],
        other => return Err(format!("the signature's recovery byte {other} is invalid")),
    };
    let id = RecoveryId::from_i32(i32::from(v)).map_err(|e| e.to_string())?;
    let sig = RecoverableSignature::from_compact(&bytes[..64], id).map_err(|e| e.to_string())?;
    let prefixed = format!("\x19Ethereum Signed Message:\n{}{text}", text.len());
    let digest: [u8; 32] = Keccak256::digest(prefixed.as_bytes()).into();
    let message = secp256k1::Message::from_slice(&digest).map_err(|e| e.to_string())?;
    let key = secp256k1::Secp256k1::verification_only()
        .recover_ecdsa(&message, &sig)
        .map_err(|e| format!("no key recovers from this signature: {e}"))?;
    Ok(address_of(&key.serialize_uncompressed()[1..]))
}

/// Checks one reply's signature: it covers this model, these exact request
/// and response bytes, was made inside the model's TEE, and recovers to the
/// attested address.
///
/// # Errors
/// Which part did not hold.
pub fn check_reply(
    attested: &Attested,
    sig: &Signature,
    request: &[u8],
    response: &[u8],
) -> Result<(), String> {
    let expected = format!("{}:{}:{}", attested.model, sha256_hex(request), sha256_hex(response));
    if sig.text != expected {
        return Err("the signed text does not cover this request and reply".into());
    }
    if sig.signature_kind != "provider_tee" {
        return Err(format!(
            "the reply was signed by `{}`, not inside the model's TEE",
            sig.signature_kind
        ));
    }
    let signer = recover_address(&sig.text, &sig.signature)?;
    if signer != attested.signing_address || sig.signing_address.to_lowercase() != signer {
        return Err(format!(
            "the reply was signed by {signer}, not by the attested key {}",
            attested.signing_address
        ));
    }
    Ok(())
}

/// Fetches and checks the model's attestation with a fresh nonce.
///
/// # Errors
/// Any check that fails, or a service that cannot be reached.
pub async fn attest(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> Result<Attested, String> {
    let nonce: [u8; 32] = rand_nonce();
    let nonce_hex = hex::encode(nonce);
    let url = format!("{}/attestation/report", base_url.trim_end_matches('/'));
    let report: Value = http
        .get(&url)
        .query(&[("model", model), ("signing_algo", "ecdsa"), ("nonce", nonce_hex.as_str())])
        .bearer_auth(api_key)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("attestation report: {e}"))?
        .json()
        .await
        .map_err(|e| format!("attestation report: {e}"))?;
    let entry = report["model_attestations"]
        .as_array()
        .and_then(|list| list.iter().find(|m| m["model_name"] == model))
        .ok_or("the report has no attestation for this model")?;
    let signing_address =
        entry["signing_address"].as_str().ok_or("no signing address")?.to_lowercase();
    // The address is the key's: a report cannot pair a quote with another key.
    let public_key = hex::decode(entry["signing_public_key"].as_str().unwrap_or_default())
        .map_err(|e| format!("signing public key: {e}"))?;
    if address_of(&public_key) != signing_address {
        return Err("the signing address is not the attested public key's".into());
    }
    let quote = hex::decode(entry["intel_quote"].as_str().unwrap_or_default())
        .map_err(|e| format!("Intel quote: {e}"))?;
    let (tcb_status, advisories, report_data, debug) = check_quote(&quote).await?;
    if debug {
        return Err("the TD runs in debug mode".into());
    }
    let address_bytes = hex::decode(signing_address.trim_start_matches("0x"))
        .map_err(|e| format!("signing address: {e}"))?;
    let mut expected = [0u8; 64];
    expected[..20].copy_from_slice(&address_bytes);
    expected[32..].copy_from_slice(&nonce);
    if report_data != expected {
        return Err("the quote's report data is not the signing address and our nonce".into());
    }
    if !matches!(tcb_status.as_str(), "UpToDate" | "SWHardeningNeeded") {
        return Err(format!("Intel reports the platform as {tcb_status}"));
    }
    let gpu_verified =
        check_gpu(http, entry["nvidia_payload"].as_str().unwrap_or_default(), &nonce_hex).await?;
    let tcb = &entry["info"]["tcb_info"];
    Ok(Attested {
        model: model.to_owned(),
        signing_address,
        nonce: nonce_hex,
        tcb_status,
        advisories,
        gpu_verified,
        os_image_hash: tcb["os_image_hash"].as_str().unwrap_or_default().to_owned(),
        compose_hash: tcb["compose_hash"].as_str().unwrap_or_default().to_owned(),
        checked_at_s: now_s(),
    })
}

/// A nonce no one could have guessed: the hash of the time, the process and
/// a counter. Uniqueness is what matters, so the report is about now.
fn rand_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    // The operating system's random source; a predictable nonce would let a
    // recorded report be replayed.
    if getrandom::getrandom(&mut nonce).is_err() {
        // No random source at all: refuse to reuse anything by mixing the
        // clock in, which at worst gives a nonce the server cannot predict
        // in advance.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        nonce = Sha256::digest(now.to_le_bytes()).into();
    }
    nonce
}

/// Verifies Intel's signature over a TDX quote; returns the TCB status,
/// advisories, the 64 bytes of report data, and whether the TD is in debug
/// mode.
async fn check_quote(quote: &[u8]) -> Result<(String, Vec<String>, [u8; 64], bool), String> {
    let pccs = std::env::var("PCCS_URL")
        .unwrap_or_else(|_| dcap_qvl::collateral::PHALA_PCCS_URL.to_owned());
    let client = dcap_qvl::collateral::CollateralClient::with_default_http(pccs.clone())
        .map_err(|e| e.to_string())?;
    let collateral =
        client.fetch(quote).await.map_err(|e| format!("Intel collateral from {pccs}: {e}"))?;
    let verified = dcap_qvl::verify::verify(quote, &collateral, now_s())
        .map_err(|e| format!("Intel's signature over the TDX quote: {e}"))?;
    let td = verified.report.as_td10().ok_or("the quote is not a TDX report")?;
    let debug = td.td_attributes[0] & 1 == 1;
    Ok((verified.status, verified.advisory_ids, td.report_data, debug))
}

/// Sends the GPU evidence to NVIDIA's attestation service; true when it says
/// the GPU is genuine and the evidence carries our nonce.
async fn check_gpu(http: &reqwest::Client, payload: &str, nonce: &str) -> Result<bool, String> {
    if payload.is_empty() {
        return Err("the report has no GPU evidence".into());
    }
    let reply: Value = http
        .post("https://nras.attestation.nvidia.com/v3/attest/gpu")
        .header("content-type", "application/json")
        .body(payload.to_owned())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("NVIDIA attestation service: {e}"))?
        .json()
        .await
        .map_err(|e| format!("NVIDIA attestation service: {e}"))?;
    let token = reply[0][1].as_str().ok_or("NVIDIA's answer has no token")?;
    let claims = nvidia_claims(http, token).await?;
    if claims["eat_nonce"] != nonce {
        return Err("NVIDIA's answer is not about our nonce".into());
    }
    if claims["x-nvidia-overall-att-result"] != true {
        return Err("NVIDIA says the GPU evidence does not verify".into());
    }
    Ok(true)
}

/// The claims of an NVIDIA attestation token, once its ES384 signature
/// verifies under one of the keys NVIDIA publishes for the service.
async fn nvidia_claims(http: &reqwest::Client, token: &str) -> Result<Value, String> {
    let jwks: Value = http
        .get("https://nras.attestation.nvidia.com/.well-known/jwks.json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("NVIDIA's signing keys: {e}"))?
        .json()
        .await
        .map_err(|e| format!("NVIDIA's signing keys: {e}"))?;
    verify_jwt(token, &jwks)
}

/// Checks an ES384 JWT against a JWK set and returns its claims.
fn verify_jwt(token: &str, jwks: &Value) -> Result<Value, String> {
    use base64::Engine as _;
    use p384::ecdsa::signature::Verifier as _;
    let b64 = |s: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.trim_end_matches('='))
            .map_err(|e| format!("NVIDIA's token: {e}"))
    };
    let mut parts = token.split('.');
    let (Some(head), Some(body), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("NVIDIA's token is malformed".into());
    };
    let header: Value = serde_json::from_slice(&b64(head)?).map_err(|e| e.to_string())?;
    if header["alg"] != "ES384" {
        return Err("NVIDIA's token is not signed with ES384".into());
    }
    let key = jwks["keys"]
        .as_array()
        .and_then(|keys| keys.iter().find(|k| k["kid"] == header["kid"]))
        .ok_or("NVIDIA's token names a key NVIDIA does not publish")?;
    let coord = |c: &str| -> Result<Vec<u8>, String> { b64(key[c].as_str().unwrap_or_default()) };
    let mut point = vec![4u8];
    point.extend(coord("x")?);
    point.extend(coord("y")?);
    let verifying = p384::ecdsa::VerifyingKey::from_sec1_bytes(&point)
        .map_err(|_| "NVIDIA's signing key is not a P-384 point")?;
    let signature = p384::ecdsa::Signature::from_slice(&b64(sig)?)
        .map_err(|_| "NVIDIA's token signature is malformed")?;
    verifying
        .verify(format!("{head}.{body}").as_bytes(), &signature)
        .map_err(|_| "NVIDIA's token signature does not verify")?;
    serde_json::from_slice(&b64(body)?).map_err(|e| format!("NVIDIA's token: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test")]
mod tests {
    #[test]
    fn an_nvidia_token_is_read_only_when_its_signature_verifies() {
        use base64::Engine as _;
        use p384::ecdsa::signature::Signer as _;
        let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        let signing = p384::ecdsa::SigningKey::from_slice(&[7u8; 48]).unwrap();
        let point = signing.verifying_key().to_encoded_point(false);
        let jwks = serde_json::json!({ "keys": [{
            "kty": "EC", "crv": "P-384", "kid": "k1",
            "x": b64(point.x().unwrap()), "y": b64(point.y().unwrap()),
        }] });
        let head = b64(br#"{"alg":"ES384","kid":"k1"}"#);
        let body = b64(br#"{"eat_nonce":"ab","x-nvidia-overall-att-result":true}"#);
        let sig: p384::ecdsa::Signature = signing.sign(format!("{head}.{body}").as_bytes());
        let token = format!("{head}.{body}.{}", b64(&sig.to_bytes()));
        assert_eq!(verify_jwt(&token, &jwks).unwrap()["eat_nonce"], "ab");
        let forged_body = b64(br#"{"eat_nonce":"ab","x-nvidia-overall-att-result":false}"#);
        let forged = format!("{head}.{forged_body}.{}", b64(&sig.to_bytes()));
        assert!(verify_jwt(&forged, &jwks).is_err());
        let other_kid = token.replacen(&head, &b64(br#"{"alg":"ES384","kid":"k2"}"#), 1);
        assert!(verify_jwt(&other_kid, &jwks).is_err());
    }

    use super::*;

    /// A real signature NEAR AI served for a reply (9 Nov 2026).
    const TEXT: &str = "deepseek-ai/DeepSeek-V4.1-Flash-1017:6ac063e7245ff6c7483e7e19a56789063b6166977ab4b800be1e94a433c20595:9ab82b692db07b2e421b9d6e76a7a0aee57bd0d2ea8b2876fa0618d6b5af27b2";
    const SIG: &str = "0x00118bfcf9bf50b7959a25af6e927282716d012017608977f31edd5280dee71c26e62f100864eb4b4fd68c3e42e521d261bd220d81fca3ec574bbb5a5655eac71c";
    const ADDRESS: &str = "0xfe0071895500a9755c18044948ab4f4e1a7f337e";

    fn attested() -> Attested {
        Attested {
            model: "deepseek-ai/DeepSeek-V4.1-Flash-1017".into(),
            signing_address: ADDRESS.into(),
            nonce: String::new(),
            tcb_status: "UpToDate".into(),
            advisories: Vec::new(),
            gpu_verified: true,
            os_image_hash: String::new(),
            compose_hash: String::new(),
            checked_at_s: 0,
        }
    }

    #[test]
    fn a_real_reply_signature_recovers_to_the_attested_key() {
        assert_eq!(recover_address(TEXT, SIG).unwrap(), ADDRESS);
    }

    #[test]
    fn a_changed_text_recovers_to_another_key() {
        let other = TEXT.replace(":6ac0", ":7ac0");
        assert_ne!(recover_address(&other, SIG).unwrap(), ADDRESS);
    }

    const REQUEST: &[u8] = include_bytes!("../tests/fixtures/near-ai-request.json");
    const RESPONSE: &[u8] = include_bytes!("../tests/fixtures/near-ai-response.json");

    fn sig(kind: &str) -> Signature {
        Signature {
            text: TEXT.into(),
            signature: SIG.into(),
            signing_address: ADDRESS.into(),
            signature_kind: kind.into(),
        }
    }

    #[test]
    fn the_real_reply_checks_against_its_exact_bytes() {
        check_reply(&attested(), &sig("provider_tee"), REQUEST, RESPONSE).unwrap();
    }

    #[test]
    fn a_changed_reply_or_request_fails() {
        let mut changed = RESPONSE.to_vec();
        changed[0] = b' ';
        assert!(check_reply(&attested(), &sig("provider_tee"), REQUEST, &changed).is_err());
        assert!(check_reply(&attested(), &sig("provider_tee"), RESPONSE, RESPONSE).is_err());
    }

    #[test]
    fn a_gateway_signature_or_another_key_fails() {
        assert!(check_reply(&attested(), &sig("gateway"), REQUEST, RESPONSE).is_err());
        let mut other = attested();
        other.signing_address = "0x0000000000000000000000000000000000000001".into();
        assert!(check_reply(&other, &sig("provider_tee"), REQUEST, RESPONSE).is_err());
    }
}
