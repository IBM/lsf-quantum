// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Golden-file regression test for the `priority` policy.
//!
//! # Where these fixtures come from
//!
//! `fixtures/py_results.json` holds the decisions the *original Python*
//! `select_device_priority()` made over `fixtures/corpus.txt` -- 40
//! randomised fleets by 25 requests, 1000 pairs, with each metric
//! independently present or absent so the missing-value paths are
//! exercised. They were captured by running the Python itself, via
//! `xval/run.sh`, while `qrmi-esub-jobstarter.py` was still in the tree.
//!
//! That script extracted the selector with `ast` and compared the two
//! implementations live. Now that the Python has been removed, the recorded
//! outputs take its place: the comparison is the same, but one side is
//! frozen. **These files are evidence, not test data to be regenerated** --
//! if a change makes this test fail, the change altered which backend a job
//! lands on, and that is the finding, not the fixture's fault.
//!
//! The corpus format is the dependency-free one the old Rust driver read,
//! so this crate still needs no JSON dependency:
//!
//! ```text
//! F <name> k=v k=v ...   one backend
//! ---                    end of a fleet
//! R <request>            the requests, after all fleets
//! ```

use std::collections::BTreeSet;

use qpu_select::{Candidates, Priority, ProcessorType, QpuMetrics, Request, Selector};

const CORPUS: &str = include_str!("fixtures/corpus.txt");
/// The Python's decisions, as a JSON array of strings and nulls.
const PYTHON_RESULTS: &str = include_str!("fixtures/py_results.json");

/// Parse one `F` line into a backend.
fn parse_backend(fields: &[&str]) -> (String, QpuMetrics) {
    let name = fields[0].to_owned();
    let mut builder = QpuMetrics::builder().available(true);

    for field in &fields[1..] {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        builder = match key {
            "qubits" => builder.qubits(value.parse().expect("qubits")),
            "clops" => builder.clops(value.parse().expect("clops")),
            "pending_jobs" => builder.pending_jobs(value.parse().expect("pending_jobs")),
            "T1_median_us" => builder.t1_median_us(value.parse().expect("t1")),
            "T2_median_us" => builder.t2_median_us(value.parse().expect("t2")),
            "readout_error_median" => builder.readout_error_median(value.parse().expect("readout")),
            "processor_type" => {
                let (family, revision) = value.split_once(':').expect("family:revision");
                builder.processor_type(ProcessorType::new(family).with_revision(revision))
            }
            other => panic!("unknown metric in corpus: {other}"),
        };
    }

    (name, builder.build())
}

fn load_corpus() -> (Vec<Candidates>, Vec<String>) {
    let mut fleets = Vec::new();
    let mut current = Candidates::new();
    let mut requests = Vec::new();

    for line in CORPUS.lines() {
        let line = line.trim();
        if line == "---" {
            fleets.push(std::mem::take(&mut current));
        } else if let Some(rest) = line.strip_prefix("F ") {
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let (name, metrics) = parse_backend(&fields);
            current.insert(name, metrics);
        } else if let Some(rest) = line.strip_prefix("R ") {
            requests.push(rest.to_owned());
        }
    }

    (fleets, requests)
}

/// Read the recorded decisions.
///
/// Hand-parsed rather than pulling in `serde_json`: the file is a flat array
/// of strings and nulls, and `qpu-select` is deliberately dependency-free.
fn load_python_results() -> Vec<Option<String>> {
    let trimmed = PYTHON_RESULTS.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .expect("a JSON array");

    inner
        .split(',')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            if piece == "null" {
                None
            } else {
                Some(
                    piece
                        .strip_prefix('"')
                        .and_then(|rest| rest.strip_suffix('"'))
                        .expect("a quoted backend name")
                        .to_owned(),
                )
            }
        })
        .collect()
}

#[test]
fn priority_matches_the_recorded_python_decisions() {
    let (fleets, requests) = load_corpus();
    let expected = load_python_results();

    assert_eq!(fleets.len(), 40, "corpus fleet count");
    assert_eq!(requests.len(), 25, "corpus request count");
    assert_eq!(
        expected.len(),
        fleets.len() * requests.len(),
        "one recorded decision per fleet/request pair"
    );

    let mut mismatches = Vec::new();
    let mut index = 0;

    for (fleet_number, fleet) in fleets.iter().enumerate() {
        for request in &requests {
            let actual = Request::parse(request).ok().and_then(|parsed| {
                Priority
                    .select(fleet, &parsed)
                    .name()
                    .map(ToOwned::to_owned)
            });

            if actual != expected[index] {
                mismatches.push(format!(
                    "fleet #{fleet_number} {request:?}: python={:?} rust={:?}",
                    expected[index], actual
                ));
            }

            index += 1;
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} decisions differ from the original Python:\n  {}",
        mismatches.len(),
        expected.len(),
        mismatches.join("\n  "),
    );
}

/// The corpus is only meaningful if it actually exercises the logic: a file
/// of all-nulls would pass the test above while proving nothing.
#[test]
fn the_corpus_exercises_real_decisions() {
    let expected = load_python_results();

    let selected = expected
        .iter()
        .filter(|decision| decision.is_some())
        .count();
    let rejected = expected.len() - selected;

    assert!(
        selected > 400,
        "only {selected} selections; corpus is too lenient"
    );
    assert!(
        rejected > 400,
        "only {rejected} rejections; corpus is too permissive"
    );

    let distinct: BTreeSet<&str> = expected
        .iter()
        .filter_map(|decision| decision.as_deref())
        .collect();

    assert!(
        distinct.len() > 20,
        "only {} distinct backends chosen; corpus lacks variety",
        distinct.len()
    );
}
