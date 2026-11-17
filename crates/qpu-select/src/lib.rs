// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Vendor-neutral QPU selection policies.
//!
//! Given the observable metrics of a set of quantum backends and a user's
//! request, decide which backend to run on. That is the whole of this
//! crate's job.
//!
//! # Scope
//!
//! This crate deliberately has **no dependencies** and performs **no I/O**.
//! It does not know about QRMI, LSF, HTTP, environment variables, or an
//! async runtime. Collecting metrics is the caller's problem; choosing
//! between them is this crate's. That separation is what makes the policies
//! reusable by a Slurm plugin, a Kubernetes scheduler, a broker service or
//! a notebook, and what keeps the tests hermetic and fast.
//!
//! It is `no_std` (with `alloc`), so it can also be linked into constrained
//! environments.
//!
//! # Policies
//!
//! | Name | Type | Chooses |
//! |------|------|---------|
//! | `basic` | [`Basic`] | the least busy backend with enough qubits |
//! | `health` | [`Health`] | the best weighted composite of T1, readout error and queue depth |
//! | `priority` | [`Priority`] | by applying the request's requirements in the order written |
//!
//! # Example
//!
//! ```
//! use qpu_select::{Candidates, ProcessorType, QpuMetrics, Request, Selector, selector_by_name};
//!
//! let mut candidates = Candidates::new();
//! candidates.insert(
//!     "ibm_fez".to_owned(),
//!     QpuMetrics::builder()
//!         .qubits(156)
//!         .clops(320_000.0)
//!         .pending_jobs(435)
//!         .processor_type(ProcessorType::new("Heron").with_revision("2"))
//!         .available(true)
//!         .build(),
//! );
//! candidates.insert(
//!     "ibm_marrakesh".to_owned(),
//!     QpuMetrics::builder()
//!         .qubits(156)
//!         .clops(195_000.0)
//!         .pending_jobs(3)
//!         .processor_type(ProcessorType::new("Heron").with_revision("2"))
//!         .available(true)
//!         .build(),
//! );
//!
//! // "At least 128 qubits, then prefer the shortest queue."
//! let request = Request::parse("qubits=128;pending_jobs=1000")?;
//! let selector = selector_by_name("priority").expect("known policy");
//! let selection = selector.select(&candidates, &request);
//!
//! assert_eq!(selection.name(), Some("ibm_marrakesh"));
//! # Ok::<(), qpu_select::ParseError>(())
//! ```
//!
//! # Relationship to the Python implementation
//!
//! These policies are ports of `select_device_default`,
//! `select_device_health` and `select_device_priority` from
//! `qrmi-esub-jobstarter.py`. Behaviour is preserved, with two deliberate
//! differences, both documented at the point they occur:
//!
//! * [`Basic`] joins status and configuration data by backend name rather
//!   than by list position.
//! * [`Health`] excludes a backend whose calibration data is missing instead
//!   of scoring it zero.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

mod basic;
pub mod health;
mod metrics;
mod priority;
mod request;
mod selector;

pub use basic::Basic;
pub use health::Health;
pub use metrics::{Direction, Metric, MetricValue, ProcessorType, QpuMetrics, QpuMetricsBuilder};
pub use priority::Priority;
pub use request::{Comparison, ParseError, Request, RequestedValue, Requirement};
pub use selector::{Candidates, Rejected, Rejection, Selection, Selector};

use alloc::boxed::Box;

/// The names of every available policy, in the order a help message should
/// list them.
///
/// Matches the `allowed_selectors` set validated by the Python `esub`.
pub const SELECTOR_NAMES: [&str; 3] = ["basic", "health", "priority"];

/// Look up a policy by the name given to `selector=`.
///
/// Returns `None` for an unknown name, so the caller can report it with the
/// same wording as before.
///
/// # Example
///
/// ```
/// use qpu_select::{selector_by_name, SELECTOR_NAMES};
///
/// for name in SELECTOR_NAMES {
///     assert_eq!(selector_by_name(name).map(|s| s.name()), Some(name));
/// }
/// assert!(selector_by_name("nonesuch").is_none());
/// ```
#[must_use]
pub fn selector_by_name(name: &str) -> Option<Box<dyn Selector>> {
    match name {
        "basic" => Some(Box::new(Basic)),
        "health" => Some(Box::new(Health)),
        "priority" => Some(Box::new(Priority)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_selector_resolves() {
        for name in SELECTOR_NAMES {
            let selector = selector_by_name(name).expect("listed policy must resolve");
            assert_eq!(selector.name(), name);
        }
    }

    #[test]
    fn unknown_selector_is_rejected() {
        assert!(selector_by_name("").is_none());
        assert!(
            selector_by_name("Basic").is_none(),
            "names are case-sensitive"
        );
        assert!(selector_by_name("nonesuch").is_none());
    }

    /// The default policy must stay `basic`, as `esub` relies on it.
    #[test]
    fn basic_is_the_first_listed_policy() {
        assert_eq!(SELECTOR_NAMES[0], "basic");
    }
}
