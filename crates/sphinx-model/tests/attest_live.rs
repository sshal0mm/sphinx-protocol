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
