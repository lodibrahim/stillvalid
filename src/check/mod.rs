//! Checks: heuristic verdicts over fetched data. Pure functions, no I/O, except `merge` and
//! `code`, which run git in the local repo, and `info`, which reads candidates' comments.

use crate::store::{Confidence, Evidence};

pub mod ai;
pub mod code;
pub mod info;
pub mod issues;
pub mod merge;
pub mod pulls;

/// A verdict's confidence and the evidence behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}
