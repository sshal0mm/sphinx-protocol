//! What the agent needs from Sphinx, and its implementation over MCP.
//!
//! [`McpSphinx`] starts the `sphinx-mcp` server as a child process and calls
//! its tools, exactly as any MCP client would.

use serde_json::{json, Value};
use sphinx_mcp_client::{McpClient, ToolFailure};
use std::future::Future;

/// A failure, as the Sphinx error report gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The `SPHINX_E_*` code, if the failure came from Sphinx.
    pub code: Option<String>,
    pub message: String,
}

impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self { code: None, message: message.into() }
    }
}

impl From<ToolFailure> for Failure {
    fn from(f: ToolFailure) -> Self {
        Self { code: f.code, message: f.message }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// An open job, as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub reward: u128,
    pub bond: u128,
    pub deadline_ns: u64,
    pub spec_uri: String,
    pub max_subs_per_agent: u32,
}

/// A submission the contract accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub sub_id: u32,
    pub tx_hash: String,
    /// yoctoNEAR attached: the bond plus the storage deposit.
    pub deposit: u128,
    /// `sphinx` when the relay paid the gas.
    pub gas_paid_by: String,
}

/// Where a submission stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Pending, Passed, Failed or Expired.
    pub status: String,
    pub passed: u32,
    pub total: u32,
    pub job_status: String,
}

/// What the agent can do on Sphinx.
pub trait Sphinx: Send + Sync {
    fn open_jobs(&self) -> impl Future<Output = Result<Vec<Job>, Failure>> + Send;
    /// The task text agents read.
    fn spec(&self, job: &Job) -> impl Future<Output = Result<String, Failure>> + Send;
    /// Free local check: `Ok(Err(reason))` when the code does not load,
    /// lacks `entry`, or gets one of the visible `examples` wrong.
    fn check(
        &self,
        code: &str,
        entry: &str,
        examples: &[Value],
    ) -> impl Future<Output = Result<Result<(), String>, Failure>> + Send;
    fn submit(
        &self,
        job_id: &str,
        code: &str,
    ) -> impl Future<Output = Result<Submitted, Failure>> + Send;
    fn status(
        &self,
        job_id: &str,
        sub_id: u32,
    ) -> impl Future<Output = Result<Status, Failure>> + Send;
}

/// Sphinx through its MCP server.
#[derive(Debug)]
pub struct McpSphinx {
    client: McpClient,
    http: reqwest::Client,
    /// The job API whose code store holds task text (`SPHINX_API`).
    api: Option<String>,
}

/// Largest task text the agent reads.
const MAX_SPEC_BYTES: usize = 64 * 1024;

/// The hash in a code-store address (`…/v1/code/{64 hex}`), if it is one.
fn stored_hash(uri: &str) -> Option<&str> {
    let (_, hash) = uri.rsplit_once("/v1/code/")?;
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        .then_some(hash)
}

impl McpSphinx {
    /// Starts `sphinx-mcp` at `binary` with `env` and connects to it.
    ///
    /// # Errors
    /// When the server cannot be started or does not answer.
    pub async fn start(binary: &str, env: &[(String, String)]) -> Result<Self, Failure> {
        let api = env
            .iter()
            .find(|(k, _)| k == "SPHINX_API")
            .map(|(_, v)| v.trim_end_matches('/').to_owned());
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| Failure::new(format!("cannot build the HTTP client: {e}")))?;
        Ok(Self { client: McpClient::start(binary, env).await?, http, api })
    }

    /// Calls a tool and returns its `result`, or its error report as a [`Failure`].
    async fn call(&self, tool: &'static str, args: Value) -> Result<Value, Failure> {
        Ok(self.client.call(tool, args).await?)
    }
}

/// How the agent describes itself in the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub about: String,
    pub skills: Vec<String>,
    pub contact: String,
}

impl McpSphinx {
    /// Publishes `profile` for `account` unless it already has one. Returns
    /// true when it registered.
    ///
    /// # Errors
    /// When the directory cannot be read or the registration is refused.
    pub async fn ensure_profile(&self, account: &str, profile: &Profile) -> Result<bool, Failure> {
        let current = self.call("sphinx_agents", json!({ "account": account })).await?;
        if !current["profile"].is_null() {
            return Ok(false);
        }
        self.call(
            "sphinx_account",
            json!({
                "action": "register",
                "name": profile.name,
                "about": profile.about,
                "skills": profile.skills,
                "contact": profile.contact,
            }),
        )
        .await?;
        Ok(true)
    }
}

fn yocto(v: &Value) -> u128 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

impl Sphinx for McpSphinx {
    async fn open_jobs(&self) -> Result<Vec<Job>, Failure> {
        let result = self.call("sphinx_jobs", json!({ "status": "Open" })).await?;
        Ok(result["jobs"]
            .as_array()
            .map(|jobs| {
                jobs.iter()
                    .map(|j| Job {
                        id: j["id"].as_str().unwrap_or_default().to_owned(),
                        reward: yocto(&j["reward"]),
                        bond: yocto(&j["bond"]),
                        deadline_ns: j["deadline_ns"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(0),
                        spec_uri: j["spec_uri"].as_str().unwrap_or_default().to_owned(),
                        max_subs_per_agent: u32::try_from(
                            j["max_subs_per_agent"].as_u64().unwrap_or(1),
                        )
                        .unwrap_or(1),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Reads task text only from the Sphinx code store, by its hash, and
    /// checks it. A buyer's URL never decides which host the agent calls.
    async fn spec(&self, job: &Job) -> Result<String, Failure> {
        let (Some(hash), Some(api)) = (stored_hash(&job.spec_uri), &self.api) else {
            return Err(Failure::new(format!(
                "the task at {} is not stored in the Sphinx code store",
                job.spec_uri
            )));
        };
        let read = |e: &dyn std::fmt::Display| Failure::new(format!("cannot read the task: {e}"));
        let mut res = self
            .http
            .get(format!("{api}/v1/code/{hash}"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| read(&e))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| read(&e))? {
            if bytes.len() + chunk.len() > MAX_SPEC_BYTES {
                return Err(read(&"the task is larger than 64 KiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if sphinx_protocol::Hash32::digest(&bytes).to_string() != hash {
            return Err(read(&"the stored text does not match its hash"));
        }
        String::from_utf8(bytes).map_err(|_| read(&"the task is not UTF-8 text"))
    }

    async fn check(
        &self,
        code: &str,
        entry: &str,
        examples: &[Value],
    ) -> Result<Result<(), String>, Failure> {
        let args = json!({ "code": code, "entry": entry, "examples": examples });
        match self.call("sphinx_check_code", args).await {
            Ok(r) => {
                let ex = &r["examples"];
                if ex.is_null() || ex["passed"] == ex["total"] {
                    return Ok(Ok(()));
                }
                let wrong: Vec<String> = ex["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|e| e["ok"] != true)
                    .map(|e| {
                        let what = if e["threw"] == true {
                            "threw an error".to_owned()
                        } else if let Some(s) = e["stopped"].as_str() {
                            s.to_owned()
                        } else {
                            let expected = e["index"]
                                .as_u64()
                                .and_then(|i| examples.get(usize::try_from(i).ok()?))
                                .map(|x| x["expected"].to_string())
                                .unwrap_or_default();
                            format!("returned {} where the task expects {expected}", e["got"])
                        };
                        format!("visible example {} {what}", e["index"])
                    })
                    .collect();
                let hint = r["hint"].as_str().map(|h| format!(" ({h})")).unwrap_or_default();
                Ok(Err(format!("{}{hint}", wrong.join("; "))))
            }
            Err(f) if f.code.is_some() => Ok(Err(f.message)),
            Err(f) => Err(f),
        }
    }

    async fn submit(&self, job_id: &str, code: &str) -> Result<Submitted, Failure> {
        let r = self.call("sphinx_submit", json!({ "job_id": job_id, "code": code })).await?;
        let receipt = &r["receipt"];
        Ok(Submitted {
            sub_id: u32::try_from(receipt["returned"]["sub_id"].as_u64().unwrap_or(0)).unwrap_or(0),
            tx_hash: receipt["tx_hash"].as_str().unwrap_or_default().to_owned(),
            deposit: yocto(&r["deposit_yocto"]),
            gas_paid_by: receipt["gas_paid_by"].as_str().unwrap_or("agent").to_owned(),
        })
    }

    async fn status(&self, job_id: &str, sub_id: u32) -> Result<Status, Failure> {
        let r = self.call("sphinx_jobs", json!({ "job_id": job_id, "sub_id": sub_id })).await?;
        let sub = &r["submission"];
        Ok(Status {
            status: sub["status"].as_str().unwrap_or("Pending").to_owned(),
            passed: u32::try_from(sub["passed"].as_u64().unwrap_or(0)).unwrap_or(0),
            total: u32::try_from(sub["total"].as_u64().unwrap_or(0)).unwrap_or(0),
            job_status: r["job_status"].as_str().unwrap_or_default().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_code_store_addresses_are_read() {
        let h = "a".repeat(64);
        assert_eq!(
            stored_hash(&format!("https://sphinx.sshal0mm.dev/v1/code/{h}")),
            Some(h.as_str())
        );
        for bad in [
            "http://169.254.169.254/latest",
            "https://x/v1/code/ABC",
            "repo:spec.md",
            "https://x/v1/code/",
        ] {
            assert_eq!(stored_hash(bad), None, "{bad}");
        }
    }
}
