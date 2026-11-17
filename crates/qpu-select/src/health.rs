// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The `health` policy: a weighted composite score.
//!
//! Scoring formula by Randy Yang (IBM Spectrum Computing Intern, 2026),
//! carried over from the Python implementation.

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::metrics::Metric;
use crate::request::Request;
use crate::selector::{Candidates, Rejected, Rejection, Selection, Selector};

/// Reference coherence time in microseconds.
///
/// Normalises T1 so that a 200 us coherence time contributes about 1.0 to the
/// score.
pub const T1_NORM_US: f64 = 200.0;

/// Floor added to the readout error, so a reported error of exactly zero
/// does not divide by zero.
pub const EPSILON: f64 = 0.001;

/// Compute the composite health score. Higher is better.
///
/// ```text
/// score = (T1 / T1_NORM) * (1 / (readout_error + EPSILON)) * (1 / (queue + 1))
/// ```
///
/// All three factors matter: a QPU with excellent coherence but 500 queued
/// jobs loses to a slightly worse one with 5.
///
/// # Example
///
/// ```
/// use qpu_select::health;
///
/// // 200 us T1, 1% readout error, empty queue.
/// let score = health::score(200.0, 0.01, 0);
/// assert!((score - 90.909).abs() < 0.01);
///
/// // The same QPU with 99 jobs queued scores a hundredth as well.
/// let busy = health::score(200.0, 0.01, 99);
/// assert!((busy - score / 100.0).abs() < 1e-9);
/// ```
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn score(t1_us: f64, readout_error: f64, queue_depth: u64) -> f64 {
    let t1_component = t1_us / T1_NORM_US;
    let error_component = 1.0 / (readout_error + EPSILON);
    let queue_component = 1.0 / (queue_depth as f64 + 1.0);

    t1_component * error_component * queue_component
}

/// Selects the healthiest available backend with enough qubits.
///
/// Weighs T1 coherence, readout error and queue depth together via
/// [`score`]. Equivalent to the Python `select_device_health()`, with one
/// deliberate difference in how missing data is handled.
///
/// # Missing calibration data
///
/// The Python version reads T1 and the readout error with
/// `element.get('T1_median_µs', 0.0)` and
/// `element.get('readout_error_median', 1.0)`. Those defaults are not
/// neutral: a T1 of `0.0` forces the whole product to zero, so every
/// backend missing calibration data scores exactly `0.0000` and the policy
/// degenerates into "first backend examined". That is visible in the worked
/// example in `README.md`, where all six QPUs score `0.0000` and the
/// selection is effectively arbitrary.
///
/// This implementation instead *excludes* a backend that cannot be scored,
/// reporting [`Rejection::MissingMetric`], so a caller can see that
/// calibration data was unavailable rather than silently ranking on nothing.
/// A missing queue depth still counts as an empty queue, matching `basic`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Health;

impl Selector for Health {
    fn name(&self) -> &'static str {
        "health"
    }

    fn select(&self, candidates: &Candidates, request: &Request) -> Selection {
        let required = request.required_qubits();
        let mut rejected = Vec::new();
        let mut best: Option<(&str, f64)> = None;

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

            // Both scoring inputs must be present, or the score is
            // meaningless; see the type docs.
            let Some(t1_us) = metrics.t1_median_us else {
                rejected.push(Rejected {
                    name: name.clone(),
                    reason: Rejection::MissingMetric {
                        metric: Metric::T1MedianUs.to_string(),
                    },
                });
                continue;
            };

            let Some(readout_error) = metrics.readout_error_median else {
                rejected.push(Rejected {
                    name: name.clone(),
                    reason: Rejection::MissingMetric {
                        metric: Metric::ReadoutErrorMedian.to_string(),
                    },
                });
                continue;
            };

            let candidate_score = score(t1_us, readout_error, metrics.pending_jobs.unwrap_or(0));

            if !candidate_score.is_finite() {
                rejected.push(Rejected {
                    name: name.clone(),
                    reason: Rejection::MissingMetric {
                        metric: "a finite health score".into(),
                    },
                });
                continue;
            }

            // Strictly-greater keeps the first backend in name order on a
            // tie, since `candidates` iterates sorted.
            if best.is_none_or(|(_, best_score)| candidate_score > best_score) {
                best = Some((name.as_str(), candidate_score));
            }
        }

        let chosen = best.map(|(name, _)| {
            let metrics = candidates[name].clone();
            (name.into(), metrics)
        });

        Selection { chosen, rejected }
    }
}

impl Health {
    /// Score a single backend by name, for diagnostics.
    ///
    /// Returns `None` if the backend is absent or lacks the metrics the
    /// score needs.
    #[must_use]
    pub fn score_of(candidates: &Candidates, name: &str) -> Option<f64> {
        let metrics = candidates.get(name)?;
        Some(score(
            metrics.t1_median_us?,
            metrics.readout_error_median?,
            metrics.pending_jobs.unwrap_or(0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::QpuMetrics;
    use alloc::borrow::ToOwned;
    use alloc::string::String;
    use alloc::vec;

    fn healthy(qubits: u32, t1: f64, readout: f64, queue: u64) -> QpuMetrics {
        QpuMetrics::builder()
            .qubits(qubits)
            .t1_median_us(t1)
            .readout_error_median(readout)
            .pending_jobs(queue)
            .available(true)
            .build()
    }

    #[test]
    fn score_matches_the_documented_formula() {
        // (200/200) * (1/(0.01+0.001)) * (1/(0+1)) = 90.909...
        let expected = (200.0 / T1_NORM_US) * (1.0 / (0.01 + EPSILON)) * (1.0 / 1.0);
        assert!((score(200.0, 0.01, 0) - expected).abs() < 1e-12);
    }

    #[test]
    fn zero_readout_error_does_not_divide_by_zero() {
        let value = score(200.0, 0.0, 0);
        assert!(value.is_finite());
        assert!((value - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn queue_depth_dominates_a_small_coherence_edge() {
        // The docstring's claim: great coherence with a deep queue loses.
        let pristine_but_busy = score(300.0, 0.005, 500);
        let decent_and_idle = score(150.0, 0.010, 5);
        assert!(decent_and_idle > pristine_but_busy);
    }

    #[test]
    fn picks_the_highest_scoring_backend() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_busy".to_owned(), healthy(156, 180.0, 0.009, 400));
        candidates.insert("ibm_good".to_owned(), healthy(156, 180.0, 0.009, 2));
        candidates.insert("ibm_noisy".to_owned(), healthy(156, 180.0, 0.090, 2));

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_good"));
    }

    #[test]
    fn respects_the_qubit_floor() {
        let mut candidates = Candidates::new();
        // Small but otherwise perfect.
        candidates.insert("ibm_small".to_owned(), healthy(120, 400.0, 0.0001, 0));
        candidates.insert("ibm_big".to_owned(), healthy(156, 100.0, 0.02, 50));

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_big"));
    }

    #[test]
    fn skips_unavailable_backends() {
        let mut candidates = Candidates::new();
        let mut down = healthy(156, 400.0, 0.0001, 0);
        down.available = false;
        candidates.insert("ibm_down".to_owned(), down);
        candidates.insert("ibm_up".to_owned(), healthy(156, 100.0, 0.02, 9));

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_up"));
    }

    /// The upstream defect: with T1 defaulted to 0.0 every backend scores
    /// zero and selection becomes arbitrary. Here such a backend is excluded
    /// with a stated reason instead.
    #[test]
    fn backends_without_calibration_data_are_excluded_not_zero_scored() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_nocalib".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .pending_jobs(3)
                .available(true)
                .build(),
        );

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(
            selection.chosen, None,
            "must not pick an unscoreable backend"
        );
        assert_eq!(
            selection.rejected,
            vec![Rejected {
                name: "ibm_nocalib".to_owned(),
                reason: Rejection::MissingMetric {
                    metric: "T1_median_us".to_owned()
                },
            }]
        );
    }

    /// A scoreable backend must win over an unscoreable one regardless of
    /// map order.
    #[test]
    fn scoreable_backend_wins_over_unscoreable() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_aaa_nocalib".to_owned(),
            QpuMetrics::builder().qubits(156).available(true).build(),
        );
        candidates.insert("ibm_zzz_good".to_owned(), healthy(156, 150.0, 0.01, 1));

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_zzz_good"));
    }

    #[test]
    fn missing_queue_depth_counts_as_idle() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_unknown_queue".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .t1_median_us(150.0)
                .readout_error_median(0.01)
                .available(true)
                .build(),
        );
        candidates.insert("ibm_known".to_owned(), healthy(156, 150.0, 0.01, 10));

        let selection = Health.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_unknown_queue"));
    }

    #[test]
    fn ties_break_by_name_deterministically() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_zulu".to_owned(), healthy(156, 150.0, 0.01, 4));
        candidates.insert("ibm_alpha".to_owned(), healthy(156, 150.0, 0.01, 4));

        for _ in 0..16 {
            let selection = Health.select(&candidates, &Request::parse("qubits=1").unwrap());
            assert_eq!(selection.name(), Some("ibm_alpha"));
        }
    }

    #[test]
    fn non_finite_inputs_are_excluded() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_nan".to_owned(), healthy(156, f64::NAN, 0.01, 0));

        let selection = Health.select(&candidates, &Request::parse("qubits=1").unwrap());
        assert_eq!(selection.chosen, None);
    }

    #[test]
    fn score_of_reads_a_named_backend() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_fez".to_owned(), healthy(156, 200.0, 0.01, 0));

        let value = Health::score_of(&candidates, "ibm_fez").expect("scoreable");
        assert!((value - score(200.0, 0.01, 0)).abs() < 1e-12);
        assert_eq!(Health::score_of(&candidates, "absent"), None);
    }

    #[test]
    fn rejection_messages_are_legible() {
        let reason = Rejection::InsufficientQubits {
            available: Some(120),
            required: 128,
        };
        assert_eq!(
            String::from("only 120 qubits < 128 required"),
            reason.to_string()
        );
    }
}
