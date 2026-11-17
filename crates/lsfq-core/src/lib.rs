// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Shared routines for the LSF quantum integration.
//!
//! The runtime machinery that `esub.qrmi`, `jobstarter.qrmi` and `elim.qpu`
//! all need: diagnostics, credentials, the LSF protocol surface, the QRMI
//! adapter, and construction of a job's environment..
//!
//! Selection *policy* is not here -- that is [`qpu_select`], which this
//! crate depends on and re-exports. The split is deliberate: `qpu-select` is
//! pure and reusable by other schedulers, while everything in this crate is
//! specific to LSF and to QRMI.
//!
//! # Modules
//!
//! | Module | Covers |
//! |---|---|
//! | [`diag`] | debug levels, warnings, `LSB_SUB_ABORT_VALUE` |
//! | [`creds`] | `.env` and `$LSF_ENVDIR/env.qpu`, legacy aliases, endpoint defaults |
//! | [`lsf`] | `LSF_SUB4_SUB_ENV_VARS`, `bpost`, ELIM lines, host→QPU mapping |
//! | [`qrmi`] | `target()` parsing, resource types, and (feature-gated) the provider and lease |
//! | [`jobenv`] | the job's environment, built explicitly |
//! | [`exit`] | child outcome → LSF exit status |
//!
//! # The `qrmi` feature
//!
//! Calling QRMI pulls in the QRMI crate and a tokio runtime. Most of this
//! crate is parsers and formatters that need neither, so those live behind
//! the `qrmi` feature, off by default. The binaries enable it; the unit
//! tests do not need to, which keeps them hermetic and fast.
//!
//! ```text
//! cargo test                      # parsers, formatters, env logic
//! cargo test --features qrmi      # the above plus the QRMI adapter
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod creds;
pub mod diag;
pub mod exit;
pub mod jobenv;
pub mod lsf;
pub mod qrmi;

pub use creds::{Credentials, CredentialsError};
pub use diag::{DebugLevel, Diagnostics, Program};
pub use exit::Outcome;
pub use jobenv::EnvMap;

// Re-exported so a binary needs one dependency rather than two, and so the
// version the policies come from is unambiguous.
pub use qpu_select;

/// The environment variables the `esub` passes to the jobstarter.
///
/// Named here rather than written as literals at each use, because they are
/// the contract between the two programs: the `esub` sets them through
/// `LSF_SUB4_SUB_ENV_VARS` and the jobstarter reads them back.
pub mod esub_vars {
    /// The selection policy name (`basic`, `health` or `priority`).
    pub const SELECTOR: &str = "ESUB_USER_REQ_SELECTOR";
    /// An explicitly requested device, bypassing selection.
    pub const DEVICE: &str = "ESUB_USER_REQ_DEVICE";
    /// The QPU attribute request string.
    pub const QPU: &str = "ESUB_USER_REQ_QPU";

    /// The QRMI resource type for the job.
    pub const QPU_TYPES: &str = "QRMI_JOB_QPU_TYPES";
    /// The resource the job acquired.
    pub const QPU_RESOURCES: &str = "QRMI_JOB_QPU_RESOURCES";
    /// The device selection, advertised to the job.
    pub const BEST_DEVICE: &str = "QRMI_IBM_QCS_BEST_DEVICE";
}

/// Whether a device name means "no explicit device was requested".
///
/// `ESUB_USER_REQ_DEVICE` arrives through the environment, so an unset
/// Python `None` is serialised as the literal text `"None"`. The jobstarter
/// therefore has to treat `"none"`, `"null"` and `""` as absent, or it would
/// try to acquire a backend called `None`.
///
/// # Example
///
/// ```
/// use lsfq_core::is_unset_device;
///
/// assert!(is_unset_device("None"));
/// assert!(is_unset_device(""));
/// assert!(!is_unset_device("ibm_fez"));
/// ```
#[must_use]
pub fn is_unset_device(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "null"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_device_spellings_are_recognised() {
        // The Python serialises None into the environment as "None".
        for value in ["None", "none", "NONE", "null", "NULL", "", "  ", " none "] {
            assert!(is_unset_device(value), "{value:?} should mean unset");
        }
    }

    #[test]
    fn real_device_names_are_not_unset() {
        for value in ["ibm_fez", "ibm_marrakesh", "none_of_your_business"] {
            assert!(!is_unset_device(value), "{value:?} is a device");
        }
    }

    #[test]
    fn esub_var_names_are_the_contracted_spellings() {
        assert_eq!(esub_vars::SELECTOR, "ESUB_USER_REQ_SELECTOR");
        assert_eq!(esub_vars::DEVICE, "ESUB_USER_REQ_DEVICE");
        assert_eq!(esub_vars::QPU, "ESUB_USER_REQ_QPU");
        assert_eq!(esub_vars::QPU_TYPES, "QRMI_JOB_QPU_TYPES");
        assert_eq!(esub_vars::QPU_RESOURCES, "QRMI_JOB_QPU_RESOURCES");
        assert_eq!(esub_vars::BEST_DEVICE, "QRMI_IBM_QCS_BEST_DEVICE");
    }
}
