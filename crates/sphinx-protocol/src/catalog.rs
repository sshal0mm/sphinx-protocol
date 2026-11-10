//! The error catalog: every error any Sphinx component can return.
//!
//! Codes are public API. They are append-only and never renamed; hints may be
//! reworded. `docs/ERRORS.md` is generated from this table
//! (`cargo run -p sphinx-protocol --bin gen-error-docs`) and a test fails if
//! the committed file drifts.

use crate::error::{BondEffect, Retry};

/// Which component emits an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Contract,
    Grader,
    Cli,
    Relayer,
    Api,
    Concierge,
}

impl Component {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contract => "contract",
            Self::Grader => "grader",
            Self::Cli => "cli",
            Self::Relayer => "relayer",
            Self::Api => "api",
            Self::Concierge => "concierge",
        }
    }
}

macro_rules! catalog {
    ($( $variant:ident => $code:literal, $component:ident, $retry:ident, $bond:ident, $hint:literal; )+) => {
        /// A stable error code. See the module docs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Code { $( $variant, )+ }

        impl Code {
            /// Every code, in catalog order.
            pub const ALL: &'static [Code] = &[ $( Code::$variant, )+ ];

            /// The stable string form, e.g. `SPHINX_E_SUBMIT_AGENT_CAP`.
            pub const fn as_str(self) -> &'static str {
                match self { $( Code::$variant => $code, )+ }
            }

            pub const fn component(self) -> Component {
                match self { $( Code::$variant => Component::$component, )+ }
            }

            /// Default retry class. Some call sites refine it from state.
            pub const fn retry(self) -> Retry {
                match self { $( Code::$variant => Retry::$retry, )+ }
            }

            /// What happened to the caller's bond when this error occurred.
            pub const fn bond_effect(self) -> BondEffect {
                match self { $( Code::$variant => BondEffect::$bond, )+ }
            }

            /// The next action for the caller, phrased for an agent.
            pub const fn hint(self) -> &'static str {
                match self { $( Code::$variant => $hint, )+ }
            }
        }
    };
}

catalog! {
    // ---- contract: initialisation and administration ----
    InitFeeTooHigh => "SPHINX_E_INIT_FEE_TOO_HIGH", Contract, FixInput, None,
        "fee is in basis points; the maximum is ctx.max_bps";
    AuthNotOwner => "SPHINX_E_AUTH_NOT_OWNER", Contract, Never, None,
        "only the contract owner may call this method";
    AuthNotBuyer => "SPHINX_E_AUTH_NOT_BUYER", Contract, Never, None,
        "only the job's buyer may call this method";
    AuthNotSelf => "SPHINX_E_AUTH_NOT_SELF", Contract, Never, None,
        "this is an internal callback and cannot be called directly";
    AdminBadPubkey => "SPHINX_E_ADMIN_BAD_PUBKEY", Contract, FixInput, None,
        "pass the raw 32-byte ed25519 public key as 64 lowercase hex characters, without an ed25519: prefix";
    AdminBadBoxKey => "SPHINX_E_ADMIN_BAD_BOX_KEY", Contract, FixInput, None,
        "pass the grader's X25519 box public key (box_public_key from the grader's identity run) as 64 lowercase hex characters";
    AdminBadWasmHash => "SPHINX_E_ADMIN_BAD_WASM_HASH", Contract, FixInput, None,
        "pass the SHA-256 of the grader wasm as 64 lowercase hex characters";
    AdminAttestationRefTooLong => "SPHINX_E_ADMIN_ATTESTATION_REF_TOO_LONG", Contract, FixInput, None,
        "shorten attestation_ref to at most ctx.max bytes";
    AdminDevKeyRefused => "SPHINX_E_ADMIN_DEV_KEY_REFUSED", Contract, Never, None,
        "this build accepts only outlayer_tdx grader keys; dev keys work only in a build with the dev-grader feature";

    // ---- contract: jobs ----
    JobNotFound => "SPHINX_E_JOB_NOT_FOUND", Contract, FixInput, None,
        "list the jobs to find valid ids (contract view list_jobs; MCP sphinx_jobs)";
    JobWrongState => "SPHINX_E_JOB_WRONG_STATE", Contract, Never, None,
        "read the job for its current status (contract view get_job; MCP sphinx_jobs with job_id); ctx.expected lists the statuses this call accepts";
    JobWrongDeposit => "SPHINX_E_JOB_WRONG_DEPOSIT", Contract, FixInput, None,
        "attach exactly ctx.expected yoctoNEAR (the reward plus the job storage deposit from get_config)";
    JobRewardTooSmall => "SPHINX_E_JOB_REWARD_TOO_SMALL", Contract, FixInput, None,
        "raise the reward to at least ctx.min_reward yoctoNEAR";
    JobBondTooSmall => "SPHINX_E_JOB_BOND_TOO_SMALL", Contract, FixInput, None,
        "raise the bond to at least ctx.min_bond yoctoNEAR";
    JobBondTooLarge => "SPHINX_E_JOB_BOND_TOO_LARGE", Contract, FixInput, None,
        "lower the bond or max_subs_total so that bond x max_subs_total does not exceed the reward";
    JobBadCaps => "SPHINX_E_JOB_BAD_CAPS", Contract, FixInput, None,
        "choose caps with 1 <= max_subs_per_agent <= max_subs_total <= ctx.max";
    JobDeadlineTooSoon => "SPHINX_E_JOB_DEADLINE_TOO_SOON", Contract, FixInput, None,
        "the deadline is block time in nanoseconds; set it at least ctx.min_duration_ns after now";
    JobDeadlineTooFar => "SPHINX_E_JOB_DEADLINE_TOO_FAR", Contract, FixInput, None,
        "set the deadline no more than ctx.max_duration_ns after now";
    JobUriTooLong => "SPHINX_E_JOB_URI_TOO_LONG", Contract, FixInput, None,
        "shorten the URI, for example to a content hash or short link";
    JobBadRuntime => "SPHINX_E_JOB_BAD_RUNTIME", Contract, FixInput, None,
        "use one of the runtimes listed in ctx.known";
    JobHasSubmissions => "SPHINX_E_JOB_HAS_SUBMISSIONS", Contract, Never, None,
        "a job with submissions cannot be cancelled; wait for the deadline and call refund_on_timeout";
    JobTooManyInvites => "SPHINX_E_JOB_TOO_MANY_INVITES", Contract, FixInput, None,
        "invite at most ctx.max agents, or invite none to open the job to every agent";

    // ---- contract: agent directory ----
    AgentProfileInvalid => "SPHINX_E_AGENT_PROFILE_INVALID", Contract, FixInput, None,
        "ctx.field is outside its limit ctx.max; shorten it and register again";
    AgentWrongDeposit => "SPHINX_E_AGENT_WRONG_DEPOSIT", Contract, FixInput, None,
        "attach exactly ctx.expected yoctoNEAR when first registering; updating a profile needs no deposit";
    AgentNotRegistered => "SPHINX_E_AGENT_NOT_REGISTERED", Contract, FixInput, None,
        "this account has no profile; register_agent creates one";

    // ---- contract: suite commit and validation ----
    CommitBadHash => "SPHINX_E_COMMIT_BAD_HASH", Contract, FixInput, None,
        "ctx.field must be the SHA-256 of the raw file bytes as 64 lowercase hex characters (`sphinx suite hash`)";
    CommitNoGraderKey => "SPHINX_E_COMMIT_NO_GRADER_KEY", Contract, Wait, None,
        "no grader key is registered yet; the operator must register one after verifying its attestation";
    CommitGraderKeyChanged => "SPHINX_E_COMMIT_GRADER_KEY_CHANGED", Contract, FixInput, None,
        "the registered grader key is ctx.current, not the ctx.given you checked; verify the new key's attestation, then commit with it";
    SuiteCheckDeadlinePassed => "SPHINX_E_SUITE_CHECK_DEADLINE_PASSED", Contract, Never, None,
        "the job expired before its suite was validated; call refund_on_timeout to return the reward";

    // ---- contract: submissions ----
    SubmitBadHash => "SPHINX_E_SUBMIT_BAD_HASH", Contract, FixInput, None,
        "code_hash must be the SHA-256 of the exact bytes at code_uri as 64 lowercase hex characters";
    SubmitUriTooLong => "SPHINX_E_SUBMIT_URI_TOO_LONG", Contract, FixInput, None,
        "shorten code_uri";
    SubmitDeadlinePassed => "SPHINX_E_SUBMIT_DEADLINE_PASSED", Contract, Never, None,
        "this job no longer accepts submissions; pick another job";
    SubmitBuyerSelf => "SPHINX_E_SUBMIT_BUYER_SELF", Contract, Never, None,
        "the buyer cannot submit to its own job";
    SubmitWrongDeposit => "SPHINX_E_SUBMIT_WRONG_DEPOSIT", Contract, FixInput, None,
        "attach exactly ctx.expected yoctoNEAR (the job bond plus the submission storage deposit); this attempt was refunded";
    SubmitJobCap => "SPHINX_E_SUBMIT_JOB_CAP", Contract, Never, None,
        "this job has no submission slots left; pick another job (no bond was taken)";
    SubmitAgentCap => "SPHINX_E_SUBMIT_AGENT_CAP", Contract, Never, None,
        "you have used all your attempts on this job; pick another job (no bond was taken)";
    SubmitPendingExists => "SPHINX_E_SUBMIT_PENDING_EXISTS", Contract, Wait, None,
        "wait for the result on your pending submission, or call reclaim_bond after ctx.reclaim_after_ns";
    SubmitNotInvited => "SPHINX_E_SUBMIT_NOT_INVITED", Contract, Never, None,
        "this job is open only to the agents its buyer invited (get_invites); pick an open job (no bond was taken)";
    SubmitDuplicateCode => "SPHINX_E_SUBMIT_DUPLICATE_CODE", Contract, FixInput, None,
        "identical code is graded once per job; submit different code";
    SubmitJobClosing => "SPHINX_E_SUBMIT_JOB_CLOSING", Contract, Never, None,
        "a submission on this job already passed and is waiting for earlier ones to finish; pick another job (no bond was taken)";

    // ---- contract: grader results ----
    ScoreBadCounts => "SPHINX_E_SCORE_BAD_COUNTS", Contract, Never, None,
        "the result's pass counts are malformed; do not retry this result";
    ScoreNoSubmission => "SPHINX_E_SCORE_NO_SUBMISSION", Contract, FixInput, None,
        "check sub_id against the receipt returned by submit";
    ScoreAlreadyGraded => "SPHINX_E_SCORE_ALREADY_GRADED", Contract, Never, None,
        "this submission already has a result; the duplicate can be dropped";
    ScoreFieldMismatch => "SPHINX_E_SCORE_FIELD_MISMATCH", Contract, Never, None,
        "post the grader's result object verbatim; ctx.field shows which value disagrees with on-chain state";
    SigMalformed => "SPHINX_E_SIG_MALFORMED", Contract, FixInput, None,
        "pass the grader's signature as 128 lowercase hex characters";
    SigInvalid => "SPHINX_E_SIG_INVALID", Contract, Never, None,
        "the signature does not verify against the grader key snapshotted for this job over ctx.signed_message";

    // ---- contract: refunds, reclaims, withdrawals ----
    RefundTooEarly => "SPHINX_E_REFUND_TOO_EARLY", Contract, Wait, None,
        "retry after ctx.refund_after_ns: the deadline plus one grading window, or two while submissions are still pending";
    RefundHasPass => "SPHINX_E_REFUND_HAS_PASS", Contract, Never, None,
        "a submission on this job passed; call settle to pay it instead of refunding";
    SettleNothing => "SPHINX_E_SETTLE_NOTHING", Contract, Never, None,
        "this job has no passing submission waiting to be paid";
    SettleBlocked => "SPHINX_E_SETTLE_BLOCKED", Contract, Wait, None,
        "an earlier submission (ctx.blocking_sub_id) is still inside its grading window; retry after ctx.retry_after_ns";
    ReclaimNotPending => "SPHINX_E_RECLAIM_NOT_PENDING", Contract, Never, None,
        "only bonds of submissions still waiting for a result can be reclaimed";
    ReclaimTooEarly => "SPHINX_E_RECLAIM_TOO_EARLY", Contract, Wait, Held,
        "the submission is still inside its grading window; retry after ctx.reclaim_after_ns or wait for the result";
    WithdrawNothing => "SPHINX_E_WITHDRAW_NOTHING", Contract, Never, None,
        "this account has no unclaimed balance";
    InternalInvariant => "SPHINX_E_INTERNAL_INVARIANT", Contract, Never, None,
        "this is a contract bug; report it with the transaction hash";

    // ---- grader: request intake ----
    GraderBadRequest => "SPHINX_E_GRADER_BAD_REQUEST", Grader, FixInput, None,
        "the request does not match the sphinx.grader/1 schema in docs/PROTOCOL.md";
    GraderSuiteMalformed => "SPHINX_E_GRADER_SUITE_MALFORMED", Grader, FixInput, None,
        "the suite is not valid sphinx.suite/1 JSON; run `sphinx suite check` locally";
    GraderSuiteEmpty => "SPHINX_E_GRADER_SUITE_EMPTY", Grader, FixInput, None,
        "add at least one test";
    GraderSuiteTooManyTests => "SPHINX_E_GRADER_SUITE_TOO_MANY_TESTS", Grader, FixInput, None,
        "trim the suite to at most ctx.max tests";
    GraderSuiteTooLarge => "SPHINX_E_GRADER_SUITE_TOO_LARGE", Grader, FixInput, None,
        "reduce the size of test arguments and expected values";
    GraderSuiteValueTooDeep => "SPHINX_E_GRADER_SUITE_VALUE_TOO_DEEP", Grader, FixInput, None,
        "flatten nested values to at most ctx.max levels";
    GraderSuiteUnrepresentable => "SPHINX_E_GRADER_SUITE_UNREPRESENTABLE", Grader, FixInput, None,
        "integers must be within +/-2^53, numbers finite, and objects must not use the __proto__ key";
    GraderSuiteHashMismatch => "SPHINX_E_GRADER_SUITE_HASH_MISMATCH", Grader, Never, None,
        "the hidden suite differs from the one committed on-chain; do not retry";
    GraderRuntimeUnsupported => "SPHINX_E_GRADER_RUNTIME_UNSUPPORTED", Grader, FixInput, None,
        "use a runtime listed in ctx.supported";
    GraderEntryInvalid => "SPHINX_E_GRADER_ENTRY_INVALID", Grader, FixInput, None,
        "name the entry function with a plain JavaScript identifier such as solve";
    GraderEntryReserved => "SPHINX_E_GRADER_ENTRY_RESERVED", Grader, FixInput, None,
        "pick an entry name that is not a JavaScript keyword or built-in global";
    GraderCodeTooLarge => "SPHINX_E_GRADER_CODE_TOO_LARGE", Grader, FixInput, None,
        "reduce the code to at most ctx.max bytes";
    GraderCodeHashMismatch => "SPHINX_E_GRADER_CODE_HASH_MISMATCH", Grader, Never, None,
        "the code fetched for grading differs from the code_hash committed on-chain; do not retry";
    GraderHiddenInputMissing => "SPHINX_E_GRADER_HIDDEN_INPUT_MISSING", Grader, Never, Held,
        "the hidden suite or reference was not available to the enclave run: the buyer's stored suite is missing, or the run used a different grader build or caller; no result was produced";
    GraderBadContractId => "SPHINX_E_GRADER_BAD_CONTRACT_ID", Grader, FixInput, None,
        "contract must be a valid NEAR account id";
    GraderSignerUnavailable => "SPHINX_E_GRADER_SIGNER_UNAVAILABLE", Grader, Transient, Held,
        "the enclave signing key is unavailable; check the module manifest and how the module is run";
    GraderInlineInputRefused => "SPHINX_E_GRADER_INLINE_INPUT_REFUSED", Grader, FixInput, None,
        "the enclave grader takes no plaintext inputs: the suite and reference come only from the buyer's stored secret, and the submission only as submission_box encrypted to the job's grader box_public_key; remove ctx.field";
    GraderNotOnChain => "SPHINX_E_GRADER_NOT_ON_CHAIN", Grader, Never, Held,
        "the request does not match the escrow's state (ctx.field): only a pending on-chain submission, or a committed suite, is graded; read the job and the submission (contract views get_job and get_submission; MCP sphinx_jobs with job_id and sub_id) and do not retry";
    GraderSuiteNotForJob => "SPHINX_E_GRADER_SUITE_NOT_FOR_JOB", Grader, Never, Held,
        "the stored hidden suite was stored for another contract or job (ctx.field); the buyer must store it for this job with `sphinx suite store`";
    GraderBoxUnreadable => "SPHINX_E_GRADER_BOX_UNREADABLE", Grader, Never, Held,
        "submission_box does not open with this grader's box key; encrypt the code to the job's grader box_public_key (job.commit.grader.box_public_key)";

    // ---- grader: submission preflight (local, no bond involved) ----
    CodeSyntax => "SPHINX_E_CODE_SYNTAX", Grader, FixInput, None,
        "fix the syntax error; on-chain, a submission that does not parse is graded 0/N, a failed attempt";
    CodeEntryMissing => "SPHINX_E_CODE_ENTRY_MISSING", Grader, FixInput, None,
        "define the entry function at top level, e.g. function solve(...) { ... }";
    CodeEntryNotFunction => "SPHINX_E_CODE_ENTRY_NOT_FUNCTION", Grader, FixInput, None,
        "the entry name is bound to a value that is not a function; make it a function";

    // ---- relayer ----
    RelayNoScore => "SPHINX_E_RELAY_NO_SCORE", Relayer, Never, Held,
        "the enclave produced no result (it ran out of resources or failed); it is not run again, and the bond can be reclaimed after ctx.reclaim_after_ns";
    RelayRpcFailed => "SPHINX_E_RELAY_RPC_FAILED", Relayer, Transient, Held,
        "the NEAR RPC call failed; retry with backoff";
    RelayNoBoxKey => "SPHINX_E_RELAY_NO_BOX_KEY", Relayer, Never, Held,
        "the job's grader key has no usable box_public_key, so its submissions cannot reach the enclave without being published; the bond can be reclaimed after ctx.reclaim_after_ns";

    // ---- job api ----
    ApiCodeNotFound => "SPHINX_E_API_CODE_NOT_FOUND", Api, FixInput, None,
        "no stored code has this hash; upload it with POST /v1/code first";
    ApiBadRequest => "SPHINX_E_API_BAD_REQUEST", Api, FixInput, None,
        "the request does not match the API schema; see docs/API.md";
    ApiRelayNotAllowed => "SPHINX_E_API_RELAY_NOT_ALLOWED", Api, FixInput, None,
        "only signed calls to the escrow's agent methods are relayed; sign a delegate action for one of them, or send the transaction yourself";
    ApiRelayDisabled => "SPHINX_E_API_RELAY_DISABLED", Api, Wait, None,
        "this API instance has no relayer configured; send the transaction yourself or use an instance with relaying enabled";
    ApiRateLimited => "SPHINX_E_API_RATE_LIMITED", Api, Wait, None,
        "this account sent too many relayed calls, or too many that failed on-chain; wait ctx.retry_after_secs seconds, or send the transaction yourself";

    // ---- cli ----
    CliUsage => "SPHINX_E_CLI_USAGE", Cli, FixInput, None,
        "run the command with --help";
    CliConfigMissing => "SPHINX_E_CLI_CONFIG_MISSING", Cli, FixInput, None,
        "set the variable named in ctx.var, e.g. in .env (see .env.example)";
    CliConfigMalformed => "SPHINX_E_CLI_CONFIG_MALFORMED", Cli, FixInput, None,
        "regenerate the value as described in .env.example";
    CliFileUnreadable => "SPHINX_E_CLI_FILE_UNREADABLE", Cli, FixInput, None,
        "check the path in ctx.path and its permissions";
    CliUnknownCode => "SPHINX_E_CLI_UNKNOWN_CODE", Cli, FixInput, None,
        "run `sphinx errors` to list every code";
    CliReferenceFails => "SPHINX_E_CLI_REFERENCE_FAILS", Cli, FixInput, None,
        "the reference solution must pass every test before a job is posted (ctx.passed of ctx.total pass); fix the suite or the reference. Nothing was posted";
    CliGraderUnverified => "SPHINX_E_CLI_GRADER_UNVERIFIED", Cli, Wait, None,
        "the registered grader's attestation could not be read or does not match its build, so there is no build the hidden suite can safely be stored for; retry later, and report it if it persists";
    CliSuiteDeleteTooEarly => "SPHINX_E_CLI_SUITE_DELETE_TOO_EARLY", Cli, Wait, None,
        "the job is still running and needs its hidden suite to grade; delete it once the job has ended (Paid, Rejected, Refunded or Cancelled)";
    CliSpendCap => "SPHINX_E_CLI_SPEND_CAP", Cli, FixInput, None,
        "this MCP server may attach at most ctx.cap yoctoNEAR in total and has attached ctx.spent; a person must raise SPHINX_MAX_SPEND_NEAR or restart it";
    CliAmountInvalid => "SPHINX_E_CLI_AMOUNT_INVALID", Cli, FixInput, None,
        "give NEAR amounts as a decimal string with at most 24 decimal places, e.g. \"0.3\"";

    // ---- concierge ----
    ConciergeModelUnavailable => "SPHINX_E_CONCIERGE_MODEL_UNAVAILABLE", Concierge, Transient, None,
        "the model on NEAR AI Cloud did not answer usably; retry, and check NEAR_AI_API_KEY and its credit if it keeps failing";
    ConciergeNoDraft => "SPHINX_E_CONCIERGE_NO_DRAFT", Concierge, FixInput, None,
        "no draft passed the checks within ctx.rounds rounds; describe the inputs, the output and the edge cases more precisely and ask again";
    ConciergeUnsettled => "SPHINX_E_CONCIERGE_UNSETTLED", Concierge, FixInput, None,
        "a second solver disagreed with the reference on ctx.differences of ctx.tests hidden tests, so they may not match the description; read the differences in the draft folder, then draft again, or post with accept_differences true if the reference is right";
    ConciergeDraftNotFound => "SPHINX_E_CONCIERGE_DRAFT_NOT_FOUND", Concierge, FixInput, None,
        "drafts are kept only while this MCP server runs; draft it again with sphinx_concierge, action draft";
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Code {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s).ok_or(())
    }
}

/// Renders `docs/ERRORS.md` from the catalog.
pub fn render_error_docs() -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "# Error codes\n\n\
         Generated from `crates/sphinx-protocol/src/catalog.rs` by \
         `cargo run -p sphinx-protocol --bin gen-error-docs`. Do not edit by hand.\n\n\
         Every Sphinx error carries a stable `code`, a one-line `message` with the actual \
         values, a `hint` (the next action), a `retry` class, a `bond_effect` and a `ctx` \
         object with machine-readable values (large integers as strings, times in `_ns`).\n\n\
         **Contract** errors appear in the NEAR transaction failure as \
         `Smart contract panicked: CODE: message {json}`, where the JSON tail is \
         `{\"v\":1,\"retry\":..,\"bond_effect\":..,\"hint\":..,\"ctx\":{..}}`. Match on the code; \
         `sphinx explain '<error text>'` decodes it.\n\n\
         **Grader** errors appear in the response envelope under `error`.\n\n\
         | retry | meaning |\n|---|---|\n\
         | `never` | the same call will fail again |\n\
         | `fix_input` | change the input as the hint says, then retry |\n\
         | `wait` | retry later; `ctx` says when |\n\
         | `transient` | retry as-is with backoff |\n\n\
         | bond_effect | meaning |\n|---|---|\n\
         | `none` | no bond was taken (a failed call refunds its deposit) |\n\
         | `forfeited` | the attempt failed: the job holds the bond, burned if another submission passes, back to you if the job is refunded or cancelled |\n\
         | `returned` | the bond was paid back |\n\
         | `held` | the contract still holds the bond; it can be recovered |\n",
    );
    for component in [
        Component::Contract,
        Component::Grader,
        Component::Relayer,
        Component::Api,
        Component::Cli,
        Component::Concierge,
    ] {
        let _ = write!(out, "\n## {}\n", component.as_str());
        for code in Code::ALL.iter().filter(|c| c.component() == component) {
            let _ = write!(
                out,
                "\n### {}\n\n- retry: `{}`\n- bond_effect: `{}`\n- hint: {}\n",
                code.as_str(),
                code.retry().as_str(),
                code.bond_effect().as_str(),
                code.hint()
            );
        }
    }
    out
}
