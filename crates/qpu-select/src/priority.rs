// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The `priority` policy: ordered filtering, then lexicographic ranking.

use alloc::borrow::ToOwned;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::metrics::{Direction, MetricValue, QpuMetrics};
use crate::request::{Request, Requirement};
use crate::selector::{Candidates, Rejected, Rejection, Selection, Selector};

/// Selects a backend by applying the request's requirements in order.
///
/// Equivalent to the surviving Python `select_device_priority()`. The
/// algorithm:
///
/// 1. Start with every available backend.
/// 2. Apply each requirement in the order written, discarding backends that
///    do not satisfy it. If none remain, select nothing.
/// 3. If one remains, select it.
/// 4. Otherwise rank the survivors lexicographically on the requested
///    metrics, in the same order: better values first, backends that do not
///    report a metric last.
/// 5. Break any remaining tie by backend name, for determinism.
///
/// # On the two upstream descriptions
///
/// The Python docstring and `README.md` describe step 4 as "retain every
/// backend tied at the best value, then continue with the next requirement".
/// The code does not do that: it builds a ranking key per backend and takes
/// the minimum (`min(candidates, key=ranking_key)`). The two agree whenever
/// a selection exists, because lexicographic minimum over the same ordered
/// keys is exactly what iterated tie-retention converges to. This
/// implementation follows the code, which is what deployed clusters observe,
/// and expresses the ranking explicitly as a lexicographic comparison.
#[derive(Debug, Clone, Copy, Default)]
pub struct Priority;

/// One component of a backend's ranking key.
///
/// Ordered so that a reported value always sorts before a missing one,
/// mirroring the Python key of `(0, value)` for present and `(1, 0.0)` for
/// absent.
#[derive(Debug, Clone, PartialEq)]
enum KeyPart {
    /// A value to be minimised (maximised metrics are negated on
    /// construction, so a single direction suffices here).
    Ranked(f64),
    /// The metric is not ranked, or the backend does not report it.
    Unranked,
}

impl KeyPart {
    fn rank(&self) -> u8 {
        match self {
            Self::Ranked(_) => 0,
            Self::Unranked => 1,
        }
    }

    fn cmp_part(&self, other: &Self) -> Ordering {
        match self.rank().cmp(&other.rank()) {
            Ordering::Equal => match (self, other) {
                (Self::Ranked(a), Self::Ranked(b)) => {
                    // Keys are finite by construction; a NaN metric is
                    // filtered out before ranking.
                    a.partial_cmp(b).unwrap_or(Ordering::Equal)
                }
                _ => Ordering::Equal,
            },
            unequal => unequal,
        }
    }
}

fn ranking_key(metrics: &QpuMetrics, requirements: &[Requirement]) -> Vec<KeyPart> {
    requirements
        .iter()
        .map(|requirement| {
            let reported = metrics.get_path(requirement.metric, requirement.field.as_deref());

            let Some(MetricValue::Number(value)) = reported else {
                // Either the metric is descriptive (processor_type,
                // qpu_version), which is matched rather than ranked, or the
                // backend does not report it.
                return KeyPart::Unranked;
            };

            if !value.is_finite() {
                return KeyPart::Unranked;
            }

            match requirement.metric.direction() {
                // Negate so that every part is minimised.
                Some(Direction::Maximize) => KeyPart::Ranked(-value),
                Some(Direction::Minimize) => KeyPart::Ranked(value),
                None => KeyPart::Unranked,
            }
        })
        .collect()
}

fn compare_keys(left: &[KeyPart], right: &[KeyPart]) -> Ordering {
    for (a, b) in left.iter().zip(right.iter()) {
        match a.cmp_part(b) {
            Ordering::Equal => {}
            unequal => return unequal,
        }
    }
    Ordering::Equal
}

impl Selector for Priority {
    fn name(&self) -> &'static str {
        "priority"
    }

    fn select(&self, candidates: &Candidates, request: &Request) -> Selection {
        let requirements = request.requirements();
        let mut rejected = Vec::new();

        // Step 1: available backends only.
        let mut surviving: Vec<(&str, &QpuMetrics)> = Vec::new();
        for (name, metrics) in candidates {
            if metrics.available {
                surviving.push((name.as_str(), metrics));
            } else {
                rejected.push(Rejected {
                    name: name.clone(),
                    reason: Rejection::Unavailable,
                });
            }
        }

        // Step 2: apply each requirement in order.
        for requirement in requirements {
            let mut matching = Vec::with_capacity(surviving.len());

            for (name, metrics) in surviving {
                let reported = metrics.get_path(requirement.metric, requirement.field.as_deref());

                if requirement.is_satisfied_by(reported.as_ref()) {
                    matching.push((name, metrics));
                } else {
                    rejected.push(Rejected {
                        name: name.to_owned(),
                        reason: Rejection::FailedRequirement {
                            requirement: describe(requirement),
                            reported: reported.map(|value| value.to_string()),
                        },
                    });
                }
            }

            surviving = matching;

            if surviving.is_empty() {
                return Selection::none(rejected);
            }
        }

        // Steps 3-5: rank the survivors, breaking ties by name.
        //
        // `surviving` is in name order because `candidates` iterates sorted
        // and the filters above preserve order, so a strict `Less` keeps the
        // earliest name on a tie.
        let mut best: Option<(&str, &QpuMetrics, Vec<KeyPart>)> = None;

        for (name, metrics) in surviving {
            let key = ranking_key(metrics, requirements);

            let is_better = match &best {
                None => true,
                Some((_, _, best_key)) => compare_keys(&key, best_key) == Ordering::Less,
            };

            if is_better {
                best = Some((name, metrics, key));
            }
        }

        let chosen = best.map(|(name, metrics, _)| (name.to_owned(), metrics.clone()));

        Selection { chosen, rejected }
    }
}

/// Render a requirement the way the user wrote it, for diagnostics.
fn describe(requirement: &Requirement) -> String {
    let mut text = String::new();
    text.push_str(requirement.metric.as_str());
    if let Some(field) = &requirement.field {
        text.push('.');
        text.push_str(field);
    }
    text.push_str(requirement.comparison.as_str());
    text.push_str(&requirement.value.to_string());
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ProcessorType;
    use alloc::vec;

    fn base(qubits: u32) -> QpuMetrics {
        QpuMetrics::builder().qubits(qubits).available(true).build()
    }

    #[test]
    fn filters_then_selects_the_single_survivor() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_small".to_owned(), base(5));
        candidates.insert("ibm_big".to_owned(), base(156));

        let request = Request::parse("qubits=128").expect("parses");
        let selection = Priority.select(&candidates, &request);

        assert_eq!(selection.name(), Some("ibm_big"));
    }

    #[test]
    fn selects_nothing_when_a_requirement_excludes_everything() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_small".to_owned(), base(5));

        let selection = Priority.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.chosen, None);
        assert_eq!(selection.rejected.len(), 1);
    }

    /// With several survivors, ranking follows requirement order: the first
    /// requirement dominates.
    #[test]
    fn ranks_on_the_first_requirement_first() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_many_qubits".to_owned(),
            QpuMetrics::builder()
                .qubits(200)
                .clops(100.0)
                .available(true)
                .build(),
        );
        candidates.insert(
            "ibm_fast".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .clops(900.0)
                .available(true)
                .build(),
        );

        // qubits first: the larger machine wins despite lower CLOPS.
        let selection =
            Priority.select(&candidates, &Request::parse("qubits=128;clops=50").unwrap());
        assert_eq!(selection.name(), Some("ibm_many_qubits"));

        // clops first: the faster machine wins.
        let selection =
            Priority.select(&candidates, &Request::parse("clops=50;qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_fast"));
    }

    #[test]
    fn minimize_metrics_rank_ascending() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_busy".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .pending_jobs(400)
                .available(true)
                .build(),
        );
        candidates.insert(
            "ibm_idle".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .pending_jobs(2)
                .available(true)
                .build(),
        );

        let request = Request::parse("pending_jobs=500;qubits=128").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_idle"));
    }

    #[test]
    fn processor_type_is_matched_by_family() {
        let mut candidates = Candidates::new();
        let mut heron = base(156);
        heron.processor_type = Some(ProcessorType::new("Heron").with_revision("2"));
        let mut nighthawk = base(156);
        nighthawk.processor_type = Some(ProcessorType::new("Nighthawk"));
        candidates.insert("ibm_heron".to_owned(), heron);
        candidates.insert("ibm_nighthawk".to_owned(), nighthawk);

        let request = Request::parse("processor_type=Nighthawk").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_nighthawk"));
    }

    /// A revision is stored as text, so a quoted request value matches it.
    #[test]
    fn dotted_processor_revision_is_matched() {
        let mut candidates = Candidates::new();
        let mut r1 = base(156);
        r1.processor_type = Some(ProcessorType::new("Heron").with_revision("1"));
        let mut r2 = base(156);
        r2.processor_type = Some(ProcessorType::new("Heron").with_revision("2"));
        candidates.insert("ibm_r1".to_owned(), r1);
        candidates.insert("ibm_r2".to_owned(), r2);

        let request = Request::parse("processor_type.revision='2'").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_r2"));
    }

    /// An *unquoted* `2` parses as a number, and a number never equals the
    /// text `"2"`, so nothing matches. This is faithful to the Python
    /// `satisfies()`, which coerces both sides only when both are numeric or
    /// both are strings and otherwise falls through to a false comparison.
    /// Documented as a test because it is surprising, and because the
    /// behaviour is part of the compatibility surface rather than something
    /// this port is free to improve.
    #[test]
    fn unquoted_numeric_revision_does_not_match_text() {
        let mut candidates = Candidates::new();
        let mut r2 = base(156);
        r2.processor_type = Some(ProcessorType::new("Heron").with_revision("2"));
        candidates.insert("ibm_r2".to_owned(), r2);

        let request = Request::parse("processor_type.revision=2").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.chosen, None);
    }

    #[test]
    fn skips_unavailable_backends() {
        let mut candidates = Candidates::new();
        let mut down = QpuMetrics::builder().qubits(500).build();
        down.available = false;
        candidates.insert("ibm_down".to_owned(), down);
        candidates.insert("ibm_up".to_owned(), base(156));

        let selection = Priority.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(selection.name(), Some("ibm_up"));
    }

    /// A backend not reporting a ranked metric must sort after one that does,
    /// matching the Python `(1, 0.0)` key for absent values.
    #[test]
    fn backends_missing_a_ranked_metric_sort_last() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_aaa_no_clops".to_owned(),
            QpuMetrics::builder().qubits(156).available(true).build(),
        );
        candidates.insert(
            "ibm_zzz_clops".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .clops(1.0)
                .available(true)
                .build(),
        );

        // `clops!=none` keeps both, so ranking decides.
        let request = Request::parse("qubits=128;clops!=none").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_zzz_clops"));
    }

    #[test]
    fn ties_break_by_name_deterministically() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_zulu".to_owned(), base(156));
        candidates.insert("ibm_alpha".to_owned(), base(156));

        for _ in 0..16 {
            let selection = Priority.select(&candidates, &Request::parse("qubits=128").unwrap());
            assert_eq!(selection.name(), Some("ibm_alpha"));
        }
    }

    #[test]
    fn rejections_explain_the_failed_requirement() {
        let mut candidates = Candidates::new();
        candidates.insert("ibm_small".to_owned(), base(5));

        let selection = Priority.select(&candidates, &Request::parse("qubits=128").unwrap());
        assert_eq!(
            selection.rejected,
            vec![Rejected {
                name: "ibm_small".to_owned(),
                reason: Rejection::FailedRequirement {
                    requirement: "qubits>=128".to_owned(),
                    reported: Some("5".to_owned()),
                },
            }]
        );
    }

    /// The README example: an explicit upper bound on queue depth.
    #[test]
    fn readme_pending_jobs_example() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_over".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .pending_jobs(250)
                .available(true)
                .build(),
        );
        candidates.insert(
            "ibm_under".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .pending_jobs(42)
                .available(true)
                .build(),
        );

        let request = Request::parse("pending_jobs<100").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_under"));
    }

    /// The README example: lowest median readout error.
    #[test]
    fn readme_readout_error_example() {
        let mut candidates = Candidates::new();
        candidates.insert(
            "ibm_noisy".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .readout_error_median(0.02)
                .available(true)
                .build(),
        );
        candidates.insert(
            "ibm_clean".to_owned(),
            QpuMetrics::builder()
                .qubits(156)
                .readout_error_median(0.004)
                .available(true)
                .build(),
        );

        let request = Request::parse("readout_error_median=1").expect("parses");
        let selection = Priority.select(&candidates, &request);
        assert_eq!(selection.name(), Some("ibm_clean"));
    }

    #[test]
    fn empty_candidates_select_nothing() {
        let selection = Priority.select(&Candidates::new(), &Request::parse("qubits=1").unwrap());
        assert_eq!(selection.chosen, None);
    }
}
