//! Concierge tools: draft a job from a plain request, then post it.
//!
//! Drafting runs on the configured model: NEAR AI Cloud by default (private inference in
//! a TEE). The hidden suite and reference are written to the drafts folder on
//! this machine and are never returned to the MCP client, so a buyer's chat
//! assistant never sees the hidden tests. Posting reads the draft back from
//! that folder and goes through `sphinx_post_job`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sphinx_concierge::{recommend, Candidate, Concierge, Draft, Recommendation};
use sphinx_protocol::{Code, SphinxError};
use std::path::{Path, PathBuf};

use crate::{internal, PostJob, SphinxMcp};

/// Agents recommended per draft.
const RECOMMENDED: usize = 3;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConciergeDraft {
    /// What you want done, in plain words, e.g. "a function that checks whether a version matches an npm semver range".
    pub request: String,
    /// Reward for the first full pass, in NEAR as a decimal string, e.g. "0.5". Used to pick agents who take jobs at this price.
    pub reward_near: String,
    /// Bond each submission locks, in NEAR. Default "0.02".
    #[serde(default)]
    pub bond_near: Option<String>,
    /// Hours until submissions close. Default 24.
    #[serde(default)]
    pub deadline_hours: Option<u32>,
    /// Graded attempts each agent may make. Default 3.
    #[serde(default)]
    pub max_subs_per_agent: Option<u32>,
    /// Submissions the job accepts in total. Default 12.
    #[serde(default)]
    pub max_subs_total: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConciergePost {
    /// The `draft_id` from `sphinx_concierge` with action `draft`.
    pub draft_id: String,
    /// Invite the recommended agents, so only they may submit. Default false: the job is open to every agent.
    #[serde(default)]
    pub invite_recommended: bool,
    /// Invite exactly these agent accounts instead (at most 8).
    #[serde(default)]
    pub invited: Option<Vec<String>>,
    /// Finish a post of this draft that stopped halfway: the job id from the
    /// error's `ctx.resume_with_job_id`. Only the missing steps run.
    #[serde(default, deserialize_with = "crate::optional_job_id")]
    pub job_id: Option<u64>,
    /// Post even though the second solver disagreed with the reference on
    /// some tests. Default false: such a draft is refused, because its hidden
    /// tests may not match the description and no agent could pass them.
    #[serde(default)]
    pub accept_differences: bool,
}

/// The terms a draft will be posted with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terms {
    pub reward_near: String,
    pub bond_near: String,
    pub deadline_hours: u32,
    pub max_subs_per_agent: u32,
    pub max_subs_total: u32,
}

/// Everything saved for a draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Saved {
    draft: Draft,
    terms: Terms,
    recommended: Vec<Recommendation>,
}

fn not_found(id: &str) -> SphinxError {
    SphinxError::new(Code::ConciergeDraftNotFound, format!("there is no draft {id}"))
        .ctx("draft_id", id)
}

/// The folder for draft `id`, refusing ids that are not the 16 hex characters drafts get.
fn folder(root: &Path, id: &str) -> Result<PathBuf, SphinxError> {
    if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(not_found(id));
    }
    Ok(root.join(id))
}

/// Writes a draft readable only by this user: the folder is 0700 and every
/// file 0600, since the folder holds hidden tests.
fn save(root: &Path, saved: &Saved) -> Result<(), SphinxError> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let dir = folder(root, &saved.draft.id)?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(internal)?;
    let json = serde_json::to_string_pretty(saved).map_err(internal)?;
    for (name, text) in [
        ("spec.md", saved.draft.spec.as_str()),
        ("suite.json", saved.draft.suite.as_str()),
        ("reference.js", saved.draft.reference.as_str()),
        ("draft.json", json.as_str()),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(dir.join(name))
            .map_err(internal)?;
        file.write_all(text.as_bytes()).map_err(internal)?;
    }
    Ok(())
}

fn load(root: &Path, id: &str) -> Result<Saved, SphinxError> {
    let text =
        std::fs::read_to_string(folder(root, id)?.join("draft.json")).map_err(|_| not_found(id))?;
    serde_json::from_str(&text).map_err(internal)
}

impl SphinxMcp {
    pub(crate) async fn concierge_draft(&self, p: ConciergeDraft) -> Result<Value, SphinxError> {
        let model = self.settings.model.as_ref().ok_or_else(|| {
            SphinxError::new(
                Code::CliConfigMissing,
                "the concierge needs a model; set SPHINX_MODEL_KEY (and SPHINX_MODEL_BASE, SPHINX_MODEL for a provider other than NEAR AI)",
            )
            .ctx("var", "SPHINX_MODEL_KEY")
        })?;
        // The concierge writes the hidden tests and the reference solution, so
        // the model's provider sees them unless the model runs in a TEE. Any
        // OpenAI-compatible provider set with SPHINX_MODEL_BASE is the buyer's
        // own choice and is used, with a note on the draft. Refused is only the
        // case nobody chooses on purpose: NEAR AI's gateway routing the model
        // to another provider instead of serving it in its TEE.
        if model.is_gateway()
            && !model.is_private()
            && !std::env::var("SPHINX_ALLOW_UNTRUSTED_MODEL").is_ok_and(|v| v == "1")
        {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!(
                    "{}; it would see the hidden tests it writes. Pick a model NEAR AI serves in its TEE, or set SPHINX_ALLOW_UNTRUSTED_MODEL=1 to accept that",
                    model.privacy_note()
                ),
            )
            .ctx("var", "SPHINX_ALLOW_UNTRUSTED_MODEL"));
        }
        let reward = sphinx_near::parse_near(&p.reward_near)?;
        let terms = Terms {
            reward_near: p.reward_near,
            bond_near: p.bond_near.unwrap_or_else(|| "0.02".into()),
            deadline_hours: p.deadline_hours.unwrap_or(24),
            max_subs_per_agent: p.max_subs_per_agent.unwrap_or(3),
            max_subs_total: p.max_subs_total.unwrap_or(12),
        };
        sphinx_near::parse_near(&terms.bond_near)?;
        let draft = Concierge::new(model).draft(&p.request, &mut |_| {}).await?;
        let directory = self.list_agents().await?;
        let candidates: Vec<Candidate> =
            serde_json::from_value(directory["agents"].clone()).unwrap_or_default();
        let recommended = recommend(&candidates, &draft.skills, reward, RECOMMENDED);
        let mut draft = draft;
        // Salt the reference once, here, so its public hash cannot be matched by
        // guessing a short solution, and a resumed post reads the same bytes.
        draft.reference = crate::with_salt(&draft.reference);
        let saved = Saved { draft, terms, recommended };
        save(&self.settings.drafts, &saved)?;
        let mut out = review(&saved);
        // Who saw the hidden tests while they were written.
        out["model"] = Value::String(model.privacy_note());
        Ok(out)
    }

    pub(crate) async fn concierge_post(&self, p: ConciergePost) -> Result<Value, SphinxError> {
        let saved = load(&self.settings.drafts, &p.draft_id)?;
        let d = &saved.draft;
        if !d.differences.is_empty() && !p.accept_differences && p.job_id.is_none() {
            return Err(SphinxError::new(
                Code::ConciergeUnsettled,
                format!(
                    "a second solver disagreed with the reference on {} of {} tests; nothing was posted",
                    d.differences.len(),
                    d.tests
                ),
            )
            .ctx("differences", d.differences.len())
            .ctx("tests", d.tests)
            .ctx("draft_id", &d.id));
        }
        let invited = match (p.invited, p.invite_recommended) {
            (Some(list), _) => Some(list),
            (None, true) if !saved.recommended.is_empty() => {
                Some(saved.recommended.iter().map(|r| r.account.clone()).collect())
            }
            (None, true) => {
                return Err(SphinxError::new(
                    Code::CliUsage,
                    "no agent was recommended for this draft; post it open to every agent",
                ))
            }
            (None, false) => None,
        };
        let t = &saved.terms;
        let posted = self
            .post_job(PostJob {
                suite: saved.draft.suite.clone(),
                reference: saved.draft.reference.clone(),
                title: None,
                spec_uri: None,
                spec: Some(saved.draft.spec.clone()),
                reward_near: t.reward_near.clone(),
                bond_near: t.bond_near.clone(),
                max_subs_per_agent: t.max_subs_per_agent,
                max_subs_total: t.max_subs_total,
                deadline_hours: t.deadline_hours,
                job_id: p.job_id,
                invited: invited.clone(),
            })
            .await
            .map_err(|e| e.ctx("draft_id", &p.draft_id))?;
        let job_id = posted["job_id"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0u64);
        // The job is committed and its suite stored for the enclave: the local
        // copy of the hidden tests is no longer needed.
        let kept = std::env::var("SPHINX_KEEP_DRAFTS").is_ok_and(|v| v == "1");
        if !kept {
            if let Ok(dir) = folder(&self.settings.drafts, &p.draft_id) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        Ok(json!({
            "draft_id": p.draft_id,
            "job_id": posted["job_id"],
            "job_uid": sphinx_near::uid::job_uid(self.settings.escrow.contract_id().as_ref(), job_id),
            "invited": invited.unwrap_or_default(),
            "posted": posted,
            "next_step": "the Sphinx relayer checks the hidden suite in the enclave and opens the job; follow it with sphinx_jobs(job_id)",
        }))
    }
}

/// What the buyer's assistant sees: the public task and the checks. Never
/// the hidden tests, the disagreeing inputs (they are hidden tests too), or
/// where they are stored.
fn review(saved: &Saved) -> Value {
    let d = &saved.draft;
    let check = if d.differences.is_empty() {
        format!(
            "a second solver that read only the description agreed with the reference on all {} tests",
            d.tests
        )
    } else {
        format!(
            "a second solver disagreed with the reference on {} of {} tests; the buyer can read them in the draft's local folder before posting",
            d.differences.len(),
            d.tests
        )
    };
    json!({
        "draft_id": d.id,
        "title": d.title,
        "spec": d.spec,
        "entry": d.entry,
        "tests": d.tests,
        "skills": d.skills,
        "second_opinion": d.second_opinion,
        "check": check,
        "terms": saved.terms,
        "recommended_agents": saved.recommended,
        "next_step": "show the buyer the spec, terms and recommended agents; when they agree, call sphinx_concierge with action post and this draft_id",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_ids_cannot_reach_outside_the_drafts_folder() {
        let root = Path::new("/drafts");
        assert_eq!(folder(root, "0123456789abcdef").unwrap(), root.join("0123456789abcdef"));
        for bad in ["../../etc/passwd", "0123456789ABCDEF", "0123", "0123456789abcdeg"] {
            assert_eq!(folder(root, bad).unwrap_err().code(), Code::ConciergeDraftNotFound);
        }
    }
}
