// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Integration tests over a realistic fleet.
//!
//! The fixture below is modelled on the values that appear in `README.md`
//! and in the ELIM output recorded there, so the policies are exercised
//! against the shape of data a real cluster sees rather than against
//! hand-tuned numbers. These tests use only the crate's public API, which
//! also keeps that API honest.

use qpu_select::{
    selector_by_name, Basic, Candidates, Health, Metric, Priority, ProcessorType, QpuMetrics,
    Request, Selection, Selector,
};

/// A six-QPU fleet, the same set named in the README's health-selector
/// example, with plausible calibration data attached.
fn fleet() -> Candidates {
    let mut fleet = Candidates::new();

    let mut add = |name: &str,
                   qubits: u32,
                   clops: f64,
                   queue: u64,
                   t1: f64,
                   readout: f64,
                   family: &str,
                   revision: &str| {
        fleet.insert(
            name.to_owned(),
            QpuMetrics::builder()
                .qubits(qubits)
                .clops(clops)
                .pending_jobs(queue)
                .t1_median_us(t1)
                .readout_error_median(readout)
                .sx_error_median(0.000_272_840_430_114_404_4)
                .cz_error_median(0.002_648_294_774_599_657_7)
                .t2_median_us(101.06)
                .processor_type(ProcessorType::new(family).with_revision(revision))
                .qpu_version("1.3.29")
                .available(true)
                .build(),
        );
    };

    // name, qubits, clops, queue, T1 us, readout err, family, revision
    add(
        "ibm_boston",
        156,
        250_000.0,
        124,
        150.0,
        0.0094,
        "Heron",
        "2",
    );
    add("ibm_fez", 156, 320_000.0, 435, 141.29, 0.0094, "Heron", "2");
    add(
        "ibm_kingston",
        156,
        240_000.0,
        475,
        120.0,
        0.0150,
        "Heron",
        "2",
    );
    add(
        "ibm_marrakesh",
        156,
        195_000.0,
        3,
        130.0,
        0.0120,
        "Heron",
        "2",
    );
    add(
        "ibm_miami",
        120,
        400_000.0,
        1,
        200.0,
        0.0050,
        "Nighthawk",
        "1",
    );
    add(
        "ibm_pittsburgh",
        156,
        260_000.0,
        679,
        160.0,
        0.0080,
        "Heron",
        "2",
    );

    fleet
}

fn select(policy: &str, request: &str) -> Selection {
    let selector = selector_by_name(policy).expect("known policy");
    let request = Request::parse(request).expect("request parses");
    selector.select(&fleet(), &request)
}

#[test]
fn basic_picks_the_shortest_queue_meeting_the_floor() {
    // ibm_miami has the shortest queue (1) but only 120 qubits, so the
    // 128-qubit floor excludes it and ibm_marrakesh (queue 3) wins.
    let selection = select("basic", "qubits=128");
    assert_eq!(selection.name(), Some("ibm_marrakesh"));
}

#[test]
fn basic_uses_the_idle_small_machine_when_the_floor_allows() {
    let selection = select("basic", "qubits=64");
    assert_eq!(selection.name(), Some("ibm_miami"));
}

#[test]
fn health_balances_coherence_error_and_queue() {
    // Among the >=128-qubit machines, ibm_marrakesh's queue of 3 dominates:
    // the others are 124 jobs deep or worse.
    let selection = select("health", "qubits=128");
    assert_eq!(selection.name(), Some("ibm_marrakesh"));

    // Sanity-check the ordering the score implies.
    let fleet = fleet();
    let marrakesh = Health::score_of(&fleet, "ibm_marrakesh").expect("scoreable");
    let pittsburgh = Health::score_of(&fleet, "ibm_pittsburgh").expect("scoreable");
    assert!(
        marrakesh > pittsburgh,
        "a short queue must beat better coherence behind 679 jobs"
    );
}

#[test]
fn health_prefers_the_pristine_machine_when_it_qualifies() {
    // ibm_miami: best T1, lowest readout error, queue of 1.
    let selection = select("health", "qubits=64");
    assert_eq!(selection.name(), Some("ibm_miami"));
}

#[test]
fn priority_ranks_on_the_first_requirement() {
    // CLOPS first, so the fastest machine meeting the floor wins.
    let selection = select("priority", "qubits=128;clops=100000");
    assert_eq!(selection.name(), Some("ibm_fez"));
}

#[test]
fn priority_honours_requirement_order() {
    // Same two constraints, opposite order: now the queue is primary.
    let selection = select("priority", "pending_jobs=1000;qubits=128");
    assert_eq!(selection.name(), Some("ibm_marrakesh"));
}

#[test]
fn priority_filters_on_processor_family() {
    let selection = select("priority", "processor_type=Nighthawk");
    assert_eq!(selection.name(), Some("ibm_miami"));
}

#[test]
fn priority_reports_nothing_when_over_constrained() {
    let selection = select("priority", "qubits=1000");
    assert_eq!(selection.chosen, None);
    assert_eq!(selection.rejected.len(), 6, "every backend is rejected");
}

#[test]
fn priority_readme_pending_jobs_example() {
    // README: "Select QPU with less than 100 pending jobs in queue".
    let selection = select("priority", "pending_jobs<100");
    let name = selection.name().expect("something qualifies");
    assert!(
        matches!(name, "ibm_marrakesh" | "ibm_miami"),
        "expected a short-queue machine, got {name}"
    );
}

#[test]
fn priority_readme_readout_error_example() {
    // README: "Select QPU with the lowest median readout error".
    let selection = select("priority", "readout_error_median=1");
    assert_eq!(selection.name(), Some("ibm_miami"));
}

/// Every policy must agree that an unavailable backend is never chosen.
#[test]
fn no_policy_selects_an_unavailable_backend() {
    let mut fleet = fleet();
    for metrics in fleet.values_mut() {
        metrics.available = false;
    }

    let request = Request::parse("qubits=1").expect("parses");
    for name in qpu_select::SELECTOR_NAMES {
        let selector = selector_by_name(name).expect("known policy");
        let selection = selector.select(&fleet, &request);
        assert_eq!(selection.chosen, None, "{name} chose an offline backend");
        assert_eq!(selection.rejected.len(), 6, "{name} should reject all six");
    }
}

/// Selection must not depend on insertion order or on a hash seed.
#[test]
fn selection_is_deterministic_across_runs() {
    let request = Request::parse("qubits=128;clops=100000").expect("parses");

    for name in qpu_select::SELECTOR_NAMES {
        let selector = selector_by_name(name).expect("known policy");
        let first = selector.select(&fleet(), &request);

        for _ in 0..32 {
            let again = selector.select(&fleet(), &request);
            assert_eq!(first.name(), again.name(), "{name} is not deterministic");
        }
    }
}

/// An empty fleet is a normal outcome (no QPUs mapped yet), not a panic.
#[test]
fn every_policy_handles_an_empty_fleet() {
    let empty = Candidates::new();
    let request = Request::parse("qubits=128").expect("parses");

    for name in qpu_select::SELECTOR_NAMES {
        let selector = selector_by_name(name).expect("known policy");
        let selection = selector.select(&empty, &request);
        assert_eq!(selection.chosen, None);
        assert!(selection.rejected.is_empty());
    }
}

/// A fleet reporting no calibration data at all: `basic` still works,
/// because it ranks on queue depth, while `health` cannot score anything.
/// This is the situation the README's worked example actually shows.
#[test]
fn fleet_without_calibration_data() {
    let mut fleet = Candidates::new();
    fleet.insert(
        "ibm_a".to_owned(),
        QpuMetrics::builder()
            .qubits(156)
            .pending_jobs(10)
            .available(true)
            .build(),
    );
    fleet.insert(
        "ibm_b".to_owned(),
        QpuMetrics::builder()
            .qubits(156)
            .pending_jobs(2)
            .available(true)
            .build(),
    );

    let request = Request::parse("qubits=128").expect("parses");

    let basic = Basic.select(&fleet, &request);
    assert_eq!(
        basic.name(),
        Some("ibm_b"),
        "basic ranks on queue depth alone"
    );

    let health = Health.select(&fleet, &request);
    assert_eq!(
        health.chosen, None,
        "health must refuse rather than score every backend zero"
    );
    assert_eq!(health.rejected.len(), 2);

    let priority = Priority.select(&fleet, &request);
    assert_eq!(
        priority.name(),
        Some("ibm_a"),
        "qubits tie, so name breaks it"
    );
}

#[test]
fn metric_names_match_the_external_contract() {
    // These spellings appear in bsub arguments and ESUB_USER_REQ_QPU, so a
    // change here is a breaking change for users.
    assert_eq!(Metric::Qubits.as_str(), "qubits");
    assert_eq!(Metric::PendingJobs.as_str(), "pending_jobs");
    assert_eq!(Metric::ReadoutErrorMedian.as_str(), "readout_error_median");
    assert_eq!(Metric::SxErrorMedian.as_str(), "sx_error_median");
    assert_eq!(Metric::CzErrorMedian.as_str(), "cz_error_median");
    assert_eq!(Metric::T1MedianUs.as_str(), "T1_median_us");
    assert_eq!(Metric::T2MedianUs.as_str(), "T2_median_us");
    assert_eq!(Metric::ProcessorType.as_str(), "processor_type");
    assert_eq!(Metric::QpuVersion.as_str(), "qpu_version");
    assert_eq!(Metric::Clops.as_str(), "clops");
}
