//! Resource limits for one grading run.
//!
//! The enclave meters wasm instructions; the relayer asks OutLayer for 2e9
//! per run (`RunLimits` in sphinx-near). Measured on the release wasm (see
//! `docs/LIMITS.md`):
//! - building a fresh sandbox and loading a small submission costs about
//!   7.7e6 instructions per test;
//! - one executor poll (`CYCLES_PER_POLL` VM cycles) costs 1.6e5 to 2.7e5
//!   depending on the instruction mix (2.7e5 for tight arithmetic loops);
//! - a test stopped by the per-frame loop limit costs about 1.7e7.
//!
//! The run is bounded by: `MAX_TESTS` (32) sandboxes, `RUN_POLLS` polls
//! shared by all tests (each test may use up to `TEST_POLLS`), and at most
//! `MAX_ABORTS` tests ending on a resource limit, after which the remaining
//! tests fail without running. Worst case: 32 x 7.7e6 + 1536 x 2.7e5 +
//! 4 x 1.7e7 = about 7.3e8 for plain loops. Allocation-heavy code costs more
//! instructions per cycle (a 300-item comparator sort in every one of 32 tests
//! needs more than 1e9), which the 2e9 allowance covers. Every count is in VM
//! cycles or events, so results are deterministic.

/// Boa "clock cycles" executed between two executor polls.
pub const CYCLES_PER_POLL: u32 = 4_096;

/// Polls one load of the submission's top-level code may use.
pub const LOAD_POLLS: u32 = 16;

/// Polls one test call may use.
pub const TEST_POLLS: u32 = 256;

/// Polls the whole run may use, across every load and test call.
pub const RUN_POLLS: u32 = 1_536;

/// Tests that may end on a resource limit before the rest are failed unrun.
pub const MAX_ABORTS: u32 = 4;

/// Per-call-frame loop iteration cap. Bounds loops inside callbacks invoked
/// by built-ins (e.g. `Array.prototype.map`), which run outside the polled
/// budget.
pub const LOOP_ITERATIONS: u64 = 20_000;

/// Maximum JavaScript call depth.
pub const RECURSION_DEPTH: usize = 256;

/// Maximum VM stack size (values).
pub const STACK_SIZE: usize = 64 * 1024;

/// Maximum submission or reference size in bytes.
pub const MAX_CODE_BYTES: usize = 64 * 1024;

/// Maximum nesting depth when reading a return value back.
pub const MAX_RESULT_DEPTH: u32 = 32;

/// Maximum number of values read back from one return value.
pub const MAX_RESULT_NODES: u32 = 10_000;
