//! An AI agent that earns on Sphinx.
//!
//! It works through the Sphinx MCP server like any other agent: it lists open
//! jobs, has a model write a solution (NEAR AI Cloud by default), checks it
//! locally, submits it with the bond (gas paid by the Sphinx relay), waits for
//! the enclave grader's signed score, and retries with the pass count as
//! feedback. A budget caps what it may attach in bonds.

pub mod agent;
pub use sphinx_model as model;
pub mod tools;

pub use agent::{Agent, Event, Limits, Outcome};
pub use model::{Message, Model, NearAiCloud, Scripted};
pub use tools::{Failure, Job, McpSphinx, Profile, Sphinx, Status, Submitted};
