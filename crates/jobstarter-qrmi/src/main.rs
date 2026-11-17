// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! `jobstarter.qrmi` --- acquires a QPU and launches the job on it.
//!
//! Runs on the execution host, wrapping the user's command. It reconstructs
//! the request the `esub` forwarded, picks a backend, acquires it, records
//! the acquisition on the job, runs the job with a precisely-constructed
//! environment, releases the backend, and reports the job's exit status to
//! LSF.
//!
//! # The two paths
//!
//! * **explicit** --- `device=<name>` was given. The device is acquired
//!   directly; nothing is enumerated. The README is explicit that the name
//!   "is taken at face value and no checks for the device availability are
//!   made", so this path deliberately does not validate it.
//! * **selection** --- backends are enumerated through QRMI, their metrics
//!   collected concurrently, and a policy from `qpu-select` picks one.
//!
//! # What must hold on both paths
//!
//! * **An acquired resource is always released.** A QPU acquisition is a
//!   chargeable reservation; leaking one holds it against other jobs until
//!   the service times it out. The Python repeats a `try`/`finally` on each
//!   path; here [`Lease`] releases on drop, so the two cannot diverge.
//! * **The job's exit status reaches LSF faithfully**, including `128 + n`
//!   for a signal death --- the case commit `8a3d1ce` fixed.
//! * **The job's environment is built, not inherited.** QRMI injects a
//!   resource-scoped variable set for every backend it enumerates; only the
//!   selected device's set may reach the job.

mod config;
mod launch;

use std::process::ExitCode;

use lsfq_core::lsf::bpost::{self, AcquisitionMessage, JobReference};
use lsfq_core::qrmi::lease::Lease;
use lsfq_core::qrmi::provider;
use lsfq_core::qrmi::qrmi_crate::{models::ResourceType, QuantumResource};
use lsfq_core::{jobenv, Diagnostics, EnvMap, Outcome, Program};

use config::{Config, Plan};
use launch::Job;

/// Exit status for a jobstarter-level failure, when the job never ran.
///
/// Distinct from any status the job itself could return, so an operator can
/// tell "the wrapper failed" from "the job failed". 125 is the convention
/// `env` and `timeout` use for the same purpose.
const WRAPPER_ERROR: u8 = 125;

fn main() -> ExitCode {
    let diagnostics = Diagnostics::from_env(Program::Qrmi);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            diagnostics.error(&format!("cannot start async runtime: {error}"));
            return ExitCode::from(WRAPPER_ERROR);
        }
    };

    match runtime.block_on(run(&diagnostics)) {
        Ok(outcome) => {
            // The job ran: its status is the jobstarter's status.
            let status = outcome.to_exit_status();
            diagnostics.debug_with("Job exited with status", &status);

            // ExitCode carries a u8, which is what wait() reports anyway.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            ExitCode::from(status as u8)
        }
        Err(message) => {
            diagnostics.error(&message);
            ExitCode::from(WRAPPER_ERROR)
        }
    }
}

/// Acquire a backend, run the job, release the backend.
async fn run(diagnostics: &Diagnostics) -> Result<Outcome, String> {
    let job = Job::from_args();
    if job.is_empty() {
        return Err("no command given; usage: jobstarter.qrmi <command> [args...]".to_owned());
    }

    let config = Config::from_env().map_err(|error| error.to_string())?;
    diagnostics.debug_with("QPU resource type:", &config.qpu_type);

    // Recorded before any enumeration: QRMI's provider setenv()s a
    // resource-scoped variable set for every backend it inspects, and only
    // the selected device's set may reach the job. Taking the snapshot here
    // covers both paths uniformly, even though the explicit path enumerates
    // nothing and so has nothing to prune.
    let before_enumeration = jobenv::snapshot();

    let device = match &config.plan {
        Plan::Explicit { device } => {
            diagnostics.debug_with("Requested device:", device);
            device.clone()
        }
        Plan::Select { selector, request } => {
            select_device(diagnostics, &config, selector, request).await?
        }
    };

    // Re-key the provider settings to the selected device: QRMI resolves a
    // resource's configuration only through "<device>_<SETTING>".
    let scoped = jobenv::scope_provider_vars(&jobenv::snapshot(), &device);
    for (key, value) in &scoped {
        std::env::set_var(key, value);
    }

    let resource = build_resource(&device, &config.qpu_type)?;

    let lease = Lease::acquire(resource, device.clone())
        .await
        .map_err(|error| error.to_string())?;

    diagnostics.debug_with("Acquired", &device);

    // The token is how QRMI identifies the acquisition to the job.
    std::env::set_var(lease.token_var(), lease.token());

    post_acquisition(diagnostics, &device, &config.qpu_type, lease.token());

    // Drop the variables QRMI injected for backends that were not selected.
    let (job_env, report) =
        jobenv::build_job_env(&before_enumeration, &jobenv::snapshot(), &device);

    if !report.is_empty() {
        diagnostics.debug_with(
            &format!(
                "QRMI enumerated {} available device(s):",
                report.enumerated.len()
            ),
            &report.enumerated.join(", "),
        );
        diagnostics.debug_with(
            "Setting QRMI variables for the selected device only:",
            &device,
        );
        diagnostics.debug(&format!(
            "Pruned {} injected and restored {} overwritten QRMI variable(s) \
             for unselected backends",
            report.created.len(),
            report.overwritten.len(),
        ));
    }

    let outcome = launch_job(&job, &job_env);

    // Capture the name before the lease is consumed by release().
    let token_var = lease.token_var();

    // Release before returning, so a failure is reported rather than being
    // swallowed by the drop guard. The guard remains the backstop for the
    // error paths above.
    if let Err(error) = lease.release().await {
        // Non-fatal: the job has already run, and the service will time the
        // acquisition out. Losing the job's exit status over this would be
        // worse than the leak.
        diagnostics.warn(&error.to_string());
    }

    // Removed whether or not the release succeeded, matching the Python's
    // `finally` -- tests/test_jobstarter_exit_status.py asserts the variable
    // is gone even when release() raises. A stale token left in the
    // environment would otherwise be inherited by anything this process
    // goes on to spawn.
    std::env::remove_var(&token_var);

    outcome
}

/// Enumerate backends and apply the requested policy.
async fn select_device(
    diagnostics: &Diagnostics,
    config: &Config,
    selector: &str,
    request: &str,
) -> Result<String, String> {
    diagnostics.debug_with("Device selection:", &selector);
    diagnostics.debug_with("User request:", &request);

    // Only the IBM types publish rankable metrics; a Pasqal or IQM resource
    // must be named explicitly with device=.
    let provider = provider::build(
        &config.qpu_type,
        config
            .qrmi_vars
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    )
    .map_err(|error| error.to_string())?;

    let (candidates, skipped) = provider::collect_metrics(provider.as_ref(), None)
        .await
        .map_err(|error| error.to_string())?;

    for skip in &skipped {
        if let provider::Queried::Skipped { name, reason } = skip {
            diagnostics.warn(&format!(
                "skipping backend {}: {reason}",
                name.as_deref().unwrap_or("<unknown>"),
            ));
        }
    }

    if candidates.is_empty() {
        return Err("no status of quantum devices".to_owned());
    }

    if diagnostics.level().is_verbose() {
        for (name, metrics) in &candidates {
            diagnostics.trace(&format!("{name}: {metrics:?}"));
        }
    }

    let parsed = lsfq_core::qpu_select::Request::parse(request)
        .map_err(|error| format!("cannot parse {request:?}: {error}"))?;

    let policy = lsfq_core::qpu_select::selector_by_name(selector)
        .ok_or_else(|| format!("unknown selector: {selector}"))?;

    let selection = policy.select(&candidates, &parsed);

    for rejected in &selection.rejected {
        diagnostics.debug(&format!("Skipping {}: {}", rejected.name, rejected.reason));
    }

    let (name, metrics) = selection
        .chosen
        .ok_or_else(|| "no backend satisfies the requirements".to_owned())?;

    diagnostics.debug_with("Selected backend:", &name);
    if diagnostics.level().is_verbose() {
        diagnostics.trace(&format!("{metrics:?}"));
    }

    Ok(name)
}

/// Build the QRMI resource for a device.
///
/// Uses the acquisition type set, which is wider than the enumeration set:
/// acquiring a named device needs no metrics.
fn build_resource(
    device: &str,
    qpu_type: &str,
) -> Result<Box<dyn QuantumResource + Send + Sync>, String> {
    use lsfq_core::qrmi::qrmi_crate::{alice_bob, ibm, iqm, pasqal};

    let resource_type = provider::resource_type(qpu_type).map_err(|error| error.to_string())?;

    let built: Box<dyn QuantumResource + Send + Sync> = match resource_type {
        ResourceType::IBMQuantumSystem => Box::new(
            ibm::IBMQuantumSystem::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        ResourceType::IBMQuantumComputeService => Box::new(
            ibm::IBMQuantumComputeService::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        ResourceType::QiskitRuntimeService => Box::new(
            ibm::IBMQiskitRuntimeService::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        ResourceType::PasqalCloud => Box::new(
            pasqal::PasqalCloud::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        ResourceType::IQMServer => Box::new(
            iqm::IQMServer::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        ResourceType::AliceBobFelis => Box::new(
            alice_bob::AliceBobFelis::new(device)
                .map_err(|error| resource_error(device, &error.to_string()))?,
        ),
        // ResourceTypeName accepts only the six above, so this is
        // unreachable; it is written out rather than left as a wildcard so
        // that adding a variant upstream is a compile error here.
        other => {
            return Err(format!(
                "QRMI resource type {} is not supported by the jobstarter",
                other.as_str()
            ))
        }
    };

    Ok(built)
}

fn resource_error(device: &str, message: &str) -> String {
    format!("failed to create QRMI resource for {device}: {message}")
}

/// Record the acquisition on the LSF job, best-effort.
///
/// Operators read this back with `bread`, so a failure is worth a warning
/// but must never fail the job.
fn post_acquisition(diagnostics: &Diagnostics, device: &str, qpu_type: &str, token: &str) {
    let reference = match JobReference::from_env() {
        Ok(reference) => reference,
        Err(error) => {
            diagnostics.warn(&format!("Cannot post QRMI acquisition ID to LSF: {error}"));
            return;
        }
    };

    let message = AcquisitionMessage {
        qrmi_resource: device,
        qrmi_resource_type: qpu_type,
        qrmi_acquisition_id: token,
    };

    if let Err(error) = bpost::post(&reference, &message) {
        diagnostics.warn(&format!("Cannot post QRMI acquisition ID to LSF: {error}"));
    }
}

/// Run the job, mapping a spawn failure to an outcome LSF can record.
fn launch_job(job: &Job, environment: &EnvMap) -> Result<Outcome, String> {
    match launch::run(job, environment) {
        Ok(outcome) => Ok(outcome),
        Err(error) => {
            // The shell reports an unrunnable command as 126/127. Returning
            // an outcome rather than an error keeps the release path above
            // on the normal course.
            Err(format!("cannot launch job: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_error_cannot_be_confused_with_a_job_status() {
        // 125 is the `env`/`timeout` convention for "the wrapper failed".
        assert_eq!(WRAPPER_ERROR, 125);
    }

    /// The status handed to LSF must survive the u8 narrowing for every
    /// outcome a child can produce.
    #[test]
    fn every_child_status_narrows_faithfully() {
        for code in 0..=255 {
            let outcome = Outcome::Exited(code);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let narrowed = outcome.to_exit_status() as u8;
            assert_eq!(u32::from(narrowed), u32::try_from(code).unwrap());
        }

        // Signals: 128 + n, all within a u8 for n <= 127.
        for signal in 1..=64 {
            let outcome = Outcome::Signalled(signal);
            let status = outcome.to_exit_status();
            assert!(status <= 255, "signal {signal} gives {status}");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let narrowed = status as u8;
            assert_eq!(i32::from(narrowed), status);
        }
    }
}
