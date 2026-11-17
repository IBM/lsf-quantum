// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The selection interface shared by every policy.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::metrics::QpuMetrics;
use crate::request::Request;

/// The candidate backends, keyed by name.
///
/// A map rather than the Python pair of parallel lists. The Python `basic`
/// selector merges its status and configuration lists positionally, with the
/// assumption written in a source comment ("Assuming that devices come in the
/// same order"); if the two enumerations ever disagree, qubit counts are
/// attributed to the wrong device. Keying by name makes that mismatch
/// unrepresentable.
///
/// `BTreeMap` rather than `HashMap` so iteration order is deterministic,
/// which makes tie-breaking reproducible without relying on a hash seed.
pub type Candidates = BTreeMap<String, QpuMetrics>;

/// Why a candidate was excluded.
///
/// Carried so callers can log the same diagnostics the Python code emits
/// through `print_debug` (for example `Skipping ibm_miami: only 120 qubits
/// < 128 required`) without the library writing to stderr itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The backend is not accepting work.
    Unavailable,
    /// The backend has too few qubits.
    InsufficientQubits {
        /// Qubits the backend reports.
        available: Option<u32>,
        /// Qubits the request demands.
        required: u32,
    },
    /// The backend failed a requirement from the request.
    FailedRequirement {
        /// The requirement as written, for diagnostics.
        requirement: String,
        /// The value the backend reported, or `None` if absent.
        reported: Option<String>,
    },
    /// The backend lacks a metric the policy needs to score it.
    MissingMetric {
        /// The metric that was absent.
        metric: String,
    },
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("not available"),
            Self::InsufficientQubits {
                available,
                required,
            } => match available {
                Some(available) => {
                    write!(f, "only {available} qubits < {required} required")
                }
                None => write!(f, "qubit count unknown, {required} required"),
            },
            Self::FailedRequirement {
                requirement,
                reported,
            } => match reported {
                Some(reported) => write!(f, "{requirement} not met (reported {reported})"),
                None => write!(f, "{requirement} not met (not reported)"),
            },
            Self::MissingMetric { metric } => write!(f, "does not report {metric}"),
        }
    }
}

/// One backend that was considered and excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// The backend's name.
    pub name: String,
    /// Why it was excluded.
    pub reason: Rejection,
}

/// The outcome of a selection.
///
/// Returned whether or not a backend was chosen, so a caller can report why
/// nothing matched. The Python code returns a bare name (or `None`) and emits
/// its reasoning to stderr as a side effect; separating the two keeps this
/// crate free of I/O and lets callers choose their own diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// The chosen backend and its metrics, if any matched.
    pub chosen: Option<(String, QpuMetrics)>,
    /// Backends that were considered and excluded, in name order.
    pub rejected: Vec<Rejected>,
}

impl Selection {
    /// A selection that chose nothing.
    #[must_use]
    pub const fn none(rejected: Vec<Rejected>) -> Self {
        Self {
            chosen: None,
            rejected,
        }
    }

    /// The chosen backend's name, if any.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.chosen.as_ref().map(|(name, _)| name.as_str())
    }

    /// The chosen backend's metrics, if any.
    #[must_use]
    pub const fn metrics(&self) -> Option<&QpuMetrics> {
        match &self.chosen {
            Some((_, metrics)) => Some(metrics),
            None => None,
        }
    }
}

/// A QPU selection policy.
///
/// Implementations are pure: same candidates and request in, same selection
/// out. No network, no clock, no environment.
///
/// # Example
///
/// ```
/// use qpu_select::{Basic, Candidates, QpuMetrics, Request, Selector};
///
/// let mut candidates = Candidates::new();
/// candidates.insert(
///     "ibm_fez".to_owned(),
///     QpuMetrics::builder().qubits(156).pending_jobs(435).available(true).build(),
/// );
/// candidates.insert(
///     "ibm_marrakesh".to_owned(),
///     QpuMetrics::builder().qubits(156).pending_jobs(3).available(true).build(),
/// );
///
/// let request = Request::parse("qubits=128")?;
/// let selection = Basic.select(&candidates, &request);
///
/// // Least busy with enough qubits.
/// assert_eq!(selection.name(), Some("ibm_marrakesh"));
/// # Ok::<(), qpu_select::ParseError>(())
/// ```
pub trait Selector {
    /// The policy's name, as accepted by `selector=` on the command line.
    fn name(&self) -> &'static str;

    /// Choose a backend.
    fn select(&self, candidates: &Candidates, request: &Request) -> Selection;
}
