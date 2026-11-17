// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The reporting cycle.
//!
//! One cycle fetches the QPU's target and status and turns them into a
//! [`LoadIndices`]. The loop that repeats it lives in [`crate::main`]; the
//! cycle itself is separated out so the two robustness invariants the
//! original ELIM's comments call out can be tested without a QPU:
//!
//! 1. **A fetch failure must not end the process.** The original warns twice
//!    that "an ELIM that exits takes every index down with it" -- LSF stops
//!    receiving *all* of that host's indices, not just the one that failed.
//!    A failed cycle therefore reports what it has and keeps the field count
//!    intact.
//! 2. **The line always carries ten pairs.** LSF parses by field count, so a
//!    short line is misread rather than ignored.
//!
//! [`QpuSource`] exists for that testability: the real implementation calls
//! QRMI, and the tests substitute a source that fails on demand.

use lsfq_core::lsf::elim::{format_processor_type, LoadIndices};
use lsfq_core::qrmi::Target;

/// What one poll of a QPU returns.
///
/// Either half may be missing: the service can answer one query and not the
/// other, which the original handles by reporting the available fields and
/// substituting `-` for the rest.
#[derive(Debug, Clone, Default)]
pub struct Reading {
    /// The `target()` payload, if it was fetched.
    pub target: Option<String>,
    /// The queue depth from `status()`, if it was reported.
    pub pending_jobs: Option<u64>,
    /// Whether `status()` answered at all.
    ///
    /// Distinguished from `pending_jobs: None`, because a vendor may report
    /// a status without a queue length -- which is also the case on QRMI
    /// builds older than 0.25.1, where only `is_accessible()` exists.
    pub status_available: bool,
}

/// A source of QPU readings.
///
/// Implemented against QRMI in [`crate::qrmi_source`]; implemented against
/// canned data in the tests below.
///
/// The method is `async` because QRMI's is, and because the reporting loop
/// already runs inside the runtime: a source that blocked on a runtime
/// handle from here would panic with "cannot start a runtime from within a
/// runtime". Keeping the await at the trait boundary makes that impossible
/// to get wrong in an implementation.
pub trait QpuSource {
    /// Fetch one reading.
    ///
    /// Must not fail: a transport error is reported as absent data so the
    /// caller can emit a well-formed line regardless. Anything worth telling
    /// the operator is returned in `warnings`.
    fn poll(&mut self) -> impl core::future::Future<Output = (Reading, Vec<String>)>;
}

/// Turn a reading into the indices to report.
///
/// A target that is absent, or present but unparseable, yields the
/// all-unavailable line rather than a short one.
#[must_use]
pub fn indices_from(reading: &Reading) -> (LoadIndices, Vec<String>) {
    let mut warnings = Vec::new();

    let Some(payload) = reading.target.as_deref() else {
        // DELIBERATE DIFFERENCE from elim.qpu, which does
        //
        //     if not device_config: print_debug(...); time.sleep(...); continue
        //
        // and so emits *nothing* for a cycle whose target fetch failed.
        //
        // An ELIM that goes quiet is indistinguishable, from LSF's side,
        // from one that is merely slow: LSF keeps serving the last values it
        // received, so during an outage it schedules against calibration and
        // queue figures that are minutes or hours stale. Reporting `-`
        // instead tells LSF the values are unavailable, which is what `-`
        // exists for, and lets it stop placing work it cannot reason about.
        //
        // The field count is unchanged either way, so no LSF configuration
        // is affected. Verified against the original for the adjacent case
        // -- a target that parses but carries an empty configuration -- which
        // yields "Unknown" for processor_type in both implementations.
        return (LoadIndices::unavailable(), warnings);
    };

    let target = match Target::parse(payload) {
        Ok(target) => target,
        Err(error) => {
            warnings.push(format!("target payload is not valid JSON: {error}"));
            return (LoadIndices::unavailable(), warnings);
        }
    };

    let medians = target.medians();

    let processor_type = match target.processor_type() {
        Some(processor) => {
            format_processor_type(Some(&processor.family), processor.revision.as_deref())
        }
        // The original reports "Unknown" when sample_name cannot be parsed,
        // rather than leaving the index unavailable.
        None => "Unknown".to_owned(),
    };

    if !reading.status_available {
        warnings.push("no status of quantum device".to_owned());
    }

    let indices = LoadIndices {
        qubits: target.qubits(),
        clops: target.clops(),
        qpu_version: target.qpu_version(),
        processor_type: Some(processor_type),
        pending_jobs: reading.pending_jobs,
        readout_error_median: medians.readout_error_median,
        sx_error_median: medians.sx_error_median,
        cz_error_median: medians.cz_error_median,
        t1_median_us: medians.t1_median_us,
        t2_median_us: medians.t2_median_us,
    };

    (indices, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_TARGET: &str = r#"{
      "configuration": {
        "n_qubits": 156,
        "backend_version": "1.3.29",
        "clops_h": 320000,
        "sample_name": "family: Heron, revision: 2"
      },
      "properties": {
        "qubits": [[
          {"name": "T1", "value": 141.29, "unit": "us"},
          {"name": "T2", "value": 101.06, "unit": "us"},
          {"name": "readout_error", "value": 0.0094}
        ]],
        "gates": [
          {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.00027}]},
          {"gate": "cz", "parameters": [{"name": "gate_error", "value": 0.00265}]}
        ]
      }
    }"#;

    /// Every line must carry the count plus ten name/value pairs, whatever
    /// went wrong. LSF parses by field count.
    fn assert_well_formed(line: &str) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 21, "malformed line: {line}");
        assert_eq!(fields[0], "10", "field count must be 10: {line}");
    }

    #[test]
    fn a_complete_reading_reports_every_index() {
        let reading = Reading {
            target: Some(FULL_TARGET.to_owned()),
            pending_jobs: Some(59),
            status_available: true,
        };

        let (indices, warnings) = indices_from(&reading);
        let line = indices.render();

        assert_well_formed(&line);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(line.contains("qubits 156 "), "{line}");
        assert!(line.contains("processor_type Heron_r2 "), "{line}");
        assert!(line.contains("pending_jobs 59 "), "{line}");
        assert!(line.contains("T1_median_µs 141.29 "), "{line}");
    }

    /// Invariant 1: a failed target fetch still produces a parseable line.
    #[test]
    fn a_missing_target_still_yields_a_well_formed_line() {
        let (indices, _) = indices_from(&Reading::default());
        let line = indices.render();

        assert_well_formed(&line);
        // Every value is the "not available" marker.
        for position in 0..10 {
            assert_eq!(line.split_whitespace().nth(2 + 2 * position), Some("-"));
        }
    }

    #[test]
    fn an_unparseable_target_is_reported_and_degrades() {
        let reading = Reading {
            target: Some("{not json".to_owned()),
            pending_jobs: Some(1),
            status_available: true,
        };

        let (indices, warnings) = indices_from(&reading);

        assert_well_formed(&indices.render());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not valid JSON"), "{warnings:?}");
    }

    /// Invariant 2: a status failure leaves the other nine indices intact and
    /// marks only `pending_jobs` unavailable.
    #[test]
    fn a_status_failure_only_costs_the_queue_index() {
        let reading = Reading {
            target: Some(FULL_TARGET.to_owned()),
            pending_jobs: None,
            status_available: false,
        };

        let (indices, warnings) = indices_from(&reading);
        let line = indices.render();

        assert_well_formed(&line);
        assert!(line.contains("pending_jobs - "), "{line}");
        // The nine indices that do not depend on status are unaffected.
        assert!(line.contains("qubits 156 "), "{line}");
        assert!(line.contains("T1_median_µs 141.29 "), "{line}");
        assert_eq!(warnings, vec!["no status of quantum device".to_owned()]);
    }

    /// A status with no queue length -- a vendor omission, or a QRMI build
    /// older than 0.25.1 where only `is_accessible()` exists.
    #[test]
    fn a_status_without_a_queue_length_is_not_a_failure() {
        let reading = Reading {
            target: Some(FULL_TARGET.to_owned()),
            pending_jobs: None,
            status_available: true,
        };

        let (indices, warnings) = indices_from(&reading);

        assert!(indices.render().contains("pending_jobs - "));
        assert!(
            warnings.is_empty(),
            "a reported status is not a failure: {warnings:?}"
        );
    }

    /// Calibration data absent but configuration present: the case the
    /// original ELIM's comments single out.
    #[test]
    fn a_target_without_properties_reports_configuration_only() {
        let reading = Reading {
            target: Some(r#"{"configuration": {"n_qubits": 156}}"#.to_owned()),
            pending_jobs: Some(3),
            status_available: true,
        };

        let (indices, _) = indices_from(&reading);
        let line = indices.render();

        assert_well_formed(&line);
        assert!(line.contains("qubits 156 "), "{line}");
        assert!(line.contains("readout_error_median - "), "{line}");
        assert!(line.contains("T1_median_µs - "), "{line}");
    }

    /// A target that parses but carries an empty configuration. Verified
    /// against the original, whose `get_system_config({})` also yields
    /// `"Unknown"` for `processor_type` and `None` for the rest -- so this is
    /// the case where the two implementations must agree exactly, unlike a
    /// failed fetch (see `indices_from`).
    #[test]
    fn an_empty_configuration_matches_the_original() {
        let reading = Reading {
            target: Some(r#"{"configuration": {}, "properties": {}}"#.to_owned()),
            pending_jobs: None,
            status_available: true,
        };

        let (indices, _) = indices_from(&reading);
        let line = indices.render();

        assert_well_formed(&line);
        assert!(line.contains("processor_type Unknown "), "{line}");
        assert!(line.contains("qubits - "), "{line}");
        assert!(line.contains("clops - "), "{line}");
        assert!(line.contains("qpu_version - "), "{line}");
    }

    #[test]
    fn an_unparseable_sample_name_reports_unknown() {
        let reading = Reading {
            target: Some(r#"{"configuration": {"sample_name": "garbage"}}"#.to_owned()),
            pending_jobs: None,
            status_available: true,
        };

        let (indices, _) = indices_from(&reading);
        assert!(indices.render().contains("processor_type Unknown "));
    }

    /// A source that fails must never be able to produce a malformed line,
    /// whatever combination of failures it reports.
    #[test]
    fn every_failure_combination_stays_well_formed() {
        let targets = [None, Some("{bad".to_owned()), Some(FULL_TARGET.to_owned())];
        let queues = [None, Some(0), Some(999_999)];

        for target in &targets {
            for queue in queues {
                for status_available in [false, true] {
                    let reading = Reading {
                        target: target.clone(),
                        pending_jobs: queue,
                        status_available,
                    };
                    assert_well_formed(&indices_from(&reading).0.render());
                }
            }
        }
    }

    /// A stub source, standing in for QRMI.
    struct Stub {
        readings: Vec<Reading>,
        index: usize,
    }

    impl QpuSource for Stub {
        // No `.await` inside: the canned readings are already in memory.
        fn poll(&mut self) -> impl core::future::Future<Output = (Reading, Vec<String>)> {
            let reading = self.readings.get(self.index).cloned().unwrap_or_default();
            self.index += 1;
            core::future::ready((reading, Vec::new()))
        }
    }

    /// A source that fails, then recovers, must keep reporting throughout --
    /// this is the behaviour that keeps a transient outage from taking the
    /// host's indices down.
    #[tokio::test]
    async fn a_transient_failure_is_survived() {
        let mut source = Stub {
            readings: vec![
                Reading {
                    target: Some(FULL_TARGET.to_owned()),
                    pending_jobs: Some(5),
                    status_available: true,
                },
                // The service drops out for one cycle.
                Reading::default(),
                Reading {
                    target: Some(FULL_TARGET.to_owned()),
                    pending_jobs: Some(7),
                    status_available: true,
                },
            ],
            index: 0,
        };

        let mut lines = Vec::new();
        for _ in 0..3 {
            let (reading, _) = source.poll().await;
            lines.push(indices_from(&reading).0.render());
        }

        for line in &lines {
            assert_well_formed(line);
        }

        assert!(lines[0].contains("pending_jobs 5 "));
        assert!(lines[1].contains("qubits - "), "the failed cycle degrades");
        assert!(
            lines[2].contains("pending_jobs 7 "),
            "and the next recovers"
        );
    }
}
