//! The language model behind Sphinx tools.
//!
//! [`NearAiCloud`] calls any OpenAI-compatible chat completions API: NEAR AI
//! Cloud by default (private inference in TEEs), or xAI, `OpenAI`, `DeepSeek`,
//! Moonshot (Kimi), `OpenRouter` or a local server. Configure it with
//! `SPHINX_MODEL_BASE`, `SPHINX_MODEL_KEY` and `SPHINX_MODEL` (the older
//! `NEAR_AI_BASE`, `NEAR_AI_API_KEY` and `NEAR_AI_MODEL` still work).
//! Only NEAR AI's gateway runs models in TEEs; with any other provider, the
//! provider sees every request. [`Scripted`] returns fixed replies for tests.

pub mod attest;
pub mod pool;

pub use pool::Pool;

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

/// One chat message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

/// Something that answers a conversation.
pub trait Model: Send + Sync {
    /// The model's name, for the log.
    fn name(&self) -> &str;

    /// The next reply in `messages`.
    fn complete(&self, messages: &[Message])
        -> impl Future<Output = Result<String, String>> + Send;

    /// What was checked about where this model runs, for people to see.
    /// `None` for a model with no attestation.
    fn attestation_json(&self) -> Option<serde_json::Value> {
        None
    }

    /// Whether the model may be given hidden tests to write now. A model on a
    /// gateway is ready once its privacy was confirmed.
    fn ready(&self) -> bool {
        true
    }
}

/// A model on NEAR AI Cloud. Its `Debug` output never shows the key.
#[derive(Clone)]
pub struct NearAiCloud {
    pub base_url: String,
    api_key: String,
    pub model: String,
    /// Whether a reasoning model may think before answering. Off by default:
    /// on these tasks thinking costs minutes and the pass count already
    /// tells the agent what to fix.
    pub thinking: bool,
    http: reqwest::Client,
    /// Whether NEAR AI's catalog lists this model as served by NEAR AI itself
    /// (in its attested TEEs): 0 not checked, 1 yes, 2 no (routed to another
    /// provider through the gateway). Set by [`NearAiCloud::confirm_privacy`].
    served: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// The model's checked attestation. While set, every reply must be
    /// signed by its key (see [`attest`]).
    attested: std::sync::Arc<Mutex<Option<attest::Attested>>>,
    /// Replies whose signature was checked against the attested key.
    verified: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl std::fmt::Debug for NearAiCloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NearAiCloud")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("thinking", &self.thinking)
            .finish_non_exhaustive()
    }
}

impl NearAiCloud {
    /// The public NEAR AI Cloud gateway.
    pub const GATEWAY: &'static str = "https://cloud-api.near.ai/v1";
    /// A model NEAR AI runs in its own TEE, with an attestation report.
    pub const DEFAULT_MODEL: &'static str = "deepseek-ai/DeepSeek-V4.1-Flash-1017";
    /// Tries per completion when the connection drops, the gateway
    /// rate-limits, or it answers with a server error.
    pub const TRIES: u32 = 3;
    /// The longest one completion may take before the attempt is abandoned.
    /// A draft (task text, a full hidden suite and a reference) is several
    /// thousand tokens in one reply; at 25 to 35 tokens a second, which NEAR
    /// AI Cloud measured on 10 Nov 2026, that is up to about four minutes.
    /// A timed-out reply is not retried here; a [`Pool`] moves to its next
    /// model instead.
    pub const TIMEOUT: Duration = Duration::from_mins(6);
    /// The longest a [`NearAiCloud::ping`] may take.
    pub const PING_TIMEOUT: Duration = Duration::from_secs(20);

    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            thinking: false,
            served: std::sync::Arc::default(),
            attested: std::sync::Arc::default(),
            verified: std::sync::Arc::default(),
            http: reqwest::Client::builder()
                .timeout(Self::TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    /// Another model on the same gateway with the same key, checked on its
    /// own: its privacy and attestation start unconfirmed.
    #[must_use]
    pub fn sibling(&self, model: String) -> Self {
        let mut other = Self::new(self.base_url.clone(), self.api_key.clone(), model);
        other.thinking = self.thinking;
        other
    }

    /// The same model on the same gateway with another key, checked on its
    /// own.
    #[must_use]
    pub fn with_key(&self, key: &str) -> Self {
        let mut other = Self::new(self.base_url.clone(), key, self.model.clone());
        other.thinking = self.thinking;
        other
    }

    /// Whether this client uses `key`.
    pub fn has_key(&self, key: &str) -> bool {
        self.api_key == key
    }

    /// The key's last four characters, for the log; never the key.
    pub fn key_hint(&self) -> String {
        let n = self.api_key.chars().count();
        format!("…{}", self.api_key.chars().skip(n.saturating_sub(4)).collect::<String>())
    }

    /// Lets a reasoning model think before answering.
    #[must_use]
    pub fn with_thinking(mut self, thinking: bool) -> Self {
        self.thinking = thinking;
        self
    }

    /// A model from the environment: `SPHINX_MODEL_KEY` (or
    /// `NEAR_AI_API_KEY`) is required; `SPHINX_MODEL_BASE` and `SPHINX_MODEL`
    /// default to NEAR AI's gateway and its TEE model. `None` without a key.
    pub fn from_env() -> Option<Self> {
        let var = |a: &str, b: &str| {
            std::env::var(a).ok().filter(|v| !v.is_empty()).or_else(|| std::env::var(b).ok())
        };
        let key = var("SPHINX_MODEL_KEY", "NEAR_AI_API_KEY").filter(|k| !k.is_empty())?;
        let base =
            var("SPHINX_MODEL_BASE", "NEAR_AI_BASE").unwrap_or_else(|| Self::GATEWAY.to_owned());
        let model =
            var("SPHINX_MODEL", "NEAR_AI_MODEL").unwrap_or_else(|| Self::DEFAULT_MODEL.to_owned());
        Some(Self::new(base, key, model))
    }

    /// Whether requests go to NEAR AI's gateway.
    pub fn is_gateway(&self) -> bool {
        self.base_url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split(['/', ':']).next())
            .is_some_and(|host| host == "cloud-api.near.ai")
    }

    /// Whether this model runs inside NEAR AI's TEEs, so no provider sees the
    /// data in the clear. The gateway also routes to other providers' models,
    /// so this is true only once [`NearAiCloud::confirm_privacy`] has found
    /// the model in NEAR AI's catalog as served by NEAR AI itself.
    pub fn is_private(&self) -> bool {
        self.is_gateway() && self.served.load(std::sync::atomic::Ordering::Relaxed) == 1
    }

    /// Looks the model up in the gateway's catalog (`/models`): private when
    /// it is owned by `nearai`. Anything else, or no answer, is not private.
    pub async fn confirm_privacy(&self) -> bool {
        if !self.is_gateway() {
            return false;
        }
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let owner = async {
            let v: serde_json::Value = self
                .http
                .get(url)
                .bearer_auth(&self.api_key)
                .timeout(Duration::from_secs(20))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            v["data"]
                .as_array()?
                .iter()
                .find(|m| m["id"] == self.model.as_str())
                .and_then(|m| m["owned_by"].as_str().map(str::to_owned))
        }
        .await;
        // Listed as NEAR AI's own is not enough: the model's attestation must
        // check out too (Intel TDX quote, NVIDIA GPU, key bound to our nonce).
        let private = owner.as_deref() == Some("nearai") && self.attest().await.is_ok();
        self.mark_served(private);
        private
    }

    /// Fetches and checks the model's attestation now, and keeps it: from
    /// then on every reply must be signed by the attested key.
    ///
    /// # Errors
    /// The check that failed.
    pub async fn attest(&self) -> Result<attest::Attested, String> {
        let a = attest::attest(&self.http, &self.base_url, &self.api_key, &self.model).await?;
        if let Ok(mut slot) = self.attested.lock() {
            *slot = Some(a.clone());
        }
        Ok(a)
    }

    /// The attestation replies are checked against, if any.
    pub fn attestation(&self) -> Option<attest::Attested> {
        self.attested.lock().ok().and_then(|a| a.clone())
    }

    /// How many replies were checked against the attested key.
    pub fn verified_replies(&self) -> u64 {
        self.verified.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How long an attestation is trusted before it is fetched again.
    pub const ATTESTATION_TTL_S: u64 = 30 * 60;

    /// Checks one reply: its signature covers these exact bytes and recovers
    /// to the attested key. Attests again first when the last check is old
    /// or the model now signs with another key.
    async fn check_reply(&self, id: &str, request: &[u8], response: &[u8]) -> Result<(), String> {
        let Some(mut attested) = self.attestation() else { return Ok(()) };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if now.saturating_sub(attested.checked_at_s) > Self::ATTESTATION_TTL_S {
            attested = self.attest().await?;
        }
        let url = format!("{}/signature/{id}", self.base_url.trim_end_matches('/'));
        let mut last = String::new();
        // The signature is written just after the reply; give it a moment.
        for attempt in 0..4u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
            let sig: attest::Signature = match async {
                self.http
                    .get(&url)
                    .query(&[("model", self.model.as_str()), ("signing_algo", "ecdsa")])
                    .bearer_auth(&self.api_key)
                    .timeout(Duration::from_secs(20))
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)?
                    .json()
                    .await
            }
            .await
            {
                Ok(sig) => sig,
                Err(e) => {
                    last = format!("the reply's signature could not be fetched: {e}");
                    continue;
                }
            };
            if sig.signing_address.to_lowercase() != attested.signing_address {
                // The model moved to another machine: attest it, then check.
                attested = self.attest().await?;
            }
            attest::check_reply(&attested, &sig, request, response)?;
            self.verified.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        Err(last)
    }

    fn mark_served(&self, private: bool) {
        self.served.store(if private { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
    }

    /// One plain sentence on who can read what is sent to this model.
    pub fn privacy_note(&self) -> String {
        if self.is_private() {
            match self.attestation() {
                Some(a) => format!(
                    "model {} runs inside a TEE on NEAR AI Cloud: Intel TDX quote ({}) and NVIDIA GPU attestation checked, and every reply must be signed by its key {}",
                    self.model, a.tcb_status, a.signing_address
                ),
                None => format!("model {} runs inside a TEE on NEAR AI Cloud", self.model),
            }
        } else if self.is_gateway() {
            format!(
                "model {} is not one NEAR AI serves in its TEEs (or that could not be confirmed): NEAR AI routes it to its provider, which sees every request",
                self.model
            )
        } else {
            format!(
                "model {} at {} is not run in a TEE: that provider sees every request",
                self.model, self.base_url
            )
        }
    }

    /// A one-token completion, to see whether the model answers now and how
    /// fast. No retries and no signature check: it only measures health, and
    /// its reply is never used.
    ///
    /// # Errors
    /// The model did not answer in time or answered with an error.
    pub async fn ping(&self) -> Result<Duration, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut body = self.body(&[Message::user("Reply with the single word ok.")]);
        body["max_tokens"] = json!(1);
        let started = std::time::Instant::now();
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .timeout(Self::PING_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("NEAR AI Cloud request failed: {}", chain(&e)))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("NEAR AI Cloud answered {status}"));
        }
        Ok(started.elapsed())
    }

    /// The request body for `messages`. `chat_template_kwargs` is NEAR AI's
    /// switch for reasoning models; other providers reject unknown fields.
    fn body(&self, messages: &[Message]) -> serde_json::Value {
        let mut body = json!({ "model": self.model, "messages": messages, "temperature": 0.2 });
        if !self.thinking && self.is_gateway() {
            // DeepSeek's template reads `thinking`; Qwen's and GLM's read
            // `enable_thinking`. Templates ignore the switch they do not use.
            body["chat_template_kwargs"] = json!({ "thinking": false, "enable_thinking": false });
            // GLM 5.3 Flash ignores both and reasons anyway unless the
            // effort is low (NEAR AI's reasoning-models guide).
            if self.model.starts_with("z-ai/glm") {
                body["reasoning_effort"] = json!("low");
            }
        }
        body
    }
}

#[derive(Deserialize)]
struct Completion {
    #[serde(default)]
    id: Option<String>,
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

impl Model for NearAiCloud {
    fn name(&self) -> &str {
        &self.model
    }

    fn ready(&self) -> bool {
        self.is_private() || std::env::var("SPHINX_ALLOW_UNTRUSTED_MODEL").as_deref() == Ok("1")
    }

    fn attestation_json(&self) -> Option<serde_json::Value> {
        let a = self.attestation()?;
        let mut v = serde_json::to_value(&a).ok()?;
        v["verified_replies"] = self.verified_replies().into();
        Some(v)
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // The exact bytes sent and received are what the model's signature
        // covers, so they are kept as bytes.
        let request = serde_json::to_vec(&self.body(messages))
            .map_err(|e| format!("the request could not be encoded: {e}"))?;
        // A dropped connection, a rate limit or a server error is worth
        // another try; anything else is the request's own fault.
        let mut attempt = 0u32;
        let resp = loop {
            attempt += 1;
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(request.clone())
                .send()
                .await;
            let retry = match &sent {
                Err(e) => !e.is_timeout() && attempt < Self::TRIES,
                Ok(r) => {
                    (r.status().as_u16() == 429 || r.status().is_server_error())
                        && attempt < Self::TRIES
                }
            };
            if retry {
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                continue;
            }
            break sent.map_err(|e| format!("NEAR AI Cloud request failed: {}", chain(&e)))?;
        };
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let body: String = body.chars().take(300).collect();
            return Err(format!("NEAR AI Cloud answered {status}: {body}"));
        }
        let response =
            resp.bytes().await.map_err(|e| format!("NEAR AI Cloud reply was cut off: {e}"))?;
        let done: Completion = serde_json::from_slice(&response)
            .map_err(|e| format!("NEAR AI Cloud reply was not understood: {e}"))?;
        if self.attestation().is_some() {
            let id = done.id.as_deref().ok_or("the reply has no id to check its signature by")?;
            self.check_reply(id, &request, &response)
                .await
                .map_err(|e| format!("the reply did not verify, so it was not used: {e}"))?;
        }
        done.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| "NEAR AI Cloud returned no choices".to_owned())
    }
}

/// An error and every cause under it, so "error sending request" says why.
fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// Fixed replies in order, for tests; records every conversation it saw.
#[derive(Debug, Default)]
pub struct Scripted {
    replies: Mutex<Vec<String>>,
    pub seen: Mutex<Vec<Vec<Message>>>,
}

impl Scripted {
    pub fn new(replies: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut replies: Vec<String> = replies.into_iter().map(Into::into).collect();
        replies.reverse();
        Self { replies: Mutex::new(replies), seen: Mutex::default() }
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    clippy::unnecessary_literal_bound,
    reason = "a test double: replies are ready at once and the name is fixed"
)]
impl Model for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, String> {
        self.seen.lock().map_err(|e| e.to_string())?.push(messages.to_vec());
        self.replies
            .lock()
            .map_err(|e| e.to_string())?
            .pop()
            .ok_or_else(|| "no scripted reply left".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_shows_the_key() {
        let m = NearAiCloud::new("https://x", "sk-secret-value", "m");
        let shown = format!("{m:?}");
        assert!(!shown.contains("sk-secret-value") && shown.contains("\"m\""), "{shown}");
    }

    #[test]
    fn only_near_ai_gets_its_reasoning_switch_and_the_private_label() {
        let msgs = [Message::user("hi")];
        for base in [
            "https://api.x.ai/v1",
            "https://api.openai.com/v1",
            "https://api.deepseek.com/v1",
            "https://api.moonshot.ai/v1",
            "https://openrouter.ai/api/v1",
            "http://127.0.0.1:11434/v1",
            "https://cloud-api.near.ai.evil.example/v1",
        ] {
            let m = NearAiCloud::new(base, "k", "some-model");
            assert!(!m.is_private(), "{base}");
            assert!(m.body(&msgs).get("chat_template_kwargs").is_none(), "{base}");
            assert!(m.privacy_note().contains("not run in a TEE"), "{base}");
        }
        let near = NearAiCloud::new(NearAiCloud::GATEWAY, "k", NearAiCloud::DEFAULT_MODEL);
        // The gateway alone proves nothing: it routes to other providers too.
        assert!(!near.is_private());
        assert!(near.privacy_note().contains("routes it to its provider"));
        near.mark_served(false);
        assert!(!near.is_private());
        near.mark_served(true);
        assert!(near.is_private());
        assert!(near.privacy_note().contains("inside a TEE"));
    }

    #[test]
    fn thinking_is_off_unless_asked_for() {
        let m = NearAiCloud::new(NearAiCloud::GATEWAY, "k", "m");
        let msgs = [Message::user("hi")];
        assert_eq!(m.body(&msgs)["chat_template_kwargs"]["thinking"], false);
        let on = m.with_thinking(true);
        assert!(on.body(&msgs).get("chat_template_kwargs").is_none());
    }
}
