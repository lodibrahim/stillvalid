//! Checks: heuristic verdicts over fetched data. Pure functions, no I/O.

use crate::store::{Confidence, Evidence};

pub mod issues;

/// A verdict's confidence and the evidence behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub confidence: Confidence,
    pub evidence: Vec<Evidence>,
}
