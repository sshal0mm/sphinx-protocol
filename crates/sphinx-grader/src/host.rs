//! OutLayer host bindings for the enclave build (wasm32-wasip2, `outlayer`).
//!
//! The module also carries its manifest: OutLayer reads the key declarations
//! and the network allowlist from the `outlayer.manifest` custom section,
//! which is covered by the wasm's SHA-256, so they are part of the build.
#![allow(unsafe_code, reason = "the manifest must live in a wasm custom section")]

#[allow(clippy::all, clippy::pedantic, unused_qualifications, reason = "generated bindings")]
mod bindings {
    wit_bindgen::generate!({ world: "grader-host", path: "wit", generate_all });
}

pub use bindings::near::rpc::api as rpc;
pub use bindings::outlayer::encryption_keys::api as encryption_keys;
pub use bindings::outlayer::signing_keys::api as signing_keys;

/// - `signing_keys.grader`: the ed25519 result key (`bind: wasm`, `caller: signer`).
/// - `encryption_keys.box`: seeds the X25519 box key submissions are encrypted to.
/// - `capabilities.network: []`: no outbound HTTP at all. Chain reads go
///   through the worker's NEAR RPC host functions, not the network.
#[used]
#[link_section = "outlayer.manifest"]
static OUTLAYER_MANIFEST: [u8; include_bytes!("../manifest.json").len()] =
    *include_bytes!("../manifest.json");
