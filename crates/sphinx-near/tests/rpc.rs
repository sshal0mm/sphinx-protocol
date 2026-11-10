//! Which RPC endpoints the testnet client uses.

#![allow(clippy::unwrap_used, reason = "test")]

use sphinx_near::{is_fastnear, testnet_network, TESTNET_RPC};

fn urls(urls: Option<&str>) -> Vec<String> {
    testnet_network(urls, None).rpc_endpoints.iter().map(|e| e.url.to_string()).collect()
}

#[test]
fn endpoints_come_from_the_list_given_or_the_defaults_in_order() {
    assert_eq!(urls(None).len(), TESTNET_RPC.len());
    assert!(urls(None)[0].starts_with(TESTNET_RPC[0]));
    assert_eq!(
        urls(Some("https://a.example/, not a url ,https://b.example/")),
        ["https://a.example/", "https://b.example/"]
    );
    assert_eq!(urls(Some(" , ")).len(), TESTNET_RPC.len());
}

#[test]
fn the_rpc_key_goes_only_to_fastnear() {
    let net = testnet_network(
        Some("https://rpc.testnet.fastnear.com,https://rpc.testnet.near.org,https://fastnear.com.evil.example"),
        Some("k"),
    );
    let keyed: Vec<bool> = net.rpc_endpoints.iter().map(|e| e.bearer_header.is_some()).collect();
    assert_eq!(keyed, [true, false, false]);
    assert!(!is_fastnear("http://rpc.testnet.fastnear.com"));
}
