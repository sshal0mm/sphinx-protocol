//! The earning loop: find a job, have the model solve it, check, submit,
//! wait for the enclave's score, and retry with the score as feedback.

use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

use crate::model::{Message, Model};
use crate::tools::{Failure, Job, Sphinx, Status, Submitted};

/// Limits on what one run may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most yoctoNEAR the run may attach in bonds and deposits, in total.
    pub budget: u128,
    /// Most submissions per job, below the job's own cap.
    pub attempts: u32,
    /// The smallest reward worth working for, in yoctoNEAR.
    pub min_reward: u128,
    /// The largest bond, as basis points of the reward, worth risking. A job
    /// whose bond is a large share of its reward is a trap for an agent that
    /// cannot see the tests.
    pub max_bond_bps: u128,
    /// How often to ask for a submission's status.
    pub poll: Duration,
    /// How long to wait for one score before giving up on it.
    pub wait: Duration,
}

/// What happened, for the log and the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Found {
        job: String,
        reward: u128,
        bond: u128,
    },
    Skipped {
        job: String,
        why: String,
    },
    Asking {
        model: String,
        attempt: u32,
    },
    CheckFailed {
        reason: String,
    },
    Submitted {
        job: String,
        sub: Submitted,
    },
    Scored {
        job: String,
        sub_id: u32,
        status: Status,
    },
    Paid {
        job: String,
        reward: u128,
    },
    OutOfBudget {
        needed: u128,
        left: u128,
    },
    Gave {
        job: String,
        why: String,
    },
    /// The model returned code already submitted; it is asked again and nothing is spent.
    Repeated,
}

/// The model's answer: the function's name and its source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Solution {
    pub entry: String,
    pub code: String,
    /// The task's visible examples as the model read them, checked for free
    /// before any bond is paid.
    #[serde(default)]
    pub examples: Vec<serde_json::Value>,
}

/// What the model is told after a scored failure: the count, never which tests.
fn score_feedback(status: &Status) -> String {
    format!(
        "That solution passed {} of {} hidden tests. You will not learn which failed. \
         Think about edge cases the task implies (bounds, empty or extreme inputs, exact formats) \
         and reply with a corrected JSON object.",
        status.passed, status.total
    )
}

/// Model replies allowed per submission before the agent gives up on a job.
const ASKS_PER_ATTEMPT: u32 = 3;

const REPEATED: &str = "You already submitted exactly this code, and it did not pass every test. \
     Submitting it again is refused. Find the case your code still gets wrong, change the logic, \
     and reply with a corrected JSON object.";

const SYSTEM: &str = "You write JavaScript functions that are graded by hidden tests. \
Reply with only a JSON object {\"entry\": \"<function name>\", \"code\": \"<complete source>\", \
\"examples\": [{\"args\": [<arguments>], \"expected\": <result>}]}, where examples are the task's own \
visible examples, copied exactly (an empty list if it has none); they are run before anything is submitted. \
The code must define the function named in the task as a plain top-level function, use no imports, \
no network, no randomness and no clock, and must handle edge cases the task implies. \
Regular expressions are disabled in the grader and throw: use string methods (indexOf, slice, split with a string, trim).";

/// Reads the model's reply, tolerating a fenced code block around the JSON.
///
/// # Errors
/// When the reply holds no `{entry, code}` object.
pub fn parse_solution(reply: &str) -> Result<Solution, String> {
    let start = reply.find('{').ok_or("the reply holds no JSON object")?;
    let end = reply.rfind('}').ok_or("the reply holds no JSON object")?;
    let s: Solution = serde_json::from_str(&reply[start..=end])
        .map_err(|e| format!("the reply is not {{entry, code}}: {e}"))?;
    if s.entry.trim().is_empty() || s.code.trim().is_empty() {
        return Err("the reply has an empty entry or code".into());
    }
    Ok(s)
}

/// The agent: a model and Sphinx, within limits.
/// How working a job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Paid,
    /// Attempted and not paid (failed, out of budget, or the job closed).
    NotPaid,
    /// Not attempted: the task could not be read. Nothing was spent.
    Skipped,
}

pub struct Agent<'a, M: Model, S: Sphinx> {
    pub model: &'a M,
    pub sphinx: &'a S,
    pub limits: Limits,
    spent: u128,
}

impl<M: Model, S: Sphinx> std::fmt::Debug for Agent<'_, M, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("model", &self.model.name())
            .field("limits", &self.limits)
            .field("spent", &self.spent)
            .finish()
    }
}

impl<'a, M: Model, S: Sphinx> Agent<'a, M, S> {
    pub fn new(model: &'a M, sphinx: &'a S, limits: Limits) -> Self {
        Self { model, sphinx, limits, spent: 0 }
    }

    /// yoctoNEAR attached so far in this run.
    pub fn spent(&self) -> u128 {
        self.spent
    }

    /// Picks the open job with the highest reward per bond that the budget can
    /// afford, skipping `done`.
    pub fn choose<'j>(&self, jobs: &'j [Job], done: &[String], storage: u128) -> Option<&'j Job> {
        jobs.iter()
            .filter(|j| !done.contains(&j.id))
            .filter(|j| {
                j.bond.saturating_add(storage) <= self.limits.budget.saturating_sub(self.spent)
            })
            .filter(|j| j.reward >= self.limits.min_reward)
            .filter(|j| {
                j.bond.saturating_mul(10_000) <= j.reward.saturating_mul(self.limits.max_bond_bps)
            })
            .max_by_key(|j| j.reward / j.bond.max(1))
    }

    /// Works one job until it is paid, the attempts or budget run out, or
    /// the job closes.
    ///
    /// # Errors
    /// When Sphinx cannot be reached.
    pub async fn work(
        &mut self,
        job: &Job,
        storage: u128,
        log: &mut impl FnMut(Event),
    ) -> Result<Outcome, Failure> {
        log(Event::Found { job: job.id.clone(), reward: job.reward, bond: job.bond });
        let task = match self.sphinx.spec(job).await {
            Ok(t) => t,
            Err(f) => {
                log(Event::Skipped { job: job.id.clone(), why: f.message });
                return Ok(Outcome::Skipped);
            }
        };
        let mut messages = vec![Message::system(SYSTEM), Message::user(format!("Task:\n{task}"))];
        // Attempts count submissions. Model replies that never reach the
        // chain (unparsable, failing the local check, or repeating earlier
        // code) cost no bond but are bounded too.
        let attempts = self.limits.attempts.min(job.max_subs_per_agent).max(1);
        let max_asks = attempts.saturating_mul(ASKS_PER_ATTEMPT);
        let mut tried: HashSet<String> = HashSet::new();
        let mut submitted = 0u32;
        let mut asks = 0u32;
        while submitted < attempts {
            if asks >= max_asks {
                log(Event::Gave {
                    job: job.id.clone(),
                    why: format!("{asks} replies without a new solution to submit"),
                });
                return Ok(Outcome::NotPaid);
            }
            asks += 1;
            let attempt = asks;
            let needed = job.bond.saturating_add(storage);
            let left = self.limits.budget.saturating_sub(self.spent);
            if needed > left {
                log(Event::OutOfBudget { needed, left });
                return Ok(Outcome::NotPaid);
            }
            log(Event::Asking { model: self.model.name().to_owned(), attempt });
            let reply = match self.model.complete(&messages).await {
                Ok(r) => r,
                Err(e) => {
                    log(Event::Gave { job: job.id.clone(), why: e });
                    return Ok(Outcome::NotPaid);
                }
            };
            messages.push(Message::assistant(reply.clone()));
            let solution = match self.vet(&reply, &tried, log).await? {
                Ok(s) => s,
                Err(ask_again) => {
                    messages.push(Message::user(ask_again));
                    continue;
                }
            };
            let sub = match self.sphinx.submit(&job.id, &solution.code).await {
                Ok(s) => s,
                // Submitted in an earlier run: the contract refuses it before any bond moves.
                Err(f) if f.code.as_deref() == Some("SPHINX_E_SUBMIT_DUPLICATE_CODE") => {
                    tried.insert(solution.code);
                    log(Event::Repeated);
                    messages.push(Message::user(REPEATED.to_owned()));
                    continue;
                }
                Err(f) => {
                    log(Event::Gave { job: job.id.clone(), why: f.to_string() });
                    return Ok(Outcome::NotPaid);
                }
            };
            tried.insert(solution.code);
            submitted += 1;
            self.spent = self.spent.saturating_add(sub.deposit);
            let sub_id = sub.sub_id;
            log(Event::Submitted { job: job.id.clone(), sub });
            let status = self.wait_for_score(&job.id, sub_id).await?;
            log(Event::Scored { job: job.id.clone(), sub_id, status: status.clone() });
            match status.status.as_str() {
                "Passed" => {
                    log(Event::Paid { job: job.id.clone(), reward: job.reward });
                    return Ok(Outcome::Paid);
                }
                "Failed" if status.job_status == "Open" => {
                    messages.push(Message::user(score_feedback(&status)));
                }
                _ => {
                    log(Event::Gave {
                        job: job.id.clone(),
                        why: format!("the job is {}", status.job_status),
                    });
                    return Ok(Outcome::NotPaid);
                }
            }
        }
        log(Event::Gave { job: job.id.clone(), why: "no attempts left".into() });
        Ok(Outcome::NotPaid)
    }

    /// Turns a reply into code worth a bond, or into the message that asks
    /// the model again: unparsable replies, code already submitted, and code
    /// that fails the free local check never reach the chain.
    async fn vet(
        &self,
        reply: &str,
        tried: &HashSet<String>,
        log: &mut impl FnMut(Event),
    ) -> Result<Result<Solution, String>, Failure> {
        let solution = match parse_solution(reply) {
            Ok(s) => s,
            Err(why) => {
                log(Event::CheckFailed { reason: why.clone() });
                return Ok(Err(format!("{why}. Reply again with only the JSON object.")));
            }
        };
        // Every candidate can be kept for debugging, submitted or not.
        if let Ok(dir) = std::env::var("SPHINX_AGENT_KEEP_CODE") {
            let name = format!("{}.js", sphinx_protocol::Hash32::digest(solution.code.as_bytes()));
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(std::path::Path::new(&dir).join(name), &solution.code);
        }
        if tried.contains(&solution.code) {
            log(Event::Repeated);
            return Ok(Err(REPEATED.to_owned()));
        }
        if let Err(reason) =
            self.sphinx.check(&solution.code, &solution.entry, &solution.examples).await?
        {
            log(Event::CheckFailed { reason: reason.clone() });
            return Ok(Err(format!("The code was not submitted: {reason}. Fix it.")));
        }
        Ok(Ok(solution))
    }

    async fn wait_for_score(&self, job: &str, sub_id: u32) -> Result<Status, Failure> {
        let deadline = tokio::time::Instant::now() + self.limits.wait;
        loop {
            // The bond is already in: an RPC hiccup while waiting is retried,
            // not a reason to walk away from the score.
            let status = match self.sphinx.status(job, sub_id).await {
                Ok(s) => s,
                Err(f)
                    if f.code.as_deref() == Some("SPHINX_E_RELAY_RPC_FAILED")
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(self.limits.poll).await;
                    continue;
                }
                Err(f) => return Err(f),
            };
            if status.status != "Pending" || tokio::time::Instant::now() >= deadline {
                return Ok(status);
            }
            tokio::time::sleep(self.limits.poll).await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unused_async_trait_impl, reason = "the fake answers at once")]
mod tests {
    use super::*;
    use crate::model::Scripted;
    use std::sync::Mutex;

    const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

    /// A job that passes code containing "good" and fails anything else.
    struct FakeSphinx {
        submitted: Mutex<Vec<String>>,
    }

    impl Sphinx for FakeSphinx {
        async fn open_jobs(&self) -> Result<Vec<Job>, Failure> {
            Ok(vec![])
        }
        async fn spec(&self, _: &Job) -> Result<String, Failure> {
            Ok("Write function f(n).".into())
        }
        async fn check(
            &self,
            code: &str,
            entry: &str,
            _: &[serde_json::Value],
        ) -> Result<Result<(), String>, Failure> {
            Ok(if code.contains(&format!("function {entry}")) {
                Ok(())
            } else {
                Err("entry not defined".into())
            })
        }
        async fn submit(&self, _: &str, code: &str) -> Result<Submitted, Failure> {
            let mut s = self.submitted.lock().unwrap();
            s.push(code.to_owned());
            Ok(Submitted {
                sub_id: u32::try_from(s.len() - 1).unwrap(),
                tx_hash: "tx".into(),
                deposit: NEAR / 50,
                gas_paid_by: "sphinx".into(),
            })
        }
        async fn status(&self, _: &str, sub_id: u32) -> Result<Status, Failure> {
            let s = self.submitted.lock().unwrap();
            let good = s[sub_id as usize].contains("good");
            Ok(Status {
                status: if good { "Passed" } else { "Failed" }.into(),
                passed: if good { 12 } else { 7 },
                total: 12,
                job_status: if good { "Paid" } else { "Open" }.into(),
            })
        }
    }

    fn job() -> Job {
        Job {
            id: "7".into(),
            reward: NEAR / 2,
            bond: NEAR / 100,
            deadline_ns: u64::MAX,
            spec_uri: "http://x".into(),
            max_subs_per_agent: 3,
        }
    }

    fn limits(budget: u128) -> Limits {
        Limits {
            budget,
            attempts: 3,
            min_reward: 0,
            max_bond_bps: 10_000,
            poll: Duration::from_millis(1),
            wait: Duration::from_millis(50),
        }
    }

    fn answer(code: &str) -> String {
        serde_json::json!({ "entry": "f", "code": code }).to_string()
    }

    #[tokio::test]
    async fn retries_with_the_score_as_feedback_until_paid() {
        let model = Scripted::new([
            answer("function f(n){return 0} // bad"),
            answer("function f(n){return n} // good"),
        ]);
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let mut agent = Agent::new(&model, &sphinx, limits(NEAR));
        let mut events = vec![];
        let paid =
            agent.work(&job(), NEAR / 100, &mut |e| events.push(e)).await.unwrap() == Outcome::Paid;
        assert!(paid);
        assert_eq!(sphinx.submitted.lock().unwrap().len(), 2);
        let second = model.seen.lock().unwrap()[1].clone();
        assert!(second.last().unwrap().content.contains("passed 7 of 12"), "the score is fed back");
        assert!(events.iter().any(|e| matches!(e, Event::Paid { .. })));
        assert_eq!(agent.spent(), 2 * (NEAR / 50));
    }

    #[tokio::test]
    async fn code_that_fails_the_local_check_is_never_submitted() {
        let model =
            Scripted::new([answer("const g = 1"), answer("function f(n){return n} // good")]);
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let mut agent = Agent::new(&model, &sphinx, limits(NEAR));
        assert_eq!(agent.work(&job(), NEAR / 100, &mut |_| {}).await.unwrap(), Outcome::Paid);
        assert_eq!(sphinx.submitted.lock().unwrap().len(), 1, "only the good code was submitted");
    }

    fn job_paying(id: &str, reward: u128, bond: u128) -> Job {
        Job {
            id: id.into(),
            reward,
            bond,
            deadline_ns: u64::MAX,
            spec_uri: String::new(),
            max_subs_per_agent: 3,
        }
    }

    #[test]
    fn skips_jobs_below_the_minimum_or_with_a_bond_too_large_for_the_reward() {
        let model = Scripted::new(Vec::<String>::new());
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let limits = Limits { min_reward: NEAR / 2, max_bond_bps: 2_000, ..limits(10 * NEAR) };
        let agent = Agent::new(&model, &sphinx, limits);
        let jobs = [
            job_paying("cheap", NEAR / 10, NEAR / 100),
            job_paying("trap", NEAR, NEAR / 2),
            job_paying("fair", NEAR, NEAR / 10),
        ];
        assert_eq!(agent.choose(&jobs, &[], 0).map(|j| j.id.as_str()), Some("fair"));
    }

    #[tokio::test]
    async fn stops_at_the_budget() {
        let model = Scripted::new([
            answer("function f(){} // bad"),
            answer("function f(){} // bad"),
            answer("function f(){} // bad"),
        ]);
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        // Room for exactly one bond plus storage.
        let mut agent = Agent::new(&model, &sphinx, limits(NEAR / 50));
        let mut events = vec![];
        assert_eq!(
            agent.work(&job(), NEAR / 100, &mut |e| events.push(e)).await.unwrap(),
            Outcome::NotPaid
        );
        assert_eq!(sphinx.submitted.lock().unwrap().len(), 1);
        assert!(events.iter().any(|e| matches!(e, Event::OutOfBudget { .. })));
    }

    #[test]
    fn parses_fenced_and_plain_replies() {
        let fenced = "```json\n{\"entry\":\"f\",\"code\":\"function f(){}\"}\n```";
        assert_eq!(parse_solution(fenced).unwrap().entry, "f");
        assert!(parse_solution("no json here").is_err());
        assert!(parse_solution("{\"entry\":\"\",\"code\":\"x\"}").is_err());
    }

    #[test]
    fn chooses_the_best_affordable_job() {
        let model = Scripted::new(Vec::<String>::new());
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let agent = Agent::new(&model, &sphinx, limits(NEAR / 10));
        let cheap = Job { id: "1".into(), reward: NEAR / 10, ..job() };
        let rich = Job { id: "2".into(), reward: NEAR, ..job() };
        let pricey = Job { id: "3".into(), reward: 5 * NEAR, bond: NEAR, ..job() };
        let jobs = [cheap, rich, pricey];
        assert_eq!(agent.choose(&jobs, &[], NEAR / 100).unwrap().id, "2");
        assert_eq!(agent.choose(&jobs, &["2".into()], NEAR / 100).unwrap().id, "1");
    }

    #[tokio::test]
    async fn repeated_code_is_never_resubmitted_and_costs_no_attempt() {
        let model = Scripted::new([
            answer("function f(n){return 0} // bad"),
            answer("function f(n){return 0} // bad"),
            answer("function f(n){return n} // good"),
        ]);
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let mut agent = Agent::new(&model, &sphinx, Limits { attempts: 2, ..limits(NEAR) });
        let mut events = vec![];
        assert_eq!(
            agent.work(&job(), NEAR / 100, &mut |e| events.push(e)).await.unwrap(),
            Outcome::Paid
        );
        assert_eq!(sphinx.submitted.lock().unwrap().len(), 2, "the repeat was not submitted");
        assert!(events.iter().any(|e| matches!(e, Event::Repeated)));
        let third = model.seen.lock().unwrap()[2].clone();
        assert!(third.last().unwrap().content.contains("already submitted"));
    }

    #[tokio::test]
    async fn a_model_that_only_repeats_is_given_up_on() {
        let model = Scripted::new(vec![answer("function f(n){return 0} // bad"); 10]);
        let sphinx = FakeSphinx { submitted: Mutex::default() };
        let mut agent = Agent::new(&model, &sphinx, Limits { attempts: 2, ..limits(NEAR) });
        let mut events = vec![];
        assert_eq!(
            agent.work(&job(), NEAR / 100, &mut |e| events.push(e)).await.unwrap(),
            Outcome::NotPaid
        );
        assert_eq!(sphinx.submitted.lock().unwrap().len(), 1);
        assert_eq!(model.seen.lock().unwrap().len(), 6, "bounded at 3 replies per attempt");
    }
}
