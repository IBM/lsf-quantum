// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Reconstructing the request from the environment.
//!
//! The `esub` serialises the user's request and the QRMI credentials into
//! `LSF_SUB4_SUB_ENV_VARS`; LSF injects them into the job's environment; the
//! jobstarter reads them back here. That round trip is the contract between
//! the two programs, so the variable names come from
//! [`lsfq_core::esub_vars`] rather than being written as literals.

use std::fmt;

use lsfq_core::{esub_vars, is_unset_device, EnvMap};

/// Which of the two paths the jobstarter takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// `device=<name>` was given: acquire it directly.
    ///
    /// Nothing is enumerated and no metrics are fetched. The README is
    /// explicit that the name "is taken at face value and no checks for the
    /// device availability are made" -- so this path must not start
    /// second-guessing the request by validating it.
    Explicit {
        /// The requested device.
        device: String,
    },
    /// No device was given: enumerate, then select.
    Select {
        /// The selection policy name.
        selector: String,
        /// The QPU attribute request string.
        request: String,
    },
}

/// The jobstarter's inputs.
#[derive(Debug, Clone)]
pub struct Config {
    /// The QRMI resource type for this job.
    pub qpu_type: String,
    /// Which path to take.
    pub plan: Plan,
    /// Every `QRMI_*` variable, for building the provider and the job env.
    pub qrmi_vars: EnvMap,
}

impl Config {
    /// Read the configuration from an environment map.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if the resource type is absent, or if a
    /// selection was asked for without the inputs it needs.
    pub fn from_env_map(environment: &EnvMap) -> Result<Self, ConfigError> {
        let qpu_type = environment
            .get(esub_vars::QPU_TYPES)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::NoQpuType)?;

        // Preserved generically, so the jobstarter does not assume a
        // particular backend namespace: a Pasqal or IQM resource carries a
        // different QRMI_* set and must pass through untouched.
        let qrmi_vars: EnvMap = environment
            .iter()
            .filter(|(key, _)| key.starts_with("QRMI_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        let device = environment
            .get(esub_vars::DEVICE)
            .map(String::as_str)
            .filter(|value| !is_unset_device(value))
            .map(|value| value.trim().to_owned());

        let plan = if let Some(device) = device {
            Plan::Explicit { device }
        } else {
            {
                let selector = environment
                    .get(esub_vars::SELECTOR)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    // The esub defaults this, so an absent value means the
                    // esub did not run -- but defaulting here too keeps a
                    // hand-built environment working, as the Python does.
                    .unwrap_or_else(|| "basic".to_owned());

                if lsfq_core::qpu_select::selector_by_name(&selector).is_none() {
                    return Err(ConfigError::UnknownSelector { name: selector });
                }

                let request = environment
                    .get(esub_vars::QPU)
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .ok_or(ConfigError::NoRequest)?;

                Plan::Select { selector, request }
            }
        };

        Ok(Self {
            qpu_type,
            plan,
            qrmi_vars,
        })
    }

    /// Read the configuration from this process's environment.
    ///
    /// # Errors
    ///
    /// As [`Config::from_env_map`].
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_map(&lsfq_core::jobenv::snapshot())
    }
}

/// The jobstarter's inputs were incomplete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `QRMI_JOB_QPU_TYPES` was absent or empty.
    NoQpuType,
    /// A selection was requested with no attribute request.
    NoRequest,
    /// The named selection policy does not exist.
    UnknownSelector {
        /// The name that was given.
        name: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoQpuType => write!(f, "no {} provided", esub_vars::QPU_TYPES),
            Self::NoRequest => write!(
                f,
                "no qubits requirement was specified: {} is empty and no \
                 device= was given",
                esub_vars::QPU
            ),
            Self::UnknownSelector { name } => {
                write!(f, "unknown selector: {name}; expected one of: ")?;
                let mut names = lsfq_core::qpu_select::SELECTOR_NAMES.iter();
                if let Some(first) = names.next() {
                    write!(f, "{first}")?;
                }
                for name in names {
                    write!(f, ", {name}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(entries: &[(&str, &str)]) -> EnvMap {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn reads_the_selection_path() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("ESUB_USER_REQ_SELECTOR", "priority"),
            ("ESUB_USER_REQ_QPU", "qubits=128"),
            ("ESUB_USER_REQ_DEVICE", "None"),
        ]))
        .expect("valid");

        assert_eq!(config.qpu_type, "qiskit-runtime-service");
        assert_eq!(
            config.plan,
            Plan::Select {
                selector: "priority".to_owned(),
                request: "qubits=128".to_owned(),
            }
        );
    }

    /// The esub serialises an unset device as the literal "None".
    #[test]
    fn the_none_sentinel_selects_the_selection_path() {
        for sentinel in ["None", "none", "null", "", "  "] {
            let config = Config::from_env_map(&env(&[
                ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
                ("ESUB_USER_REQ_QPU", "qubits=1"),
                ("ESUB_USER_REQ_DEVICE", sentinel),
            ]))
            .expect("valid");

            assert!(
                matches!(config.plan, Plan::Select { .. }),
                "{sentinel:?} should not be treated as a device"
            );
        }
    }

    #[test]
    fn reads_the_explicit_device_path() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("ESUB_USER_REQ_DEVICE", "ibm_sherbrooke"),
            // Present but irrelevant on this path.
            ("ESUB_USER_REQ_QPU", "qubits=128"),
            ("ESUB_USER_REQ_SELECTOR", "priority"),
        ]))
        .expect("valid");

        assert_eq!(
            config.plan,
            Plan::Explicit {
                device: "ibm_sherbrooke".to_owned()
            }
        );
    }

    /// An explicit device needs no request: the README says the name is taken
    /// at face value.
    #[test]
    fn explicit_device_needs_no_request() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "pasqal-cloud"),
            ("ESUB_USER_REQ_DEVICE", "some_device"),
        ]))
        .expect("valid");

        assert!(matches!(config.plan, Plan::Explicit { .. }));
    }

    #[test]
    fn qrmi_vars_are_collected_generically() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("QRMI_IBM_QCS_IAM_APIKEY", "key"),
            ("QRMI_PASQAL_SOMETHING", "value"),
            ("ESUB_USER_REQ_DEVICE", "d"),
            ("PATH", "/usr/bin"),
        ]))
        .expect("valid");

        // Every QRMI_* name, whatever the vendor namespace.
        assert_eq!(config.qrmi_vars.len(), 3);
        assert!(config.qrmi_vars.contains_key("QRMI_PASQAL_SOMETHING"));
        assert!(!config.qrmi_vars.contains_key("PATH"));
    }

    #[test]
    fn missing_qpu_type_is_an_error() {
        let error =
            Config::from_env_map(&env(&[("ESUB_USER_REQ_DEVICE", "d")])).expect_err("no type");
        assert_eq!(error, ConfigError::NoQpuType);
        assert!(error.to_string().contains("QRMI_JOB_QPU_TYPES"));
    }

    #[test]
    fn empty_qpu_type_is_an_error() {
        assert_eq!(
            Config::from_env_map(&env(&[
                ("QRMI_JOB_QPU_TYPES", "   "),
                ("ESUB_USER_REQ_DEVICE", "d"),
            ]))
            .expect_err("empty type"),
            ConfigError::NoQpuType
        );
    }

    #[test]
    fn selection_without_a_request_is_an_error() {
        let error = Config::from_env_map(&env(&[("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service")]))
            .expect_err("no request");

        assert_eq!(error, ConfigError::NoRequest);
    }

    #[test]
    fn unknown_selector_is_rejected_with_the_valid_names() {
        let error = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("ESUB_USER_REQ_SELECTOR", "nonesuch"),
            ("ESUB_USER_REQ_QPU", "qubits=1"),
        ]))
        .expect_err("bad selector");

        let rendered = error.to_string();
        assert!(rendered.contains("nonesuch"), "{rendered}");
        assert!(rendered.contains("basic"), "{rendered}");
        assert!(rendered.contains("priority"), "{rendered}");
    }

    /// The esub always sets a selector, so an absent one means a hand-built
    /// environment; defaulting keeps that working, as the Python does.
    #[test]
    fn absent_selector_defaults_to_basic() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "qiskit-runtime-service"),
            ("ESUB_USER_REQ_QPU", "qubits=128"),
        ]))
        .expect("valid");

        assert_eq!(
            config.plan,
            Plan::Select {
                selector: "basic".to_owned(),
                request: "qubits=128".to_owned()
            }
        );
    }

    #[test]
    fn whitespace_is_trimmed_from_environment_values() {
        let config = Config::from_env_map(&env(&[
            ("QRMI_JOB_QPU_TYPES", "  qiskit-runtime-service\n"),
            ("ESUB_USER_REQ_DEVICE", " ibm_fez "),
        ]))
        .expect("valid");

        assert_eq!(config.qpu_type, "qiskit-runtime-service");
        assert_eq!(
            config.plan,
            Plan::Explicit {
                device: "ibm_fez".to_owned()
            }
        );
    }
}
