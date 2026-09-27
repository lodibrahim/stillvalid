//! Checks: heuristic verdicts over fetched data. Pure functions, no I/O, except `merge` and
//! `code`, which run git in the local repo.

use crate::store::{Confidence, Evidence};

pub mod code;
pub mod issues;
pub mod merge;
pub mod pulls;

/// A verdict's confidence and the evidence behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}
