// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The esub/jobstarter round trip.
//!
//! The esub serialises a request into `LSF_SUB4_SUB_ENV_VARS`, LSF turns
//! that into the job's environment, and the jobstarter parses it back. Both
//! halves have unit tests; these check that what one writes, the other
//! reads -- a mismatch would admit the job and then fail it on the
//! execution host, which is the failure mode the phase ordering exists to
//! avoid.
//!
//! The directive is parsed here rather than invoking the real jobstarter,
//! which needs QRMI; the parsing it performs is `lsfq_core`'s and is
//! exercised directly.

use std::collections::BTreeMap;

use lsfq_core::lsf::esub::EnvVarDirective;
use lsfq_core::qpu_select::{selector_by_name, Request as QpuRequest};
use lsfq_core::{esub_vars, is_unset_device, Credentials};

/// Undo the directive the way LSF does: strip the wrapper, split on commas,
/// and take each `KEY=value`.
fn parse_directive(rendered: &str) -> BTreeMap<String, String> {
    let inner = rendered
        .strip_prefix("LSF_SUB4_SUB_ENV_VARS=\"")
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("directive is quoted");

    inner
        .split(',')
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let (key, value) = piece.split_once('=').expect("key=value");
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

/// Build the directive the esub would write for a given request.
fn directive_for(credentials: &str, selector: &str, qpu: &str) -> BTreeMap<String, String> {
    let credentials = Credentials::parse(credentials);

    let mut directive = EnvVarDirective::new();
    directive.extend(credentials.iter());
    directive.set(esub_vars::SELECTOR, selector);
    directive.set(esub_vars::QPU, qpu);

    parse_directive(&directive.render().expect("renders"))
}

const CREDENTIALS: &str = "QRMI_IBM_QCS_IAM_APIKEY=key\n\
                           QRMI_IBM_QCS_SERVICE_CRN=crn:v1:bluemix:public\n\
                           QRMI_IBM_QCS_ENDPOINT=https://quantum.cloud.ibm.com/api/v1\n\
                           QRMI_IBM_QCS_IAM_ENDPOINT=https://iam.cloud.ibm.com\n\
                           QRMI_JOB_QPU_TYPES=ibm-quantum-compute-service\n";

#[test]
fn credentials_survive_the_round_trip() {
    let environment = directive_for(CREDENTIALS, "basic", "qubits=128");

    assert_eq!(
        environment
            .get("QRMI_IBM_QCS_IAM_APIKEY")
            .map(String::as_str),
        Some("key")
    );
    assert_eq!(
        environment.get(esub_vars::QPU_TYPES).map(String::as_str),
        Some("ibm-quantum-compute-service")
    );
}

/// The jobstarter must be able to parse every request the esub can emit.
#[test]
fn the_request_string_is_parseable_by_the_selector() {
    for request in [
        "qubits=128",
        "qubits=120;processor_type=Nighthawk",
        "qubits=128;clops=240000;pending_jobs=100",
        "qubits=128;readout_error_median=0.01;T1_median_us=150;T2_median_us=120",
        "qpu_version=1.3.29;sx_error_median=0.0003;cz_error_median=0.002",
    ] {
        let environment = directive_for(CREDENTIALS, "priority", request);
        let forwarded = environment.get(esub_vars::QPU).expect("request forwarded");

        assert_eq!(forwarded, request, "the request must survive verbatim");

        QpuRequest::parse(forwarded)
            .unwrap_or_else(|error| panic!("jobstarter cannot parse {forwarded:?}: {error}"));
    }
}

/// Every selector the esub accepts must resolve in the jobstarter.
#[test]
fn every_forwarded_selector_resolves() {
    for name in lsfq_core::qpu_select::SELECTOR_NAMES {
        let environment = directive_for(CREDENTIALS, name, "qubits=1");
        let forwarded = environment
            .get(esub_vars::SELECTOR)
            .expect("selector forwarded");

        assert_eq!(forwarded, name);
        assert!(selector_by_name(forwarded).is_some(), "{name} must resolve");
    }
}

/// A CRN contains `=`, which must not be mistaken for a key separator.
#[test]
fn values_containing_equals_survive() {
    let environment = directive_for(
        "QRMI_IBM_QCS_SERVICE_CRN=crn:v1:bluemix:public:quantum:a=b=c\n\
         QRMI_JOB_QPU_TYPES=ibm-quantum-compute-service\n",
        "basic",
        "qubits=1",
    );

    assert_eq!(
        environment
            .get("QRMI_IBM_QCS_SERVICE_CRN")
            .map(String::as_str),
        Some("crn:v1:bluemix:public:quantum:a=b=c")
    );
}

/// On the explicit-device path the jobstarter must see the device, and the
/// `None` sentinel must not be mistaken for one.
#[test]
fn the_device_path_round_trips() {
    let mut directive = EnvVarDirective::new();
    directive.extend(Credentials::parse(CREDENTIALS).iter());
    directive.set(esub_vars::SELECTOR, "basic");
    directive.set(esub_vars::DEVICE, "ibm_marrakesh");

    let environment = parse_directive(&directive.render().expect("renders"));
    let device = environment
        .get(esub_vars::DEVICE)
        .expect("device forwarded");

    assert_eq!(device, "ibm_marrakesh");
    assert!(!is_unset_device(device));
}

/// The esub omits `ESUB_USER_REQ_DEVICE` entirely rather than writing
/// "None"; the jobstarter must treat an absent variable the same way.
#[test]
fn an_omitted_device_is_not_a_device() {
    let environment = directive_for(CREDENTIALS, "priority", "qubits=128");

    assert!(
        !environment.contains_key(esub_vars::DEVICE),
        "no device= means the variable is absent"
    );
    // And the sentinel the Python writes is also handled.
    assert!(is_unset_device("None"));
}

/// `file=` names a path on the submission host and must not be forwarded.
#[test]
fn the_credentials_path_is_not_forwarded() {
    let environment = directive_for(CREDENTIALS, "basic", "qubits=1");

    for key in environment.keys() {
        assert_ne!(key, "file");
        assert!(
            !key.starts_with("ESUB_USER_REQ_FILE"),
            "{key} leaks the path"
        );
    }
}

/// The directive must contain nothing that would break LSF's parsing.
#[test]
fn the_directive_is_well_formed() {
    let mut directive = EnvVarDirective::new();
    directive.extend(Credentials::parse(CREDENTIALS).iter());
    directive.set(esub_vars::SELECTOR, "priority");
    directive.set(esub_vars::QPU, "qubits=128;clops=240000");

    let rendered = directive.render().expect("renders");

    assert!(rendered.starts_with("LSF_SUB4_SUB_ENV_VARS=\""));
    assert!(rendered.ends_with('"'));
    assert!(
        !rendered.contains('\n'),
        "a newline would truncate the directive"
    );

    // Exactly two quotes: the opening and closing ones.
    assert_eq!(rendered.matches('"').count(), 2);
}
