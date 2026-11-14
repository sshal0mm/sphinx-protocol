//! Drafting a job from a plain request.
//!
//! The model writes the task description, a reference solution and the test
//! inputs. Every expected value is then computed by running the reference
//! in the grader's own sandbox, so the suite and the reference cannot
//! disagree, and the examples agents see are rendered from the same values.
//! Other models solve the task from the description alone; a third breaks
//! ties, the author fixes what the readings dispute, two readings that agree
//! against the reference replace it, and a case all three read differently
//! is left out. Code another model wrote from the final description must
//! pass every hidden test before the draft is offered.

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
\"description\": \"Markdown task statement. Name the function as `function NAME(...)`. Give every parameter's type and range, the return value, every rule and edge case, and exactly what to return for invalid input. List every input your reference rejects, for every parameter (wrong types included): agents follow the description literally, so a rejection it does not state fails them. No examples section: examples are added from the tests. Plain JavaScript, no imports or require.\",\n \
\"entry\": \"the function name\",\n \
\"reference\": \"complete, correct JavaScript that defines the function; deterministic, no Date, no Math.random, no imports, no regular expressions (they are disabled and throw; use indexOf, slice, split with a string, charCodeAt)\",\n \
\"inputs\": [[\"argument lists\", \"one per test\"]],\n \
\"examples\": [0, 1, 2],\n \
\"skills\": [\"javascript\"]}\n\
Rules: the description must also tell agents that regular expressions are disabled. Give 24 to 32 distinct argument lists covering typical cases, every rule, boundaries and invalid input. \
Arguments and return values must be plain JSON (numbers, strings, booleans, null, arrays, objects): \
never NaN, Infinity, undefined or functions; to test a wrong type, pass a string, a boolean, null, an array or an object. \
`examples` are the indices of 3 inputs shown to agents. `skills` are 1 to 4 lowercase skills an agent needs.";

/// The checking run follows the task text literally, so a test whose
/// expected answer rests on a rule the text never states (say, rejecting an
/// input type the rejection list does not name) shows up as a disagreement
/// and the description is made precise before anyone pays for it.
const SOLVER: &str =
    "You write a JavaScript function for the task below. Follow the description literally: \
apply only the rules it states, and treat an input as invalid (or reject it) only when the description \
explicitly says so; never add a check the description does not ask for. Plain JavaScript, no imports or require. \
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
    /// Leaving out tests the task text did not settle.
    Dropped { tests: usize },
    /// Two other models agreed against the reference on `tests` inputs, so
    /// the reference was replaced by the second model's code.
    ReferenceReplaced { tests: usize },
    /// Checking that a solution another model wrote from the final task text
    /// passes every hidden test.
    FinalCheck,
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
    /// posted. [`Concierge::draft`] leaves these out of the suite instead
    /// (see `dropped`), so a finished draft has none.
    pub differences: Vec<Difference>,
    /// Tests left out because the checks could not settle them from the task
    /// text: no hidden test rests on a rule the text does not state.
    #[serde(default)]
    pub dropped: Vec<Difference>,
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
    /// Indices of the inputs shown to agents. A model sometimes writes
    /// something else here; anything that is not a list of indices is
    /// dropped, and the first three inputs are shown instead.
    #[serde(default, deserialize_with = "indices_or_none")]
    examples: Vec<usize>,
    #[serde(default)]
    skills: Vec<String>,
}

fn indices_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<usize>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter().filter_map(|i| i.as_u64().and_then(|i| usize::try_from(i).ok())).collect()
        })
        .unwrap_or_default())
}

/// How many times a solver is asked before its run counts as no answer.
const SOLVE_ATTEMPTS: usize = 3;

/// Fresh pairs of solutions asked for when no earlier run passes the final suite.
const FINAL_ATTEMPTS: usize = 2;

/// At most one test in this many may be left out as unsettled; more means the
/// text does not say enough, and the buyer is asked to say it.
const MAX_DROPPED_PARTS: usize = 4;

/// Whether the second and third readings gave the same answer on a disputed input.
fn two_agree(d: &Difference) -> bool {
    d.third_opinion.as_ref().is_some_and(|t| sphinx_grader::values_equal(t, &d.second_opinion))
}

/// Whether a third reading agrees with the reference on a disputed input.
fn settled_by_third(d: &Difference) -> bool {
    d.third_opinion.as_ref().is_some_and(|t| sphinx_grader::values_equal(t, &d.reference))
}

/// Whether `code` mentions a function named `entry` (a cheap check; the
/// grader still decides whether it runs).
fn defines(code: &str, entry: &str) -> bool {
    code.contains(entry)
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

/// Keeps the tests marked in `keep`, and the shown examples among them;
/// when an example is left out, the first kept test with a non-null answer
/// takes its place, so the task still shows three examples.
fn retain_tests(w: &mut Written, outputs: &mut Vec<Value>, keep: &[bool]) {
    let mut new_index = Vec::with_capacity(keep.len());
    let mut n = 0;
    for &k in keep {
        new_index.push(k.then_some(n));
        n += usize::from(k);
    }
    let mut examples: Vec<usize> =
        w.examples.iter().filter_map(|&i| new_index.get(i).copied().flatten()).collect();
    let mut i = 0;
    w.inputs = w.inputs.drain(..).zip(keep).filter(|(_, k)| **k).map(|(a, _)| a).collect();
    *outputs = outputs.drain(..).zip(keep).filter(|(_, k)| **k).map(|(o, _)| o).collect();
    let wanted = w.examples.len().max(1).min(w.inputs.len());
    while examples.len() < wanted && i < outputs.len() {
        if !examples.contains(&i) && !outputs[i].is_null() {
            examples.push(i);
        }
        i += 1;
    }
    w.examples = examples;
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

/// Whether most inputs carry a string that is really JSON (an object or array
/// written as text) or a line break written as a literal backslash-n.
fn looks_double_encoded(inputs: &[Vec<Value>]) -> bool {
    let suspicious = |v: &Value| match v {
        Value::String(s) => {
            let t = s.trim();
            let json = (t.starts_with('{') || t.starts_with('['))
                && serde_json::from_str::<Value>(t).is_ok_and(|j| j.is_object() || j.is_array());
            json || (s.contains("\\n") && !s.contains('\n'))
        }
        _ => false,
    };
    let flagged = inputs.iter().filter(|args| args.iter().any(suspicious)).count();
    !inputs.is_empty() && flagged * 2 > inputs.len()
}

/// Drafts jobs with a model.
#[derive(Debug)]
pub struct Concierge<'a, M: Model> {
    model: &'a M,
    /// Reads only the task text and writes its own solution: preferably a
    /// different model from the author, so it cannot share the author's
    /// unstated assumptions.
    checker: Option<&'a M>,
    /// Breaks ties between the author and the checker: a third model.
    tiebreak: Option<&'a M>,
}

impl<'a, M: Model> Concierge<'a, M> {
    pub fn new(model: &'a M) -> Self {
        Self { model, checker: None, tiebreak: None }
    }

    /// Checks drafts with other models: `checker` solves each draft from its
    /// text alone, `tiebreak` settles inputs the two answer differently.
    #[must_use]
    pub fn with_checkers(mut self, checker: &'a M, tiebreak: &'a M) -> Self {
        self.checker = Some(checker);
        self.tiebreak = Some(tiebreak);
        self
    }

    async fn ask(&self, messages: &[Message]) -> Result<String, SphinxError> {
        self.model.complete(messages).await.map_err(|e| model_failed(&e))
    }

    /// A solution written by `model` from the task text alone.
    ///
    /// A reply that holds no code, or code that does not define the entry
    /// point, is asked for again: it says nothing about the task text, and
    /// counting it as a disagreement on every test would send the author
    /// rewriting a correct draft.
    async fn solve_with(&self, model: &M, spec: &str, entry: &str) -> Result<Option<String>, SphinxError> {
        let mut messages = vec![Message::system(SOLVER), Message::user(spec.to_owned())];
        for _ in 0..SOLVE_ATTEMPTS {
            let reply = model.complete(&messages).await.map_err(|e| model_failed(&e))?;
            let why = match json_object::<Solved>(&reply) {
                Ok(s) if defines(&s.code, entry) => return Ok(Some(s.code)),
                Ok(_) => format!("Your code does not define a function named `{entry}`"),
                Err(why) => why,
            };
            messages.push(Message::assistant(reply));
            messages.push(Message::user(format!(
                "{why}. Reply again with only the JSON object {{\"entry\": \"{entry}\", \"code\": \"the complete code\"}}, valid JSON."
            )));
        }
        Ok(None)
    }

    /// Asks until the reference runs on every input; returns the draft and its outputs.
    async fn write(
        &self,
        conversation: &mut Vec<Message>,
        progress: &mut impl FnMut(Progress),
    ) -> Result<(Written, Vec<Value>), SphinxError> {
        let mut last = String::from("the model never answered");
        let mut nudged = false;
        let mut nudged_null = false;
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
            // Models often write an object input as JSON text, or a line break as
            // a literal backslash-n, so most hidden tests only exercise the
            // invalid-input path and both solvers agree on it. Ask once; a task
            // that really takes JSON text answers the same way again.
            if failures.is_empty() && !nudged && looks_double_encoded(&written.inputs) {
                nudged = true;
                conversation.push(Message::user(
                    "Most of your inputs are strings that hold JSON (an object or array written as text) \
                     or contain a backslash followed by n instead of a real line break. Unless the task \
                     truly takes JSON text or literal backslashes, pass objects and arrays as JSON values \
                     and write line breaks as real newlines (\\n inside the JSON string). If the inputs are \
                     right as they are, reply with the same JSON object again."
                        .to_owned(),
                ));
                continue;
            }
            // A suite where the shown examples, or most tests, return null
            // tests only the invalid-input path; the usual cause is an
            // argument list that is not wrapped (`[{..}, {..}]` passed as
            // three arguments instead of `[[{..}, {..}]]` as one array).
            if failures.is_empty() && !nudged_null {
                let null = |r: &Evaluation| matches!(r, Evaluation::Returned(Value::Null));
                let nulls = results.iter().filter(|r| null(r)).count();
                let example_nulls: Vec<String> = written
                    .examples
                    .iter()
                    .filter(|&&i| results.get(i).is_some_and(null))
                    .map(|&i| call_text(&written.entry, &written.inputs[i]))
                    .collect();
                if !example_nulls.is_empty() || nulls * 2 > results.len() {
                    nudged_null = true;
                    conversation.push(Message::user(format!(
                        "Your reference returns null for {nulls} of {} inputs, including these shown examples:\n{}\n\
                         Each entry of `inputs` is the full argument list: a function that takes one array \
                         is called as [[item, item]], not [item, item]. Most tests and every shown example \
                         must exercise the normal path, with only a few invalid inputs. Fix the inputs \
                         (or the reference) and reply with the complete JSON object; if null really is the \
                         right answer for them, reply with the same JSON object again.",
                        results.len(),
                        if example_nulls.is_empty() { "(none)".to_owned() } else { example_nulls.join("\n") }
                    )));
                    continue;
                }
            }
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
    async fn second_opinion(&self, spec: &str, entry: &str) -> Result<Option<String>, SphinxError> {
        self.solve_with(self.checker.unwrap_or(self.model), spec, entry).await
    }

    async fn third_opinion(&self, spec: &str, entry: &str) -> Result<Option<String>, SphinxError> {
        self.solve_with(self.tiebreak.or(self.checker).unwrap_or(self.model), spec, entry).await
    }

    /// Drafts a job for `request`.
    ///
    /// # Errors
    /// `SPHINX_E_CONCIERGE_MODEL_UNAVAILABLE` when the model fails, and
    /// `SPHINX_E_CONCIERGE_NO_DRAFT` when no reference runs on every input.
    #[allow(clippy::too_many_lines, reason = "one pass: write, check, settle, leave out, gate")]
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
        let mut solver = self.second_opinion(&spec, &written.entry).await?;
        let mut third: Option<String> = None;
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
            if !differences.is_empty() {
                progress(Progress::ThirdOpinion);
                third = self.third_opinion(&spec, &written.entry).await?;
                break_ties(&written.entry, &mut differences, third.as_deref());
                // Where a third model, reading only the task text, lands on
                // the reference's answer, two readings of three back it: the
                // second run misread, not the text.
                differences.retain(|d| !settled_by_third(d));
            }
            if differences.is_empty() || rounds == REVIEW_ROUNDS {
                break;
            }
            rounds += 1;
            progress(Progress::Reviewing { round: rounds, differences: differences.len() });
            conversation.push(Message::user(review_prompt(&written.entry, &differences)));
            (written, outputs) = self.write(&mut conversation, progress).await?;
            spec = render_spec(&written, &outputs);
            // A fresh reading every round, so one bad run cannot keep a
            // correct test disputed.
            progress(Progress::SecondOpinion);
            solver = self.second_opinion(&spec, &written.entry).await?;
        }
        // When two other models, each reading only the task text, agree with
        // each other against the reference on every input still disputed,
        // and the author would not change it, the reference is what misread
        // the text: the second model's code becomes the reference and every
        // expected answer is recomputed from it. Agents read the same text.
        let mut adopted = false;
        if !differences.is_empty() && differences.iter().all(two_agree) {
            if let Some(code) = solver.as_deref() {
                let results = run_all(code, &written.entry, &written.inputs);
                if results.iter().all(|r| matches!(r, Evaluation::Returned(_))) {
                    progress(Progress::ReferenceReplaced { tests: differences.len() });
                    outputs = results
                        .into_iter()
                        .map(|r| match r {
                            Evaluation::Returned(v) => v,
                            _ => Value::Null,
                        })
                        .collect();
                    written.reference = code.to_owned();
                    spec = render_spec(&written, &outputs);
                    differences.clear();
                    adopted = true;
                }
            }
        }
        let mut dropped: Vec<Difference> = Vec::new();
        for mut d in disputed {
            if !written.inputs.contains(&d.args) {
                d.reference = Value::String("dropped by the redraft".into());
                dropped.push(d);
            }
        }
        // No hidden test may rest on something the task text does not settle:
        // inputs the checks still dispute after the review rounds are left
        // out, as long as enough tests remain to grade the job and most of
        // what the buyer asked for is still tested.
        if !differences.is_empty() {
            let unsettled: Vec<Vec<Value>> = differences.iter().map(|d| d.args.clone()).collect();
            let keep: Vec<bool> = written.inputs.iter().map(|a| !unsettled.contains(a)).collect();
            let left = keep.iter().filter(|k| **k).count();
            if left < MIN_TESTS || unsettled.len() * MAX_DROPPED_PARTS > written.inputs.len() {
                return Err(no_draft(
                    rounds,
                    &format!(
                        "the task text left {} of {} tests unsettled; say more precisely what you want",
                        unsettled.len(),
                        written.inputs.len()
                    ),
                ));
            }
            progress(Progress::Dropped { tests: unsettled.len() });
            retain_tests(&mut written, &mut outputs, &keep);
            spec = render_spec(&written, &outputs);
            dropped.append(&mut differences);
        }
        let suite = suite_json(&written.entry, &written.inputs, &outputs);
        let score = sphinx_grader::local_score(&suite, &written.reference, 0)?;
        if score.passed != score.total {
            return Err(SphinxError::new(
                Code::InternalInvariant,
                "the reference does not pass the suite computed from it",
            ));
        }
        // The last word: code another model wrote from the task text alone
        // passes every hidden test, so an honest agent can pass this job.
        // The runs already made are tried first, then fresh ones on the
        // final text.
        progress(Progress::FinalCheck);
        let passes = |code: Option<&str>| {
            code.is_some_and(|c| {
                sphinx_grader::local_score(&suite, c, 0).is_ok_and(|s| s.passed == s.total)
            })
        };
        // A solver whose code became the reference does not count as its own check.
        let mut independent =
            (!adopted && passes(solver.as_deref())) || passes(third.as_deref());
        for _ in 0..FINAL_ATTEMPTS {
            if independent {
                break;
            }
            let fresh = self.second_opinion(&spec, &written.entry).await?;
            independent = passes(fresh.as_deref())
                || passes(self.third_opinion(&spec, &written.entry).await?.as_deref());
        }
        if !independent {
            return Err(no_draft(
                rounds,
                "no independent solution written from the task text passes every test",
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
            dropped,
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
            if two_agree(d) {
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

    #[test]
    fn double_encoded_inputs_are_noticed() {
        let json_text =
            vec![vec![json!("{\"name\":\"a\"}")], vec![json!("[1,2]")], vec![json!({"name": "a"})]];
        assert!(looks_double_encoded(&json_text));
        let escaped = vec![vec![json!("a,b\\n1,2")], vec![json!("x\\ny")]];
        assert!(looks_double_encoded(&escaped));
        let fine =
            vec![vec![json!({"name": "a"})], vec![json!("a,b\n1,2")], vec![json!("{not json")]];
        assert!(!looks_double_encoded(&fine));
        assert!(!looks_double_encoded(&[]));
    }
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
    async fn a_suite_that_mostly_returns_null_is_sent_back_once() {
        // `sum(list)` takes one array; the first draft passes the numbers as
        // separate arguments, so every call returns null.
        let reference = "function sum(xs){ if (!Array.isArray(xs)) return null; return xs.reduce((a, b) => a + b, 0) }";
        let draft = |wrapped: bool| {
            let inputs: Vec<Value> = (0..14)
                .map(|n| if wrapped { json!([[n, n + 1]]) } else { json!([n, n + 1]) })
                .collect();
            json!({ "title": "Sum", "description": "Write `function sum(xs)`.", "entry": "sum",
                    "reference": reference, "inputs": inputs, "examples": [0, 1, 2] })
            .to_string()
        };
        let model = Scripted::new([
            draft(false),
            draft(true),
            json!({ "entry": "sum", "code": reference }).to_string(),
        ]);
        let d = Concierge::new(&model).draft("sum a list", &mut |_| {}).await.unwrap();
        assert!(d.spec.contains("sum([0,1]) // 1"), "{}", d.spec);
        let fix = &model.seen.lock().unwrap()[1];
        let note = &fix.last().unwrap().content;
        assert!(note.contains("returns null for 14 of 14 inputs"), "{note}");
        assert!(note.contains("[[item, item]]"), "{note}");
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
            solved("function double(n){ return n * 2 }"),
        ]);
        let d = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap();
        assert_eq!(d.tests, 12);
        let dropped: Vec<_> = d.dropped.iter().map(|x| x.args.clone()).collect();
        assert_eq!(dropped, [vec![json!(12)], vec![json!(13)]]);
        assert!(d.dropped.iter().all(|x| x.reference == "dropped by the redraft"));
        assert_eq!(d.differences, Vec::<Difference>::new());
        assert!(!d.task_rewritten);
    }

    #[tokio::test]
    async fn a_third_reading_that_backs_the_reference_keeps_the_test() {
        let model = Scripted::new([
            written("function double(n){ return n * 2 }", "Write `double`."),
            // The second run misreads 0; the third reads it as the reference does.
            solved("function double(n){ return n === 0 ? 1 : n * 2 }"),
            solved("function double(n){ return 2 * n }"),
        ]);
        let d = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap();
        assert_eq!((d.tests, d.second_opinion.rounds), (14, 0));
        assert_eq!(d.dropped, Vec::<Difference>::new());
        assert!(d.suite.contains("[0]"), "{}", d.suite);
        assert_eq!(model.seen.lock().unwrap().len(), 3, "no review round was needed");
    }

    #[tokio::test]
    async fn unsettled_tests_are_left_out_not_posted() {
        let reference = written("function double(n){ return n * 2 }", "Write `double`.");
        // Three readings, three answers for 0, every round.
        let second = solved("function double(n){ return n === 0 ? 1 : n * 2 }");
        let third = solved("function double(n){ return n === 0 ? 2 : n * 2 }");
        let mut script = Vec::new();
        for _ in 0..=REVIEW_ROUNDS {
            script.extend([reference.clone(), second.clone(), third.clone()]);
        }
        let model = Scripted::new(script);
        let d = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap();
        let last = model.seen.lock().unwrap()[9].last().unwrap().content.clone();
        assert!(
            last.contains("your reference: 0, a second solver: 1, a third solver: 2"),
            "{last}"
        );
        assert_eq!(d.second_opinion.rounds, REVIEW_ROUNDS);
        // The one input the checks never settled is not a hidden test.
        assert_eq!(d.differences, Vec::<Difference>::new());
        assert_eq!(d.dropped.len(), 1);
        assert_eq!(d.dropped[0].args, [json!(0)]);
        assert_eq!(d.tests, 13);
        assert!(!d.suite.contains("[0]"), "{}", d.suite);
        assert_eq!(
            sphinx_grader::local_score(
                &d.suite,
                "function double(n){ return n === 0 ? 1 : n * 2 }",
                0
            )
            .unwrap()
            .passed,
            13
        );
    }

    #[tokio::test]
    async fn two_models_agreeing_against_a_stubborn_reference_set_the_answers() {
        // The author keeps a reference that is wrong on 3; both other
        // readings get 3 right, every round.
        let reference = written("function double(n){ return n === 3 ? 0 : n * 2 }", "Write `double`.");
        let second = solved("function double(n){ return n * 2 }");
        let third = solved("function double(n){ return 2 * n }");
        let mut script = Vec::new();
        for _ in 0..=REVIEW_ROUNDS {
            script.extend([reference.clone(), second.clone(), third.clone()]);
        }
        let model = Scripted::new(script);
        let mut seen = Vec::new();
        let d = Concierge::new(&model).draft("double", &mut |p| seen.push(p)).await.unwrap();
        assert!(seen.contains(&Progress::ReferenceReplaced { tests: 1 }), "{seen:?}");
        assert_eq!((d.tests, d.dropped.len()), (14, 0));
        assert_eq!(d.reference, "function double(n){ return n * 2 }");
        assert_eq!(
            sphinx_grader::local_score(&d.suite, "function double(n){ return n + n }", 0).unwrap().passed,
            14
        );
    }

    #[tokio::test]
    async fn no_draft_when_too_many_tests_would_be_left_out() {
        let reference = written("function double(n){ return n * 2 }", "Write `double`.");
        // 0 to 3 (4 of 14, over a quarter) never settle.
        let second = solved("function double(n){ return n < 4 ? -1 : n * 2 }");
        let third = solved("function double(n){ return n < 4 ? -2 : n * 2 }");
        let mut script = Vec::new();
        for _ in 0..=REVIEW_ROUNDS {
            script.extend([reference.clone(), second.clone(), third.clone()]);
        }
        let model = Scripted::new(script);
        let e = Concierge::new(&model).draft("double", &mut |_| {}).await.unwrap_err();
        assert_eq!(e.code(), Code::ConciergeNoDraft);
    }

    #[tokio::test]
    async fn a_reply_without_code_is_asked_again_not_counted_as_disagreement() {
        let author = Scripted::new([written("function double(n){ return n * 2 }", "Write `double`.")]);
        let checker = Scripted::new([
            "I think the answer is to multiply by two.".to_owned(),
            solved("function double(n){ return n + n }"),
        ]);
        let tiebreak = Scripted::new(Vec::<String>::new());
        let mut seen = Vec::new();
        let d = Concierge::new(&author)
            .with_checkers(&checker, &tiebreak)
            .draft("double", &mut |p| seen.push(p))
            .await
            .unwrap();
        assert_eq!((d.second_opinion.agreed, d.second_opinion.rounds), (14, 0));
        assert!(seen.contains(&Progress::Compared { agreed: 14, total: 14 }), "{seen:?}");
        assert_eq!(checker.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_final_check_asks_again_when_no_earlier_run_passes_everything() {
        let author = Scripted::new([written("function double(n){ return n * 2 }", "Write `double`.")]);
        // The second run misreads 0; the third backs the reference on 0 but
        // misreads 5, which only it gets wrong. Neither passes every test,
        // so a fresh run is asked for, and it does.
        let checker = Scripted::new([
            solved("function double(n){ return n === 0 ? 1 : n * 2 }"),
            solved("function double(n){ return n * 2 }"),
        ]);
        let tiebreak = Scripted::new([solved("function double(n){ return n === 5 ? 0 : n * 2 }")]);
        let mut seen = Vec::new();
        let d = Concierge::new(&author)
            .with_checkers(&checker, &tiebreak)
            .draft("double", &mut |p| seen.push(p))
            .await
            .unwrap();
        assert_eq!(d.tests, 14);
        assert!(seen.contains(&Progress::FinalCheck));
        assert_eq!(checker.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn another_model_checks_the_draft_and_must_pass_it() {
        // The author drafts; a separate checker model solves from the text.
        let author = Scripted::new([written(
            "function double(n){ return n * 2 }",
            "Write `function double(n)`.",
        )]);
        let checker = Scripted::new([solved("function double(n){ return n + n }")]);
        let tiebreak = Scripted::new(Vec::<String>::new());
        let d = Concierge::new(&author)
            .with_checkers(&checker, &tiebreak)
            .draft("double", &mut |_| {})
            .await
            .unwrap();
        assert_eq!((d.tests, d.second_opinion.agreed), (14, 14));
        assert_eq!(checker.seen.lock().unwrap().len(), 1, "the checker, not the author, solved it");
        assert_eq!(author.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn no_draft_when_no_independent_solution_passes() {
        // A checker that never writes working code: nothing settles, so too
        // few tests would be left and nothing is posted.
        let author =
            Scripted::new(vec![
                written("function double(n){ return n * 2 }", "Write `double`.");
                8
            ]);
        let checker = Scripted::new(vec![solved("function double(n){ throw 1 }"); 8]);
        let tiebreak = Scripted::new(vec![solved("function double(n){ throw 1 }"); 8]);
        let e = Concierge::new(&author)
            .with_checkers(&checker, &tiebreak)
            .draft("double", &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(e.code(), Code::ConciergeNoDraft);
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
