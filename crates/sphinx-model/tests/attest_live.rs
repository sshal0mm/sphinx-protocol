//! Live attestation of a NEAR AI Cloud model. Needs `SPHINX_MODEL_KEY` and the
//! network: `cargo test -p sphinx-model --test attest_live -- --ignored`.
#![allow(clippy::unwrap_used, clippy::print_stdout, reason = "test")]

#[tokio::test]
#[ignore = "network and a NEAR AI key"]
async fn the_default_model_attests() {
    let key = std::env::var("SPHINX_MODEL_KEY").unwrap();
    let model = std::env::var("SPHINX_MODEL")
        .unwrap_or_else(|_| "deepseek-ai/DeepSeek-V4.1-Flash-1017".into());
    let http = reqwest::Client::new();
    let a = sphinx_model::attest::attest(&http, "https://cloud-api.near.ai/v1", &key, &model)
        .await
        .unwrap();
    println!("{a:#?}");
    assert!(a.gpu_verified);
    assert!(a.signing_address.starts_with("0x"));
}

/// Every model in the drafting pool (the configured one and its fallbacks)
/// is confirmed private, and answers with a reply signed by its attested key.
#[tokio::test]
#[ignore = "network and a NEAR AI key"]
async fn every_model_in_the_pool_attests_and_signs_its_replies() {
    use sphinx_model::{Message, Model, NearAiCloud};
    let key = std::env::var("SPHINX_MODEL_KEY").unwrap();
    let first = NearAiCloud::new(NearAiCloud::GATEWAY, key, NearAiCloud::DEFAULT_MODEL);
    let names = std::iter::once(NearAiCloud::DEFAULT_MODEL.to_owned())
        .chain(sphinx_model::pool::DEFAULT_FALLBACKS.iter().map(|s| (*s).to_owned()));
    for name in names {
        let m = first.sibling(name.clone());
        assert!(m.confirm_privacy().await, "{name}: {}", m.privacy_note());
        let reply = m.complete(&[Message::user("Reply with the single word ok.")]).await.unwrap();
        println!("{name}: {reply:?}, {} signed replies", m.verified_replies());
        assert_eq!(m.verified_replies(), 1, "{name}");
        assert!(reply.to_lowercase().contains("ok"), "{name}: {reply}");
    }
}
