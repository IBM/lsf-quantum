// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Constructing the environment a job is launched with.
//!
//! # Why this module is small
//!
//! This is the module the proposal singles out. In the Python implementation
//! the same job is done by `read_process_environ()`, which loads `libc`
//! through `ctypes` and reads the `environ` symbol directly, plus
//! `snapshot_backend_env()` and `job_environment()` built on top of it --
//! roughly 120 lines whose docstrings are mostly an explanation of why they
//! have to exist.
//!
//! The cause is a language boundary. QRMI's `ResourceProvider` calls
//! `env::set_var()` from Rust for every backend it enumerates, writing
//! `<backend>_<KEY>` because `IBMQuantumComputeService::new()` resolves its
//! configuration only through that resource-scoped name. That goes through
//! libc `setenv()`, mutating the C-level environment block that `execve()`
//! hands to children. Python, however, populated `os.environ` once at
//! interpreter start and never re-reads it: the injected names are invisible
//! to `os.environ` while still being inherited by every subprocess, and
//! deleting them from `os.environ` is a no-op. The `ctypes` read exists to
//! see past Python's own abstraction.
//!
//! In Rust there is no such gap. `std::env::vars()` reads the same block
//! QRMI wrote to. More to the point, the job's environment does not need to
//! be *repaired* at all: it is built as an explicit map and handed to
//! `Command::env_clear().envs(..)`, so what the job receives is a value this
//! code computed rather than ambient state it inherited.
//!
//! What remains is the genuine policy question, which is not about Python:
//! after enumerating N backends, the process holds N sets of resource-scoped
//! variables, and only the selected backend's set should reach the job.

use std::collections::BTreeMap;
use std::env;

/// A snapshot of the environment.
///
/// `BTreeMap` for deterministic iteration, so a rendered environment is
/// reproducible and diffable in tests and in debug output.
pub type EnvMap = BTreeMap<String, String>;

/// Capture the environment as it stands now.
///
/// Values are captured, not just names, because QRMI's injection overwrites
/// an existing `<backend>_<KEY>` as readily as it creates one: a site-set
/// value for an unselected device would otherwise be replaced by the
/// provider's own, which is harder to notice than an outright removal.
#[must_use]
pub fn snapshot() -> EnvMap {
    env::vars().collect()
}

/// What pruning changed, for diagnostics.
///
/// Returned rather than logged so this module stays free of I/O and the
/// caller can report it at whatever debug level it chooses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Variables QRMI created for unselected backends, which were dropped.
    pub created: Vec<String>,
    /// Variables QRMI overwrote, which were restored to their prior values.
    pub overwritten: Vec<String>,
    /// The backend names QRMI appears to have enumerated.
    ///
    /// Recovered from the pruned variable names, which are the only record
    /// of which backends were considered -- pruning destroys it otherwise.
    pub enumerated: Vec<String>,
}

impl PruneReport {
    /// Whether anything was changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.overwritten.is_empty()
    }
}

/// Build the environment for a job, keeping only the selected device's
/// resource-scoped QRMI variables.
///
/// `before` is a [`snapshot`] taken before backend enumeration; `current` is
/// the environment now, including anything the caller set itself. Every
/// resource-scoped variable belonging to another backend is removed if QRMI
/// created it, or restored to its earlier value if QRMI overwrote it.
///
/// Unprefixed `QRMI_*` names are left alone: those are the provider
/// configuration the caller manages deliberately, not per-backend
/// injections.
///
/// Returns the environment to launch with, and a [`PruneReport`] describing
/// what changed.
///
/// # Example
///
/// ```
/// use lsfq_core::jobenv::{self, EnvMap};
///
/// let mut before = EnvMap::new();
/// before.insert("PATH".into(), "/usr/bin".into());
///
/// // QRMI enumerated two backends and injected a set for each.
/// let mut current = before.clone();
/// current.insert("ibm_fez_QRMI_IBM_QCS_ENDPOINT".into(), "https://a".into());
/// current.insert("ibm_torino_QRMI_IBM_QCS_ENDPOINT".into(), "https://b".into());
///
/// let (environment, report) = jobenv::build_job_env(&before, &current, "ibm_fez");
///
/// // Only the selected device's variables survive.
/// assert!(environment.contains_key("ibm_fez_QRMI_IBM_QCS_ENDPOINT"));
/// assert!(!environment.contains_key("ibm_torino_QRMI_IBM_QCS_ENDPOINT"));
/// assert_eq!(report.created, vec!["ibm_torino_QRMI_IBM_QCS_ENDPOINT"]);
/// assert_eq!(report.enumerated, vec!["ibm_fez", "ibm_torino"]);
/// ```
#[must_use]
pub fn build_job_env(before: &EnvMap, current: &EnvMap, device: &str) -> (EnvMap, PruneReport) {
    let mut environment = current.clone();
    let mut report = PruneReport::default();

    let device_prefix = format!("{device}_");

    for (name, value) in current {
        if !is_foreign_scoped(name, &device_prefix) {
            continue;
        }

        match before.get(name) {
            None => report.created.push(name.clone()),
            Some(previous) if previous != value => report.overwritten.push(name.clone()),
            Some(_) => {
                // Unchanged from before enumeration: not QRMI's doing, so
                // leave it as the site configured it.
            }
        }
    }

    if report.is_empty() {
        return (environment, report);
    }

    for name in &report.created {
        environment.remove(name);
    }
    for name in &report.overwritten {
        if let Some(previous) = before.get(name) {
            environment.insert(name.clone(), previous.clone());
        }
    }

    // Recover the backend names from the variables being pruned, and include
    // the selected device, which by definition was enumerated too.
    let mut enumerated: Vec<String> = report
        .created
        .iter()
        .chain(report.overwritten.iter())
        .filter_map(|name| {
            name.split_once("_QRMI")
                .map(|(backend, _)| backend.to_owned())
        })
        .collect();
    enumerated.push(device.to_owned());
    enumerated.sort_unstable();
    enumerated.dedup();
    report.enumerated = enumerated;

    (environment, report)
}

/// Whether a name is a resource-scoped QRMI variable for some *other*
/// backend.
///
/// Mirrors the Python `is_foreign()` exactly: the name must mention `QRMI`,
/// must not be an unprefixed `QRMI_*` provider setting, and must not belong
/// to the selected device.
fn is_foreign_scoped(name: &str, device_prefix: &str) -> bool {
    if !name.contains("QRMI") || name.starts_with("QRMI_") {
        return false;
    }

    !name.starts_with(device_prefix)
}

/// Rewrite the unprefixed provider settings into the selected device's
/// resource-scoped names.
///
/// QRMI resolves a resource's configuration only through
/// `<resource_id>_<SETTING>`, so the provider settings have to be re-keyed
/// before the job (and `acquire`/`release`) can use them. Equivalent to the
/// Python `build_qrmi_vars_job()`.
///
/// `QRMI_JOB_QPU_RESOURCES` and `QRMI_JOB_QPU_TYPES` are job-level rather
/// than resource-level, so they are not re-keyed; the first is instead set to
/// the selected device.
#[must_use]
pub fn scope_provider_vars(environment: &EnvMap, device: &str) -> EnvMap {
    /// Job-level names that must not be given a device prefix.
    const JOB_LEVEL: [&str; 2] = ["QRMI_JOB_QPU_RESOURCES", "QRMI_JOB_QPU_TYPES"];

    let mut result = environment.clone();

    for (name, value) in environment {
        if !name.starts_with("QRMI_") || JOB_LEVEL.contains(&name.as_str()) {
            continue;
        }

        result.insert(format!("{device}_{name}"), value.clone());
        result.remove(name);
    }

    result.insert("QRMI_IBM_QCS_BEST_DEVICE".to_owned(), device.to_owned());
    result.insert("QRMI_JOB_QPU_RESOURCES".to_owned(), device.to_owned());

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> EnvMap {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn drops_variables_qrmi_created_for_other_backends() {
        let before = map(&[("PATH", "/usr/bin")]);
        let current = map(&[
            ("PATH", "/usr/bin"),
            ("ibm_fez_QRMI_IBM_QCS_ENDPOINT", "https://a"),
            ("ibm_torino_QRMI_IBM_QCS_ENDPOINT", "https://b"),
            ("ibm_torino_QRMI_IBM_QCS_IAM_APIKEY", "k"),
        ]);

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert!(environment.contains_key("ibm_fez_QRMI_IBM_QCS_ENDPOINT"));
        assert!(!environment.contains_key("ibm_torino_QRMI_IBM_QCS_ENDPOINT"));
        assert!(!environment.contains_key("ibm_torino_QRMI_IBM_QCS_IAM_APIKEY"));
        assert_eq!(environment.get("PATH"), Some(&"/usr/bin".to_owned()));
        assert_eq!(report.created.len(), 2);
        assert!(report.overwritten.is_empty());
    }

    /// The subtle case the Python docstring calls out: a site-set value that
    /// QRMI overwrote must be put *back*, not dropped.
    #[test]
    fn restores_values_qrmi_overwrote() {
        let before = map(&[("ibm_torino_QRMI_IBM_QCS_ENDPOINT", "https://site-set")]);
        let current = map(&[("ibm_torino_QRMI_IBM_QCS_ENDPOINT", "https://qrmi-wrote")]);

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert_eq!(
            environment.get("ibm_torino_QRMI_IBM_QCS_ENDPOINT"),
            Some(&"https://site-set".to_owned()),
            "an overwritten value is restored, not removed"
        );
        assert_eq!(report.overwritten, vec!["ibm_torino_QRMI_IBM_QCS_ENDPOINT"]);
        assert!(report.created.is_empty());
    }

    #[test]
    fn leaves_unchanged_foreign_variables_alone() {
        let before = map(&[("ibm_torino_QRMI_X", "same")]);
        let current = map(&[("ibm_torino_QRMI_X", "same")]);

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert_eq!(
            environment.get("ibm_torino_QRMI_X"),
            Some(&"same".to_owned())
        );
        assert!(report.is_empty());
    }

    /// Unprefixed QRMI_* names are provider configuration, not injections.
    #[test]
    fn unprefixed_qrmi_vars_are_never_pruned() {
        let before = map(&[]);
        let current = map(&[
            ("QRMI_IBM_QCS_ENDPOINT", "https://a"),
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
        ]);

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert_eq!(environment.len(), 2);
        assert!(report.is_empty());
    }

    #[test]
    fn non_qrmi_variables_are_untouched() {
        let before = map(&[]);
        let current = map(&[("HOME", "/root"), ("ibm_torino_OTHER", "x")]);

        let (environment, _) = build_job_env(&before, &current, "ibm_fez");

        assert_eq!(environment.get("HOME"), Some(&"/root".to_owned()));
        // No "QRMI" in the name, so not a resource-scoped injection.
        assert_eq!(environment.get("ibm_torino_OTHER"), Some(&"x".to_owned()));
    }

    #[test]
    fn selected_device_variables_always_survive() {
        let before = map(&[]);
        let current = map(&[
            ("ibm_fez_QRMI_IBM_QCS_ENDPOINT", "https://a"),
            ("ibm_fez_QRMI_JOB_ACQUISITION_TOKEN", "token"),
        ]);

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert_eq!(environment.len(), 2, "acquire/release depend on these");
        assert!(report.is_empty());
    }

    #[test]
    fn enumeration_is_recovered_for_diagnostics() {
        let before = map(&[]);
        let current = map(&[
            ("ibm_aaa_QRMI_IBM_QCS_ENDPOINT", "a"),
            ("ibm_zzz_QRMI_IBM_QCS_ENDPOINT", "z"),
            ("ibm_zzz_QRMI_IBM_QCS_IAM_APIKEY", "k"),
        ]);

        let (_, report) = build_job_env(&before, &current, "ibm_fez");

        // Deduplicated, sorted, and including the selected device.
        assert_eq!(report.enumerated, vec!["ibm_aaa", "ibm_fez", "ibm_zzz"]);
    }

    #[test]
    fn empty_environments_are_handled() {
        let (environment, report) = build_job_env(&EnvMap::new(), &EnvMap::new(), "ibm_fez");
        assert!(environment.is_empty());
        assert!(report.is_empty());
    }

    // ---- scope_provider_vars ----

    #[test]
    fn provider_vars_are_rekeyed_to_the_device() {
        let environment = map(&[
            ("QRMI_IBM_QCS_IAM_APIKEY", "key"),
            ("QRMI_IBM_QCS_SERVICE_CRN", "crn"),
            ("PATH", "/usr/bin"),
        ]);

        let scoped = scope_provider_vars(&environment, "ibm_fez");

        assert_eq!(
            scoped.get("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"),
            Some(&"key".to_owned())
        );
        assert_eq!(
            scoped.get("ibm_fez_QRMI_IBM_QCS_SERVICE_CRN"),
            Some(&"crn".to_owned())
        );

        // The unprefixed originals are removed: QRMI never reads them.
        assert!(!scoped.contains_key("QRMI_IBM_QCS_IAM_APIKEY"));
        assert!(!scoped.contains_key("QRMI_IBM_QCS_SERVICE_CRN"));

        assert_eq!(scoped.get("PATH"), Some(&"/usr/bin".to_owned()));
    }

    #[test]
    fn job_level_vars_are_not_rekeyed() {
        let environment = map(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("QRMI_JOB_QPU_RESOURCES", "stale"),
        ]);

        let scoped = scope_provider_vars(&environment, "ibm_fez");

        assert_eq!(
            scoped.get("QRMI_JOB_QPU_TYPES"),
            Some(&"qiskit-runtime-service".to_owned()),
            "the type is job-level, not resource-scoped"
        );
        assert!(!scoped.contains_key("ibm_fez_QRMI_JOB_QPU_TYPES"));

        // The resource list is overwritten with the selection.
        assert_eq!(
            scoped.get("QRMI_JOB_QPU_RESOURCES"),
            Some(&"ibm_fez".to_owned())
        );
    }

    #[test]
    fn best_device_is_advertised() {
        let scoped = scope_provider_vars(&EnvMap::new(), "ibm_marrakesh");
        assert_eq!(
            scoped.get("QRMI_IBM_QCS_BEST_DEVICE"),
            Some(&"ibm_marrakesh".to_owned())
        );
        assert_eq!(
            scoped.get("QRMI_JOB_QPU_RESOURCES"),
            Some(&"ibm_marrakesh".to_owned())
        );
    }

    /// Scoping then pruning must leave exactly the selected device's set.
    #[test]
    fn scope_then_prune_composes() {
        let before = map(&[]);

        let mut current = map(&[
            ("QRMI_IBM_QCS_IAM_APIKEY", "key"),
            ("ibm_other_QRMI_IBM_QCS_IAM_APIKEY", "foreign"),
        ]);
        current = scope_provider_vars(&current, "ibm_fez");

        let (environment, report) = build_job_env(&before, &current, "ibm_fez");

        assert!(environment.contains_key("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"));
        assert!(!environment.contains_key("ibm_other_QRMI_IBM_QCS_IAM_APIKEY"));
        assert_eq!(report.created, vec!["ibm_other_QRMI_IBM_QCS_IAM_APIKEY"]);
    }

    #[test]
    fn snapshot_reads_the_real_environment() {
        // The point of the module: no ctypes, no stale copy.
        let captured = snapshot();
        if let Ok(path) = env::var("PATH") {
            assert_eq!(captured.get("PATH"), Some(&path));
        }
    }
}
