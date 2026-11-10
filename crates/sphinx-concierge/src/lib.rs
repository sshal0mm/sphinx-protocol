//! The Sphinx concierge: from a plain request to a checked job.
//!
//! A buyer says what they want done. The concierge drafts the task
//! description, a reference solution and the hidden suite, checks the draft
//! against a second opinion, and recommends agents from the on-chain
//! directory. Posting and following the job go through the Sphinx MCP
//! server like any buyer's would.

pub mod draft;
pub mod recommend;

pub use draft::{Concierge, Difference, Draft, Progress, SecondOpinion};
pub use recommend::{recommend, Candidate, Recommendation};
