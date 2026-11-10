//! Drafting a job from a plain request.
//!
//! The model writes the task description, a reference solution and the test
//! inputs. Every expected value is then computed by running the reference
//! in the grader's own sandbox, so the suite and the reference cannot
//! disagree, and the examples agents see are rendered from the same values.
//! A second model call solves the task from the description alone; wherever
//! its answers differ from the reference, the model reviews the case and
//! fixes the reference or makes the description exact.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sphinx_grader::executor::Budget;
use sphinx_grader::js::{Evaluation, JsBoaRuntime};
use sphinx_grader::limits::RUN_POLLS;
use sphinx_model::{Message, Model};
use sphinx_protocol::suite::{MAX_TESTS, SUITE_VERSION};
use sphinx_protocol::{Code, SphinxError, RUNTIME_JS_BOA_V1};
use std::fmt::Write as _;

/// Fewest tests a draft may have.
pub const MIN_TESTS: usize = 12;
/// Rounds the model gets to make its reference run on every input.
pub const WRITE_ROUNDS: u32 = 3;
/// Rounds the model gets to settle disagreements with the second opinion.
pub const REVIEW_ROUNDS: u32 = 3;
/// A rough look for regular expressions (a literal after `(`, `=`, `,`,
/// `:` or `return`, or the `RegExp` constructor), for a hint only.
pub fn uses_regex(code: &str) -> bool {
    if code.contains("RegExp(") {
        return true;
    }
    let bytes = code.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        if b != b'/' || bytes.get(i + 1).is_some_and(|n| *n == b'/' || *n == b'*') {
            return false;
        }
        let before = code[..i].trim_end();
        ['(', '=', ',', ':'].iter().any(|c| before.ends_with(*c)) || before.ends_with("return")
    })
}

/// Disagreements shown to the model per review, to keep the prompt small.
const SHOWN_DIFFERENCES: usize = 8;

const AUTHOR: &str = "You turn a buyer's request into a programming job that AI agents solve and hidden tests grade. \
Reply with only one JSON object and nothing else:\n\
{\"title\": \"a short title in sentence case, under 60 characters\",\n \
\"description\": \"Markdown task statement. Name the function as `function NAME(...)`. Give every parameter's type and range, the return value, every rule and edge case, and exactly what to return for invalid input. No examples section: examples are added from the tests. Plain JavaScript, no imports or require.\",\n \
\"entry\": \"the function name\",\n \
\"reference\": \"complete, correct JavaScript that defines the function; deterministic, no Date, no Math.random, no imports, no regular expressions (they are disabled and throw; use indexOf, slice, split with a string, charCodeAt)\",\n \
\"inputs\": [[\"argument lists\", \"one per test\"]],\n \
\"examples\": [0, 1, 2],\n \
\"skills\": [\"javascript\"]}\n\
Rules: the description must also tell agents that regular expressions are disabled. Give 24 to 32 distinct argument lists covering typical cases, every rule, boundaries and invalid input. \
Arguments and return values must be plain JSON (numbers, strings, booleans, null, arrays, objects): \
never NaN, Infinity, undefined or functions; to test a wrong type, pass a string, a boolean, null, an array or an object. \
`examples` are the indices of 3 inputs shown to agents. `skills` are 1 to 4 lowercase skills an agent needs.";

const SOLVER: &str =
    "You write a JavaScript function for the task below. Plain JavaScript, no imports or require. \
Reply with only one JSON object: {\"entry\": \"function name\", \"code\": \"the complete code\"}.";

/// What the drafting is doing, for a progress display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Asking the model to write (round 1) or fix (later rounds) the draft.
    Writing { round: u32 },
    /// The reference failed on some inputs; asking for a fix.
    ReferenceFailed { failures: usize },
    /// Asking a fresh model call to solve the task from the description.
    SecondOpinion,
    /// The second opinion matched the reference on `agreed` of `total` tests.
    Compared { agreed: usize, total: usize },
    /// Asking a third solver to break the ties.
    ThirdOpinion,
    /// Asking the model to settle the differences.
    Reviewing { round: u32, differences: usize },
}

/// How a fresh solver that read only the description compares with the reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecondOpinion {
    pub agreed: usize,
    pub total: usize,
    /// Review rounds used.
    pub rounds: u32,
}

/// A test input where the second opinion still differs from the reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Difference {
    pub args: Vec<Value>,
    pub reference: Value,
    pub second_opinion: Value,
    /// A third run's answer, asked to break the tie; it sees only the task
    /// text and the public examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub third_opinion: Option<Value>,
}

/// A job ready to post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// Derived from the content, so the same draft always has the same id.
    pub id: String,
    pub title: String,
    /// What agents read: the description, the rendered examples and the test count.
    pub spec: String,
    pub entry: String,
    /// Hidden from agents.
    pub reference: String,
    /// The hidden suite as `sphinx.suite/1` JSON text.
    pub suite: String,
    pub tests: usize,
    pub skills: Vec<String>,
    pub second_opinion: SecondOpinion,
    /// Inputs the second opinion still answers differently, and disputed
    /// inputs a redraft dropped instead of settling. They are hidden test
    /// inputs, so they are shown only to the buyer. A draft with any is not
    /// posted.
    pub differences: Vec<Difference>,
    /// Whether the task text was rewritten while settling differences, so
    /// the buyer reads the final text rather than the first one.
    #[serde(default)]
    pub task_rewritten: bool,
}

/// The model's draft before any checking.
#[derive(Debug, Clone, Deserialize)]
struct Written {
    title: String,
    description: String,
    entry: String,
    reference: String,
    inputs: Vec<Vec<Value>>,
    #[serde(default)]
    examples: Vec<usize>,
    #[serde(default)]
    skills: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Solved {
    code: String,
}

fn model_failed(why: &str) -> SphinxError {
    SphinxError::new(Code::ConciergeModelUnavailable, why.to_owned())
}

fn no_draft(rounds: u32, why: &str) -> SphinxError {
    SphinxError::new(Code::ConciergeNoDraft, why.to_owned()).ctx("rounds", rounds)
}

/// The first JSON object in a reply, with or without a code fence.
fn json_object<T: for<'de> Deserialize<'de>>(reply: &str) -> Result<T, String> {
    let start = reply.find('{').ok_or("the reply has no JSON object")?;
    let end = reply.rfind('}').ok_or("the reply has no JSON object")?;
    if end < start {
        return Err("the reply has no JSON object".into());
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| format!("the JSON does not fit: {e}"))
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

impl Written {
    /// Checks the shape and drops repeated inputs.
    fn tidy(mut self) -> Result<Self, String> {
        if !is_identifier(&self.entry) {
            return Err(format!("entry {:?} is not a JavaScript function name", self.entry));
        }
        let mut seen = Vec::new();
        self.inputs.retain(|i| {
            let fresh = !seen.contains(i);
            if fresh {
                seen.push(i.clone());
            }
            fresh
        });
        self.inputs.truncate(MAX_TESTS);
        if self.inputs.len() < MIN_TESTS {
            return Err(format!(
                "there are {} distinct inputs; give at least {MIN_TESTS}",
                self.inputs.len()
            ));
        }
        self.examples.retain(|&i| i < self.inputs.len());
        self.examples.dedup();
        if self.examples.is_empty() {
            self.examples = (0..3.min(self.inputs.len())).collect();
        }
        self.skills = self
            .skills
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty() && s.chars().count() <= 32)
            .take(4)
            .collect();
        // Every job runs JavaScript, so it is always a needed skill.
        if !self.skills.iter().any(|s| s == "javascript") {
            self.skills.insert(0, "javascript".into());
            self.skills.truncate(4);
        }
        Ok(self)
    }
}

/// Runs `code` on every input under one shared budget, as the grader does.
fn run_all(code: &str, entry: &str, inputs: &[Vec<Value>]) -> Vec<Evaluation> {
    let runtime = JsBoaRuntime { epoch_ms: 0 };
    let mut budget = Budget::new(RUN_POLLS);
    inputs
        .iter()
        .map(|args| match runtime.evaluate(code, entry, args, &mut budget) {
            Evaluation::Returned(v) => Evaluation::Returned(whole_numbers(v)),
            other => other,
        })
        .collect()
}

/// Largest integer a JavaScript number holds exactly (2^53 - 1).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Writes whole JavaScript numbers as integers (`5400`, not `5400.0`), so
/// the suite and the examples read as a person would write them.
fn whole_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) => match n.as_f64() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "checked whole and within the safe range"
            )]
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER => {
                Value::from(f as i64)
            }
            _ => Value::Number(n),
        },
        Value::Array(a) => Value::Array(a.into_iter().map(whole_numbers).collect()),
        Value::Object(o) => {
            Value::Object(o.into_iter().map(|(k, v)| (k, whole_numbers(v))).collect())
        }
        other => other,
    }
}

fn describe(e: &Evaluation) -> String {
    match e {
        Evaluation::Returned(v) => v.to_string(),
        Evaluation::Unreadable => "a value JSON cannot carry".into(),
        Evaluation::Threw => "an exception".into(),
        Evaluation::Aborted => "running out of time".into(),
        Evaluation::NotLoaded(d) => d.message.clone(),
    }
}

fn call_text(entry: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    format!("{entry}({})", args.join(", "))
}

/// The description agents read: the model's text, examples rendered from
/// the reference's real outputs, and the test count.
fn render_spec(w: &Written, outputs: &[Value]) -> String {
    let mut spec = format!("# {}\n\n{}\n\n## Examples\n\n", w.title.trim(), w.description.trim());
    for &i in &w.examples {
        let _ = writeln!(spec, "    {} // {}", call_text(&w.entry, &w.inputs[i]), outputs[i]);
    }
    let _ = write!(
        spec,
        "\nThe hidden suite has {} tests. You are paid when every hidden test passes.\n",
        w.inputs.len()
    );
    spec
}

fn suite_json(entry: &str, inputs: &[Vec<Value>], outputs: &[Value]) -> String {
    let tests: Vec<Value> = inputs
        .iter()
        .zip(outputs)
        .map(|(args, expected)| json!({ "args": args, "expected": expected }))
        .collect();
    json!({ "version": SUITE_VERSION, "runtime": RUNTIME_JS_BOA_V1, "entry": entry, "tests": tests })
        .to_string()
}

fn draft_id(spec: &str, suite: &str, reference: &str) -> String {
    let mut h = Sha256::new();
    for part in [spec, suite, reference] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    hex::encode(&h.finalize()[..8])
}

/// Drafts jobs with a model.
#[derive(Debug)]
pub struct Concierge<'a, M: Model> {
    model: &'a M,
}

impl<'a, M: Model> Concierge<'a, M> {
    pub fn new(model: &'a M) -> Self {
        Self { model }
    }

    async fn ask(&self, messages: &[Message]) -> Result<String, SphinxError> {
        self.model.complete(messages).await.map_err(|e| model_failed(&e))
    }

    /// Asks until the reference runs on every input; returns the draft and its outputs.
    async fn write(
        &self,
        conversation: &mut Vec<Message>,
        progress: &mut impl FnMut(Progress),
    ) -> Result<(Written, Vec<Value>), SphinxError> {
        let mut last = String::from("the model never answered");
        for round in 1..=WRITE_ROUNDS {
            progress(Progress::Writing { round });
            let reply = self.ask(conversation).await?;
            conversation.push(Message::assistant(reply.clone()));
            let written = match json_object::<Written>(&reply).and_then(Written::tidy) {
                Ok(w) => w,
                Err(why) => {
                    last.clone_from(&why);
                    conversation.push(Message::user(format!(
                        "{why}. Reply again with the complete JSON object, valid JSON only: \
                         no NaN, Infinity, undefined, comments or trailing commas."
                    )));
                    continue;
                }
            };
            let results = run_all(&written.reference, &written.entry, &written.inputs);
            let failures: Vec<String> = written
                .inputs
                .iter()
                .zip(&results)
                .filter(|(_, r)| !matches!(r, Evaluation::Returned(_)))
                .map(|(args, r)| {
                    format!("{} ended in {}", call_text(&written.entry, args), describe(r))
                })
                .collect();
            if failures.is_empty() {
                let outputs = results
                    .into_iter()
                    .map(|r| match r {
                        Evaluation::Returned(v) => v,
                        _ => Value::Null,
                    })
                    .collect();
                return Ok((written, outputs));
            }
            progress(Progress::ReferenceFailed { failures: failures.len() });
            last = format!("the reference failed on {} inputs", failures.len());
            let hint = if uses_regex(&written.reference) {
                "\nYour reference uses a regular expression. They are disabled in the grader and throw: \
                 rewrite it with string methods (indexOf, slice, split with a string, charCodeAt)."
            } else {
                ""
            };
            conversation.push(Message::user(format!(
                "Your reference did not return a JSON value for these inputs:\n{}{hint}\n\
                 Fix the reference, or replace inputs that are outside the task, and reply with the complete JSON object.",
                failures.join("\n")
            )));
        }
        Err(no_draft(WRITE_ROUNDS, &format!("no draft ran on every input: {last}")))
    }

    /// A fresh solver's code for `spec`, or `None` if it gave no usable code.
    async fn second_opinion(&self, spec: &str) -> Result<Option<String>, SphinxError> {
        let reply = self.ask(&[Message::system(SOLVER), Message::user(spec.to_owned())]).await?;
        Ok(json_object::<Solved>(&reply).ok().map(|s| s.code))
    }

    /// Drafts a job for `request`.
    ///
    /// # Errors
    /// `SPHINX_E_CONCIERGE_MODEL_UNAVAILABLE` when the model fails, and
    /// `SPHINX_E_CONCIERGE_NO_DRAFT` when no reference runs on every input.
    pub async fn draft(
        &self,
        request: &str,
        progress: &mut impl FnMut(Progress),
    ) -> Result<Draft, SphinxError> {
        let request = request.trim();
        if request.is_empty() {
            return Err(SphinxError::new(Code::CliUsage, "describe the task you want done"));
        }
        let mut conversation =
            vec![Message::system(AUTHOR), Message::user(format!("The buyer asks:\n{request}"))];
        let (mut written, mut outputs) = self.write(&mut conversation, progress).await?;
        let mut spec = render_spec(&written, &outputs);
        progress(Progress::SecondOpinion);
        let mut solver = self.second_opinion(&spec).await?;
        let mut rounds = 0;
        let mut differences;
        // Every input ever disputed, with the answers at the time, so a
        // redraft cannot settle a dispute by deleting the input.
        let mut disputed: Vec<Difference> = Vec::new();
        let first_description = written.description.clone();
        loop {
            differences = compare(&written, &outputs, solver.as_deref());
            for d in &differences {
                if !disputed.iter().any(|x| x.args == d.args) {
                    disputed.push(d.clone());
                }
            }
            progress(Progress::Compared {
                agreed: written.inputs.len() - differences.len(),
                total: written.inputs.len(),
            });
            if differences.is_empty() || rounds == REVIEW_ROUNDS {
                break;
            }
            rounds += 1;
            progress(Progress::ThirdOpinion);
            let third = self.second_opinion(&spec).await?;
            break_ties(&written.entry, &mut differences, third.as_deref());
            progress(Progress::Reviewing { round: rounds, differences: differences.len() });
            conversation.push(Message::user(review_prompt(&written.entry, &differences)));
            let before = written.description.clone();
            (written, outputs) = self.write(&mut conversation, progress).await?;
            spec = render_spec(&written, &outputs);
            if written.description != before || solver.is_none() {
                progress(Progress::SecondOpinion);
                solver = self.second_opinion(&spec).await?;
            }
        }
        for mut d in disputed {
            if !written.inputs.contains(&d.args) {
                d.reference = Value::String("dropped by the redraft".into());
                differences.push(d);
            }
        }
        let suite = suite_json(&written.entry, &written.inputs, &outputs);
        let score = sphinx_grader::local_score(&suite, &written.reference, 0)?;
        if score.passed != score.total {
            return Err(SphinxError::new(
                Code::InternalInvariant,
                "the reference does not pass the suite computed from it",
            ));
        }
        Ok(Draft {
            id: draft_id(&spec, &suite, &written.reference),
            title: written.title.trim().to_owned(),
            entry: written.entry.clone(),
            tests: written.inputs.len(),
            skills: written.skills.clone(),
            task_rewritten: written.description != first_description,
            second_opinion: SecondOpinion {
                agreed: written.inputs.len().saturating_sub(differences.len()),
                total: written.inputs.len(),
                rounds,
            },
            differences,
            spec,
            suite,
            reference: written.reference,
        })
    }
}

/// Inputs where `solver` differs from the reference's outputs. Without
/// usable second-opinion code, every input counts as a difference.
fn compare(w: &Written, outputs: &[Value], solver: Option<&str>) -> Vec<Difference> {
    let theirs: Vec<Option<Value>> = match solver {
        Some(code) => run_all(code, &w.entry, &w.inputs)
            .into_iter()
            .map(|r| match r {
                Evaluation::Returned(v) => Some(v),
                _ => None,
            })
            .collect(),
        None => vec![None; w.inputs.len()],
    };
    w.inputs
        .iter()
        .zip(outputs)
        .zip(theirs)
        .filter(|((_, ours), theirs)| {
            !theirs.as_ref().is_some_and(|t| sphinx_grader::values_equal(ours, t))
        })
        .map(|((args, ours), theirs)| Difference {
            args: args.clone(),
            reference: ours.clone(),
            second_opinion: theirs.unwrap_or(Value::String("no answer".into())),
            third_opinion: None,
        })
        .collect()
}

/// Asks a third solver on the disputed inputs and records its answers.
fn break_ties(entry: &str, differences: &mut [Difference], third: Option<&str>) {
    let Some(code) = third else { return };
    let inputs: Vec<Vec<Value>> = differences.iter().map(|d| d.args.clone()).collect();
    for (d, r) in differences.iter_mut().zip(run_all(code, entry, &inputs)) {
        if let Evaluation::Returned(v) = r {
            d.third_opinion = Some(v);
        }
    }
}

fn review_prompt(entry: &str, differences: &[Difference]) -> String {
    let shown: Vec<String> = differences
        .iter()
        .take(SHOWN_DIFFERENCES)
        .map(|d| {
            let call = call_text(entry, &d.args);
            let two_agree = d
                .third_opinion
                .as_ref()
                .is_some_and(|t| sphinx_grader::values_equal(t, &d.second_opinion));
            if two_agree {
                format!(
                    "{call} — two separate solver runs both answered {}; your reference answered {}",
                    d.second_opinion, d.reference
                )
            } else {
                let third = d
                    .third_opinion
                    .as_ref()
                    .map_or_else(|| "no answer".to_owned(), Value::to_string);
                format!(
                    "{call} — your reference: {}, a second solver: {}, a third solver: {third}",
                    d.reference, d.second_opinion
                )
            }
        })
        .collect();
    format!(
        "Independent solvers who read only your description answered {} inputs differently:\n{}\n\
         Where two separate solver runs, which saw only the task text, agree against your reference, your reference is most likely wrong: \
         work the case out by hand from the buyer's request and fix the reference. \
         If the description leaves the answer open, make it say exactly what to return. \
         Reply with the complete JSON object again.",
        differences.len(),
        shown.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sphinx_model::Scripted;

    fn written(reference: &str, description: &str) -> String {
        let inputs: Vec<Value> = (0..14).map(|n| json!([n])).collect();
        json!({
            "title": "Double a number",
            "description": description,
            "entry": "double",
            "reference": reference,
            "inputs": inputs,
            "examples": [1, 2, 40],
            "skills": ["JavaScript", ""]
        })
        .to_string()
    }

    fn written_with(reference: &str, description: &str, n: i64) -> String {
        let mut w: Value = serde_json::from_str(&written(reference, description)).unwrap();
        w["inputs"] = (0..n).map(|i| json!([i])).collect::<Vec<_>>().into();
        w.to_string()
    }

    fn solved(code: &str) -> String {
        json!({ "entry": "double", "code": code }).to_string()
    }

    #[tokio::test]
    async fn expected_values_come_from_the_reference_and_agree_with_a_second_opinion() {
        let model = Scripted::new([
            written("function double(n){ return n * 2 }", "Write `function double(n)`."),
            solved("function double(n){ return n + n }"),
        ]);
        let mut seen = Vec::new();
        let d =
            Concierge::new(&model).draft("double numbers", &mut |p| seen.push(p)).await.unwrap();
        assert_eq!((d.tests, d.second_opinion.agreed, d.second_opinion.rounds), (14, 14, 0));
        assert_eq!(d.differences, []);
        assert!(
            d.spec.contains("double(2) // 4"),
            "examples rendered from real outputs:\n{}",
            d.spec
        );
        assert!(d.spec.contains("hidden suite has 14 tests"));
        assert_eq!(d.skills, ["javascript"]);
        let score = sphinx_grader::local_score(&d.suite, &d.reference, 0).unwrap();
        assert_eq!(score.passed, 14);
        assert!(seen.contains(&Progress::Compared { agreed: 14, total: 14 }));
    }

    #[tokio::test]
    async fn a_reference_that_throws_is_sent_back_before_anything_else() {
        let model = Scripted::new([
            written("function double(n){ if (n > 5) throw 1; return n * 2 }", "Write `double`."),
            written("function double(n){ return n * 2 }", "Write `double`."),
            solved("function double(n){ return 2 * n }"),
        ]);
        let mut seen = Vec::new();
        let d = Concierge::new(&model).draft("double", &mut |p| seen.push(p)).await.unwrap();
        assert!(seen.contains(&Progress::ReferenceFailed { failures: 8 }));
        assert_eq!(d.second_opinion.agreed, 14);
        let fix = &model.seen.lock().unwrap()[1];
        assert!(fix.last().unwrap().content.contains("double(6) ended in an exception"));
    }

    #[tokio::test]
    async fn disagreements_are_reviewed_and_a_changed_description_gets_a_new_opinion() {
        let model = Scripted::new([
            written("function double(n){ return n * 2 }", "Write `double(n)`."),
            solved("function double(n){ return n < 10 ? n * 2 : 0 }"),
            // The tie-breaker sides with the second solver.
            solved("function double(n){ return n < 10 ? 2 * n : 0 }"),
            written(
                "function double(n){ return n * 2 }",
                "Write `double(n)`: return n times two for every n.",
            ),
            solved("function double(n){ return n * 2 }"),
        ]);
        let mut seen = Vec::new();
        let d = Concierge::new(&model).draft("double", &mut |p| seen.push(p)).await.unwrap();
        assert!(seen.contains(&Progress::Reviewing { round: 1, differences: 4 }));
        assert_eq!((d.second_opinion.agreed, d.second_opinion.rounds), (14, 1));
        assert!(seen.contains(&Progress::ThirdOpinion));
        let review = &model.seen.lock().unwrap()[3];
        assert!(review.last().unwrap().content.contains(
            "double(10) — two separate solver runs both answered 0; your reference answered 20"
        ));
    }

    #[tokio::test]
    async fn a_redraft_that_drops_disputed_inputs_does_not_settle_them() {
        let model = Scripted::new([
            written_with("function double(n){ return n * 2 }", "Write `double(n)`.", 14),
            // The second solver disagrees on 12 and 13.
            solved("function double(n){ return n < 12 ? n * 2 : 0 }"),
            solved("function double(n){ return n < 12 ? 2 * n : 0 }"),
            // The redraft keeps the reference and quietly drops 12 and 13.
            written_with("function double(n){ return n * 2 }", "Write `double(n)`.", 12),
        ]);
        let d = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap();
        assert_eq!(d.tests, 12);
        let dropped: Vec<_> = d.differences.iter().map(|x| x.args.clone()).collect();
        assert_eq!(dropped, [vec![json!(12)], vec![json!(13)]]);
        assert!(d.differences.iter().all(|x| x.reference == "dropped by the redraft"));
        assert!(!d.task_rewritten);
    }

    #[tokio::test]
    async fn unsettled_differences_are_reported_not_hidden() {
        let reference = written("function double(n){ return n * 2 }", "Write `double`.");
        let wrong = solved("function double(n){ return n === 0 ? 1 : n * 2 }");
        // The tie-breaker sides with the reference every round.
        let third = solved("function double(n){ return 2 * n }");
        let model = Scripted::new([
            reference.clone(),
            wrong.clone(),
            third.clone(),
            reference.clone(),
            third.clone(),
            reference.clone(),
            third,
            reference,
        ]);
        let d = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap();
        let last = model.seen.lock().unwrap()[7].last().unwrap().content.clone();
        assert!(
            last.contains("your reference: 0, a second solver: 1, a third solver: 0"),
            "{last}"
        );
        assert_eq!((d.second_opinion.agreed, d.second_opinion.rounds), (13, REVIEW_ROUNDS));
        assert_eq!(d.differences.len(), 1);
        assert_eq!(d.differences[0].args, [json!(0)]);
    }

    #[tokio::test]
    async fn a_model_that_never_writes_a_working_reference_gives_no_draft() {
        let model = Scripted::new(vec!["not json"; 3]);
        let e = Concierge::new(&model).draft("x", &mut |_| {}).await.unwrap_err();
        assert_eq!(e.code(), Code::ConciergeNoDraft);
    }

    #[test]
    fn whole_numbers_read_as_integers() {
        let v = whole_numbers(json!([5400.0, 1.5, -0.0, 1e300, { "a": 2.0 }, "3.0"]));
        assert_eq!(v.to_string(), r#"[5400,1.5,0,1e+300,{"a":2},"3.0"]"#);
    }

    #[test]
    fn too_few_inputs_and_bad_names_are_refused() {
        let few: Written = serde_json::from_value(json!({
            "title": "t", "description": "d", "entry": "f", "reference": "",
            "inputs": [[1], [1], [2]]
        }))
        .unwrap();
        assert!(few.tidy().unwrap_err().contains("2 distinct inputs"));
        assert!(!is_identifier("2f") && !is_identifier("a-b") && is_identifier("_f$1"));
    }
}
