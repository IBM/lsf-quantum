// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The `basic` policy: the least busy backend with enough qubits.

use alloc::vec::Vec;

use crate::metrics::Metric;
use crate::request::Request;
use crate::selector::{Candidates, Rejected, Rejection, Selection, Selector};

/// Selects the least busy available backend that has enough qubits.
///
/// This is the default policy, equivalent to the Python
/// `select_device_default()`. The Python version builds two parallel lists
/// and merges them by index; this one joins on the backend name, so a
/// difference in enumeration order between status and configuration cannot
/// misattribute a qubit count.
///
/// A backend that does not report its queue depth is treated as having an
/// empty queue. This preserves the upstream behaviour, where
/// `get_devices_topology_qrmi()` substitutes `0` for a missing
/// `pending_job_count` precisely so that such a backend stays eligible and
/// is ranked on the remaining criteria.
///
/// Ties on queue depth are broken by name, so the choice is deterministic.
#[derive(Debug, Clone, Copy, Default)]
pub struct Basic;

impl Selector for Basic {
    fn name(&self) -> &'static str {
        "basic"
    }

    fn select(&self, candidates: &Candidates, request: &Request) -> Selection {
        let required = request.required_qubits();
        let mut rejected = Vec::new();
        let mut best: Option<(&str, u64)> = None;

        for (name, metrics) in candidates {
            if !metrics.available {
                rejected.push(Rejected {
                    name: name.clone(),
                    reason: Rejection::Unavailable,
                });
                continue;
            }

            if let Some(required) = required {
                if metrics.qubits.is_none_or(|qubits| qubits < required) {
                    rejected.push(Rejected {
                        name: name.clone(),
                        reason: Rejection::InsufficientQubits {
                            available: metrics.qubits,
                            required,
                        },
                    });
                    continue;
                }
            }

            // A missing queue depth counts as empty; see the type docs.
            let queue = metrics.pending_jobs.unwrap_or(0);

            // Strictly-less keeps the first backend in name order on a tie,
            // because `candidates` iterates in sorted order.
            if best.is_none_or(|(_, best_queue)| queue < best_queue) {
                best = Some((name.as_str(), queue));
            }
        }

        let chosen = best.map(|(name, _)| {
            let metrics = candidates[name].clone();
            (name.into(), metrics)
        });

        Selection { chosen, rejected }
    }
}

impl Basic {
    /// The metric this policy ranks on.
    pub const RANKS_ON: Metric = Metric::PendingJobs;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::QpuMetrics;
    use alloc::borrow::ToOwned;
    use alloc::vec;

    fn candidate(qubits: u32, queue: u64, available: bool) -> QpuMetrics {
        QpuMetrics::builder()
            .qubits(qubits)
            .pending_jobs(queue)
            .available(available)
            .build()
    }

    fn candidates(entries: &[(&str, u32, u64, bool)]) -> Candidates {
        entries
            .iter()
            .map(|(name, qubits, queue, available)| {
                ((*name).to_owned(), candidate(*qubits, *queue, *available))
            })
            .collect()
    }

    #[test]
    fn picks_least_busy_with_enough_qubits() {
        let candidates = candidates(&[
            ("ibm_boston", 156, 124, true),
            ("ibm_fez", 156, 435, true),
            ("ibm_marrakesh", 156, 3, true),
        ]);
        let request = Request::parse("qubits=128").expect("parses");

        let selection = Basic.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_marrakesh"));
    }

    #[test]
    fn skips_backends_with_too_few_qubits_even_if_idle() {
        let candidates = candidates(&[
            ("ibm_miami", 120, 0, true), // idle but too small
            ("ibm_fez", 156, 435, true),
        ]);
        let request = Request::parse("qubits=128").expect("parses");

        let selection = Basic.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_fez"));
        assert_eq!(
            selection.rejected,
            vec![Rejected {
                name: "ibm_miami".to_owned(),
                reason: Rejection::InsufficientQubits {
                    available: Some(120),
                    required: 128
                },
            }]
        );
    }

    #[test]
    fn skips_unavailable_backends() {
        let candidates = candidates(&[("ibm_down", 156, 0, false), ("ibm_up", 156, 99, true)]);
        let request = Request::parse("qubits=128").expect("parses");

        let selection = Basic.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_up"));
    }

    #[test]
    fn returns_nothing_when_all_are_too_small() {
        let candidates = candidates(&[("ibm_small", 5, 0, true)]);
        let request = Request::parse("qubits=128").expect("parses");

        let selection = Basic.select(&candidates, &request);
        assert_eq!(selection.chosen, None);
        assert_eq!(selection.rejected.len(), 1);
    }

    #[test]
    fn empty_candidate_set_selects_nothing() {
        let selection = Basic.select(&Candidates::new(), &Request::parse("qubits=1").unwrap());
        assert_eq!(selection.chosen, None);
        assert!(selection.rejected.is_empty());
    }

    /// Missing queue depth is treated as empty, matching the upstream `0`
    /// substitution for an absent `pending_job_count`.
    #[test]
    fn missing_queue_depth_counts_as_idle() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_unknown".to_owned(),
            QpuMetrics::builder().qubits(156).available(true).build(),
        );
        candidates.insert("ibm_busy".to_owned(), candidate(156, 10, true));

        let selection = Basic.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_unknown"));
    }

    #[test]
    fn ties_break_by_name_deterministically() {
        let candidates = candidates(&[("ibm_zulu", 156, 7, true), ("ibm_alpha", 156, 7, true)]);
        let request = Request::parse("qubits=1").expect("parses");

        // Run repeatedly: a hash-ordered map would make this flaky.
        for _ in 0..16 {
            let selection = Basic.select(&candidates, &request);
            assert_eq!(selection.name(), Some("ibm_alpha"));
        }
    }

    /// A backend with an unknown qubit count cannot be shown to satisfy the
    /// floor, so it is excluded rather than optimistically accepted.
    #[test]
    fn unknown_qubit_count_is_rejected_when_a_floor_is_set() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_unknown".to_owned(),
            QpuMetrics::builder()
                .pending_jobs(0)
                .available(true)
                .build(),
        );

        let selection = Basic.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.chosen, None);
    }

    /// With no qubit requirement the policy still ranks on queue depth.
    #[test]
    fn without_qubit_requirement_ranks_on_queue_only() {
        let candidates = candidates(&[("ibm_busy", 5, 100, true), ("ibm_idle", 5, 1, true)]);
        let request = Request::parse("clops=1").expect("parses");

        let selection = Basic.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_idle"));
    }
}
