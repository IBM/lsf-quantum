// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Golden-file regression test for the ELIM line format.
//!
//! # Where these fixtures come from
//!
//! `fixtures/py_lines.txt` holds the lines the *original* `elim.qpu`
//! produced for the target documents in `fixtures/targets.json`. They were
//! captured by running the Python itself, via `xval/run.sh`, while
//! `elim.qpu` was still in the tree; the script extracted its formatters
//! with `ast` and compared the two implementations live.
//!
//! Now that the Python has been removed, the recorded lines take its place.
//! The comparison is unchanged -- and still **byte-for-byte**, which is the
//! right standard because LSF parses these lines itself, so a one-character
//! difference is a deployment regression rather than a cosmetic one.
//!
//! The corpus deliberately covers the cases where two implementations
//! diverge: odd and even sample counts (median tie-breaking), absent
//! properties, a missing `pending_job_count`, an unparseable `sample_name`,
//! an empty configuration, an uncalibrated CZ gate, and error values at the
//! extremes of the exponent formatting.
//!
//! **These files are evidence, not test data to be regenerated.** If a
//! change makes this test fail, the change altered what LSF receives.

use lsfq_core::lsf::elim::{format_processor_type, LoadIndices};
use lsfq_core::qrmi::Target;

const TARGETS: &str = include_str!("fixtures/targets.json");
const PYTHON_LINES: &str = include_str!("fixtures/py_lines.txt");

/// Render the ELIM line for one target document, as `elim.qpu` does.
fn render(case: &serde_json::Value) -> String {
    let payload = serde_json::to_string(case).expect("re-serialises");
    let target = Target::parse(&payload).expect("parses");

    let medians = target.medians();

    let processor_type = match target.processor_type() {
        Some(processor) => {
            format_processor_type(Some(&processor.family), processor.revision.as_deref())
        }
        // The original reports "Unknown" when sample_name cannot be parsed.
        None => "Unknown".to_owned(),
    };

    let indices = LoadIndices {
        qubits: target.qubits(),
        clops: target.clops(),
        qpu_version: target.qpu_version(),
        processor_type: Some(processor_type),
        pending_jobs: case
            .get("pending_job_count")
            .and_then(serde_json::Value::as_u64),
        readout_error_median: medians.readout_error_median,
        sx_error_median: medians.sx_error_median,
        cz_error_median: medians.cz_error_median,
        t1_median_us: medians.t1_median_us,
        t2_median_us: medians.t2_median_us,
    };

    indices.render()
}

#[test]
fn elim_lines_match_the_recorded_python_output() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(TARGETS).expect("fixture is valid JSON");

    // The recorded file has one line per case. Trailing whitespace is
    // significant -- the Python emits a trailing space after the last value
    // -- so only the line terminator is stripped.
    let expected: Vec<&str> = PYTHON_LINES.lines().collect();

    assert_eq!(
        cases.len(),
        expected.len(),
        "one recorded line per target document"
    );
    assert!(cases.len() >= 11, "corpus shrank: {} cases", cases.len());

    for (number, (case, want)) in cases.iter().zip(expected.iter()).enumerate() {
        let got = render(case);
        assert_eq!(
            got, *want,
            "case {number} differs from the original elim.qpu\n  python: {want:?}\n  rust:   {got:?}"
        );
    }
}

/// Whatever the input, the line LSF receives must stay parseable: the field
/// count is how LSF reads it.
#[test]
fn every_recorded_line_is_well_formed() {
    for (number, line) in PYTHON_LINES.lines().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();

        assert_eq!(fields.len(), 21, "case {number}: {line:?}");
        assert_eq!(fields[0], "10", "case {number} field count");
    }
}

/// The corpus must contain the degenerate cases, or it proves little.
#[test]
fn the_corpus_covers_the_difficult_cases() {
    let lines: Vec<&str> = PYTHON_LINES.lines().collect();

    assert!(
        lines
            .iter()
            .any(|line| line.contains("processor_type Unknown")),
        "no unparseable sample_name case"
    );
    assert!(
        lines.iter().any(|line| line.contains("T1_median_µs - ")),
        "no missing-calibration case"
    );
    assert!(
        lines.iter().any(|line| line.contains("pending_jobs - ")),
        "no missing-queue-length case"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("e-10") || line.contains("e+0")),
        "no extreme-exponent case"
    );
}
