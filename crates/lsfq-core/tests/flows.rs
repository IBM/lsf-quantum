// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! End-to-end flows across modules, through the public API only.
//!
//! The unit tests cover each module in isolation; these check that the pieces
//! compose the way the three binaries will use them, which is where a
//! mismatch between two correct modules would otherwise hide until the binaries run.

use lsfq_core::jobenv::{self, EnvMap};
use lsfq_core::lsf::{elim::LoadIndices, esub::EnvVarDirective};
use lsfq_core::qrmi::Target;
use lsfq_core::{esub_vars, is_unset_device, Credentials, Outcome};
use qpu_select::{selector_by_name, Candidates, Request};

/// A realistic `target()` payload, trimmed to two qubits and three gates.
const TARGET_PAYLOAD: &str = r#"{
  "configuration": {
    "n_qubits": 156,
    "backend_version": "1.3.29",
    "clops_h": 320000,
    "sample_name": "family: Heron, revision: 2"
  },
  "properties": {
    "qubits": [
      [
        {"name": "T1", "value": 140.0, "unit": "us"},
        {"name": "T2", "value": 100.0, "unit": "us"},
        {"name": "readout_error", "value": 0.009}
      ],
      [
        {"name": "T1", "value": 142.0, "unit": "us"},
        {"name": "T2", "value": 102.0, "unit": "us"},
        {"name": "readout_error", "value": 0.011}
      ]
    ],
    "gates": [
      {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.0002}]},
      {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.0004}]},
      {"gate": "cz", "parameters": [{"name": "gate_error", "value": 0.0026}]}
    ]
  }
}"#;

/// The esub flow: read a credentials file, add the user's request, and
/// serialise the lot into the directive LSF consumes.
#[test]
fn esub_builds_a_directive_from_credentials_and_request() {
    let credentials = Credentials::parse(
        "QRMI_IBM_QCS_IAM_APIKEY=secret\n\
         QRMI_IBM_QCS_SERVICE_CRN=crn:v1:bluemix:public:quantum:a=b\n\
         QRMI_IBM_QCS_ENDPOINT=https://quantum.cloud.ibm.com/api/v1\n\
         QRMI_IBM_QCS_IAM_ENDPOINT=https://iam.cloud.ibm.com\n",
    );
    credentials.validate_required().expect("complete");

    let mut directive = EnvVarDirective::new();
    directive.extend(credentials.iter());
    directive
        .set(esub_vars::SELECTOR, "priority")
        .set(esub_vars::QPU, "qubits=128;clops=200000")
        .set(esub_vars::DEVICE, "None");

    let rendered = directive.render().expect("renders");

    assert!(rendered.starts_with("LSF_SUB4_SUB_ENV_VARS=\""));
    assert!(rendered.ends_with('"'));
    assert!(rendered.contains("ESUB_USER_REQ_SELECTOR=priority,"));
    assert!(rendered.contains("ESUB_USER_REQ_QPU=qubits=128;clops=200000,"));
    // A CRN containing '=' must survive intact.
    assert!(rendered.contains("crn:v1:bluemix:public:quantum:a=b,"));
}

/// The jobstarter flow: parse the target, select, scope and prune the
/// environment, and translate the child's exit status.
#[test]
fn jobstarter_flow_from_metrics_to_environment() {
    // 1. Two backends' metrics, as the QRMI adapter would produce them.
    let busy = Target::parse(TARGET_PAYLOAD)
        .expect("parses")
        .to_metrics(Some(435), true);
    let idle = Target::parse(TARGET_PAYLOAD)
        .expect("parses")
        .to_metrics(Some(3), true);

    let mut candidates = Candidates::new();
    candidates.insert("ibm_fez".to_owned(), busy);
    candidates.insert("ibm_marrakesh".to_owned(), idle);

    // 2. Select, using the request the esub forwarded.
    let request = Request::parse("qubits=128;pending_jobs=1000").expect("parses");
    let selector = selector_by_name("priority").expect("known policy");
    let selection = selector.select(&candidates, &request);

    let device = selection.name().expect("something qualifies");
    assert_eq!(device, "ibm_marrakesh", "the shorter queue wins");

    // 3. Scope the provider settings to the selection.
    let mut environment = EnvMap::new();
    environment.insert("PATH".into(), "/usr/bin".into());
    environment.insert("QRMI_IBM_QCS_IAM_APIKEY".into(), "secret".into());
    environment.insert("QRMI_JOB_QPU_TYPES".into(), "qiskit-runtime-service".into());

    let before = environment.clone();
    let scoped = jobenv::scope_provider_vars(&environment, device);

    assert_eq!(
        scoped.get("ibm_marrakesh_QRMI_IBM_QCS_IAM_APIKEY"),
        Some(&"secret".to_owned())
    );
    assert_eq!(scoped.get(esub_vars::BEST_DEVICE), Some(&device.to_owned()));
    assert_eq!(
        scoped.get(esub_vars::QPU_RESOURCES),
        Some(&device.to_owned())
    );

    // 4. QRMI injected a set for the backend it did not select.
    let mut current = scoped;
    current.insert("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY".into(), "injected".into());

    let (job_env, report) = jobenv::build_job_env(&before, &current, device);

    assert!(job_env.contains_key("ibm_marrakesh_QRMI_IBM_QCS_IAM_APIKEY"));
    assert!(
        !job_env.contains_key("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"),
        "the unselected backend's variables must not reach the job"
    );
    assert_eq!(job_env.get("PATH"), Some(&"/usr/bin".to_owned()));
    assert_eq!(report.enumerated, vec!["ibm_fez", "ibm_marrakesh"]);

    // 5. Translate the child's fate for LSF.
    assert_eq!(Outcome::Exited(0).to_exit_status(), 0);
    assert_eq!(Outcome::Signalled(9).to_exit_status(), 137);
}

/// The explicit-device path: `device=` bypasses selection entirely.
#[test]
fn explicit_device_path_skips_selection() {
    assert!(
        is_unset_device("None"),
        "an unset request serialises as \"None\""
    );
    assert!(!is_unset_device("ibm_sherbrooke"));

    // Nothing is enumerated on this path, so there is nothing to prune.
    let before = EnvMap::new();
    let current = jobenv::scope_provider_vars(
        &[("QRMI_IBM_QCS_IAM_APIKEY".to_owned(), "k".to_owned())]
            .into_iter()
            .collect(),
        "ibm_sherbrooke",
    );

    let (job_env, report) = jobenv::build_job_env(&before, &current, "ibm_sherbrooke");

    assert!(report.is_empty(), "no enumeration means no pruning");
    assert!(job_env.contains_key("ibm_sherbrooke_QRMI_IBM_QCS_IAM_APIKEY"));
}

/// The ELIM flow: resolve credentials for the host's QPU, parse a target,
/// and render the load-index line.
#[test]
fn elim_flow_from_env_qpu_to_load_line() {
    // 1. The legacy env.qpu spelling plus endpoint defaults.
    let credentials = Credentials::parse("APIKEY=legacy_key\nCRN=legacy_crn\n");
    let resolved = credentials.resolve_for_device("ibm_fez").expect("resolves");

    assert_eq!(resolved.len(), 4);
    for (key, _) in resolved.iter() {
        assert!(key.starts_with("ibm_fez_"), "{key} must be device-scoped");
    }

    // 2. Parse the target and render the line.
    let target = Target::parse(TARGET_PAYLOAD).expect("parses");
    let medians = target.medians();
    let processor = target.processor_type().expect("present");

    let indices = LoadIndices {
        qubits: target.qubits(),
        clops: target.clops(),
        qpu_version: target.qpu_version(),
        processor_type: Some(lsfq_core::lsf::elim::format_processor_type(
            Some(&processor.family),
            processor.revision.as_deref(),
        )),
        pending_jobs: Some(59),
        readout_error_median: medians.readout_error_median,
        sx_error_median: medians.sx_error_median,
        cz_error_median: medians.cz_error_median,
        t1_median_us: medians.t1_median_us,
        t2_median_us: medians.t2_median_us,
    };

    let line = indices.render();

    // The shape the README records from a running ELIM.
    assert!(
        line.starts_with("10 qubits 156 clops 320000 qpu_version 1.3.29 "),
        "{line}"
    );
    assert!(line.contains("processor_type Heron_r2 "), "{line}");
    assert!(line.contains("pending_jobs 59 "), "{line}");
    assert!(line.contains("T1_median_µs 141.00 "), "{line}");
    assert!(line.contains("T2_median_µs 101.00 "), "{line}");

    // 21 fields: the count plus ten name/value pairs.
    assert_eq!(line.split_whitespace().count(), 21);
}

/// A transient fetch failure must still produce a well-formed line: an ELIM
/// that stops reporting takes every index for its host down with it.
#[test]
fn elim_emits_a_valid_line_when_everything_is_unavailable() {
    let line = LoadIndices::unavailable().render();

    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(fields.len(), 21);
    assert_eq!(fields[0], "10");

    // Every value is the "not available" marker, and none is empty.
    for position in 0..10 {
        assert_eq!(fields[2 + 2 * position], "-");
    }
}

/// Partial calibration data: the configuration is present but properties are
/// not, which is the case the ELIM's own comments call out.
#[test]
fn elim_handles_a_target_without_properties() {
    let target = Target::parse(r#"{"configuration": {"n_qubits": 156}}"#).expect("parses");
    let medians = target.medians();

    let indices = LoadIndices {
        qubits: target.qubits(),
        readout_error_median: medians.readout_error_median,
        t1_median_us: medians.t1_median_us,
        ..Default::default()
    };

    let line = indices.render();
    assert_eq!(line.split_whitespace().count(), 21);
    assert!(line.contains("qubits 156 "));
    assert!(line.contains("T1_median_µs - "));
}

/// The metrics the ELIM reports and the metrics the selectors consume come
/// from the same target document, so they must agree.
#[test]
fn elim_and_selector_metrics_agree() {
    let target = Target::parse(TARGET_PAYLOAD).expect("parses");

    let medians = target.medians();
    let metrics = target.to_metrics(Some(59), true);

    assert_eq!(metrics.t1_median_us, medians.t1_median_us);
    assert_eq!(metrics.t2_median_us, medians.t2_median_us);
    assert_eq!(metrics.readout_error_median, medians.readout_error_median);
    assert_eq!(metrics.sx_error_median, medians.sx_error_median);
    assert_eq!(metrics.cz_error_median, medians.cz_error_median);
    assert_eq!(metrics.qubits, target.qubits());
    assert_eq!(metrics.clops, target.clops());
}

/// The health policy needs T1 and the readout error; a target carrying both
/// must therefore be scoreable. This guards against the upstream defect
/// where missing calibration data silently zeroed every score.
#[test]
fn health_policy_can_score_a_real_target() {
    let metrics = Target::parse(TARGET_PAYLOAD)
        .expect("parses")
        .to_metrics(Some(3), true);

    let mut candidates = Candidates::new();
    candidates.insert("ibm_marrakesh".to_owned(), metrics);

    let selector = selector_by_name("health").expect("known policy");
    let selection = selector.select(&candidates, &Request::parse("qubits=128").unwrap());

    assert_eq!(selection.name(), Some("ibm_marrakesh"));
    assert!(
        selection.rejected.is_empty(),
        "a complete target must be scoreable"
    );
}

/// Every policy the esub accepts must be resolvable by the jobstarter.
#[test]
fn every_accepted_selector_resolves() {
    for name in qpu_select::SELECTOR_NAMES {
        assert!(
            selector_by_name(name).is_some(),
            "{name} is advertised but unresolvable"
        );
    }
}
