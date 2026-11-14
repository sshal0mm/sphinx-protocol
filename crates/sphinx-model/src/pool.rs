//! Several NEAR AI Cloud models in order of preference. When the one in use
//! is down (the gateway answers a server error or the connection fails after
//! its retries), the next model that was confirmed private takes over, and
//! stays in use until it fails in turn.

use crate::{Message, Model, NearAiCloud};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Models in order of preference, each checked on its own.
#[derive(Debug)]
pub struct Pool {
    models: Vec<NearAiCloud>,
    active: AtomicUsize,
}

/// NEAR AI's own TEE models tried when the configured one is down, unless
/// `SPHINX_MODEL_FALLBACKS` names others (empty turns fallback off).
pub const DEFAULT_FALLBACKS: &[&str] = &["Qwen/Qwen3.8-27B", "z-ai/glm-5.3-flash"];

impl Pool {
    /// The first model is preferred; the rest are tried in order.
    ///
    /// # Panics
    /// With no models.
    pub fn new(models: Vec<NearAiCloud>) -> Self {
        assert!(!models.is_empty(), "a pool needs at least one model");
        Self { models, active: AtomicUsize::new(0) }
    }

    /// The model from the environment (see [`NearAiCloud::from_env`]), then
    /// `SPHINX_MODEL_FALLBACKS` (comma separated) on the same gateway and key;
    /// then, for each key in `SPHINX_MODEL_KEYS` (comma separated, optional),
    /// all of those models again on that key. Fallbacks are only added on
    /// NEAR AI's gateway.
    pub fn from_env() -> Option<Self> {
        let first = NearAiCloud::from_env()?;
        let names: Vec<String> = match std::env::var("SPHINX_MODEL_FALLBACKS") {
            Ok(list) => list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            Err(_) => DEFAULT_FALLBACKS.iter().map(|s| (*s).to_owned()).collect(),
        };
        let mut models = vec![first.clone()];
        if first.is_gateway() {
            for name in names {
                if models.iter().all(|m| m.model != name) {
                    models.push(first.sibling(name));
                }
            }
            let keys = std::env::var("SPHINX_MODEL_KEYS").unwrap_or_default();
            let on_first_key = models.clone();
            for key in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                if first.has_key(key) {
                    continue;
                }
                models.extend(on_first_key.iter().map(|m| m.with_key(key)));
            }
        }
        Some(Self::new(models))
    }

    /// The model in use now.
    pub fn current(&self) -> &NearAiCloud {
        let i = self.active.load(Ordering::Relaxed).min(self.models.len() - 1);
        &self.models[i]
    }

    /// Checks every model not yet confirmed private. True when at least one
    /// is, so drafting can go on.
    pub async fn confirm_privacy(&self) -> bool {
        for m in &self.models {
            if !m.is_private() {
                m.confirm_privacy().await;
            }
        }
        self.is_private()
    }

    /// Whether any model may be used.
    pub fn is_private(&self) -> bool {
        self.models.iter().any(NearAiCloud::is_private)
    }

    /// Whether every model has been confirmed private.
    pub fn all_private(&self) -> bool {
        self.models.iter().all(NearAiCloud::is_private)
    }

    /// The privacy note of the model in use, or of the first usable one.
    pub fn privacy_note(&self) -> String {
        self.usable()
            .next()
            .map_or_else(|| self.current().privacy_note(), |(_, m)| m.privacy_note())
    }

    /// Usable models, starting at the one in use.
    fn usable(&self) -> impl Iterator<Item = (usize, &NearAiCloud)> {
        let n = self.models.len();
        let start = self.active.load(Ordering::Relaxed) % n;
        (0..n)
            .map(move |k| (start + k) % n)
            .map(|i| (i, &self.models[i]))
            .filter(|(_, m)| m.ready())
    }
}

/// An error that says the model (or this key) cannot answer now, not that
/// the request was wrong: a server error, a rate limit, a dropped
/// connection, or a key refused (another key may still work).
fn is_outage(e: &str) -> bool {
    e.starts_with("NEAR AI Cloud answered 5")
        || e.starts_with("NEAR AI Cloud answered 429")
        || e.starts_with("NEAR AI Cloud answered 401")
        || e.starts_with("NEAR AI Cloud answered 402")
        || e.starts_with("NEAR AI Cloud answered 403")
        || e.starts_with("NEAR AI Cloud request failed")
}

impl Model for Pool {
    fn name(&self) -> &str {
        &self.current().model
    }

    fn ready(&self) -> bool {
        self.models.iter().any(Model::ready)
    }

    fn attestation_json(&self) -> Option<serde_json::Value> {
        self.current().attestation_json()
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let order: Vec<usize> = self.usable().map(|(i, _)| i).collect();
        let mut last = "no drafting model is confirmed private right now".to_owned();
        for i in order {
            let m = &self.models[i];
            match m.complete(messages).await {
                Ok(reply) => {
                    if self.active.swap(i, Ordering::Relaxed) != i {
                        eprintln!("model: now using {} (key {})", m.model, m.key_hint());
                    }
                    return Ok(reply);
                }
                Err(e) if is_outage(&e) => {
                    eprintln!(
                        "model: {} (key {}) cannot answer ({e}), trying the next one",
                        m.model,
                        m.key_hint()
                    );
                    last = e;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outages_are_told_apart_from_bad_requests() {
        assert!(is_outage("NEAR AI Cloud answered 502 Bad Gateway: unavailable"));
        assert!(is_outage("NEAR AI Cloud answered 429 Too Many Requests: slow down"));
        assert!(is_outage("NEAR AI Cloud request failed: connection reset"));
        assert!(is_outage("NEAR AI Cloud answered 402 Payment Required: no credits"));
        assert!(!is_outage("NEAR AI Cloud answered 400 Bad Request: bad field"));
        assert!(!is_outage("the reply did not verify, so it was not used: bad signature"));
    }

    #[test]
    fn fallbacks_share_the_gateway_and_skip_duplicates() {
        let first = NearAiCloud::new(NearAiCloud::GATEWAY, "k", "Qwen/Qwen3.8-27B");
        let mut models = vec![first.clone()];
        for name in DEFAULT_FALLBACKS {
            if models.iter().all(|m| m.model != *name) {
                models.push(first.sibling((*name).to_owned()));
            }
        }
        let pool = Pool::new(models);
        assert_eq!(pool.models.len(), DEFAULT_FALLBACKS.len());
        assert!(pool.models.iter().all(NearAiCloud::is_gateway));
        // Nothing is confirmed private yet, so nothing may draft.
        assert!(!pool.ready());
    }
}
