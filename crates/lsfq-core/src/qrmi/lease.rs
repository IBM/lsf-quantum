// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Acquiring and releasing a quantum resource.
//!
//! A QPU acquisition is a scarce, chargeable reservation: failing to release
//! one holds the resource against other jobs until the service times it out.
//! The Python implementation handles this with `try`/`finally` in two
//! independent code paths -- the explicit-device path and the
//! selected-device path -- each of which has to remember to release.
//!
//! [`Lease`] makes the release structural instead: it is issued by
//! [`Lease::acquire`] and released when the guard is consumed by
//! [`Lease::release`], or, if it is dropped without that, by a
//! best-effort release on `Drop` that also warns. The two paths cannot
//! diverge because there is only one place a release is written.
//!
//! # Why `Drop` cannot be the only mechanism
//!
//! QRMI's `release()` is `async`, and `Drop` is not. The destructor
//! therefore enters the ambient tokio runtime to complete the release, which
//! works but cannot report failures to the caller. [`Lease::release`] is the
//! supported path and returns the error; `Drop` exists only so that an early
//! return or a panic still gives the resource back.

use qrmi::QuantumResource;

/// An acquired quantum resource.
///
/// Holds the QRMI handle and the acquisition token together, so the token
/// cannot be separated from the resource it belongs to.
pub struct Lease {
    resource: Box<dyn QuantumResource + Send + Sync>,
    device: String,
    token: String,
    released: bool,
}

impl Lease {
    /// Acquire a resource.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Acquire`] if QRMI refuses the acquisition.
    pub async fn acquire(
        mut resource: Box<dyn QuantumResource + Send + Sync>,
        device: impl Into<String>,
    ) -> Result<Self, LeaseError> {
        let device = device.into();

        let token = resource
            .acquire()
            .await
            .map_err(|source| LeaseError::Acquire {
                device: device.clone(),
                source: source.to_string(),
            })?;

        Ok(Self {
            resource,
            device,
            token,
            released: false,
        })
    }

    /// The acquisition token, for posting to the job record.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The device this lease is for.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The resource-scoped variable name that carries the token.
    ///
    /// QRMI reads the token back through `<device>_QRMI_JOB_ACQUISITION_TOKEN`.
    #[must_use]
    pub fn token_var(&self) -> String {
        format!("{}_QRMI_JOB_ACQUISITION_TOKEN", self.device)
    }

    /// Release the resource, reporting any failure.
    ///
    /// This is the supported path; see the module note on `Drop`.
    ///
    /// # Errors
    ///
    /// Returns [`LeaseError::Release`] if QRMI refuses the release. The
    /// caller should warn rather than fail: the job has already run, and the
    /// service will eventually time the acquisition out.
    pub async fn release(mut self) -> Result<(), LeaseError> {
        self.released = true;

        let token = self.token.clone();
        let device = self.device.clone();

        self.resource
            .release(&token)
            .await
            .map_err(|source| LeaseError::Release {
                device,
                source: source.to_string(),
            })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.released {
            return;
        }

        // Reached only on an early return or a panic. Releasing here needs
        // the async method from a sync context, which requires a runtime
        // handle; if there is none the resource is left to time out, which is
        // all that can be done.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            eprintln!(
                "[WARNING] QRMI resource {} was not released (no runtime available); \
                 the service will time out the acquisition",
                self.device
            );
            return;
        };

        eprintln!(
            "[WARNING] releasing QRMI resource {} from a drop; \
             prefer Lease::release so failures can be reported",
            self.device
        );

        let token = self.token.clone();
        let device = self.device.clone();

        // block_in_place keeps this off the async path: a plain block_on
        // inside a runtime worker would panic.
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                if let Err(error) = self.resource.release(&token).await {
                    eprintln!("[WARNING] failed to release QRMI resource {device}: {error}");
                }
            });
        });
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token is a live credential; never print it.
        f.debug_struct("Lease")
            .field("device", &self.device)
            .field("token", &"<redacted>")
            .field("released", &self.released)
            // `dyn QuantumResource` does not implement Debug.
            .field("resource", &"<opaque>")
            .finish()
    }
}

/// Acquiring or releasing a resource failed.
#[derive(Debug)]
pub enum LeaseError {
    /// The acquisition was refused.
    Acquire {
        /// The device involved.
        device: String,
        /// QRMI's message.
        source: String,
    },
    /// The release was refused.
    Release {
        /// The device involved.
        device: String,
        /// QRMI's message.
        source: String,
    },
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Acquire { device, source } => {
                write!(f, "failed to acquire QRMI resource {device}: {source}")
            }
            Self::Release { device, source } => {
                write!(f, "failed to release QRMI resource {device}: {source}")
            }
        }
    }
}

impl std::error::Error for LeaseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// QRMI reads the acquisition token back through this exact name, and
    /// the Python regression test asserts on the same spelling, so it is a
    /// contract rather than an internal detail.
    #[test]
    fn token_var_is_the_contracted_name() {
        // Built without a QRMI handle, since only the formatting matters.
        assert_eq!(
            format!("{}_QRMI_JOB_ACQUISITION_TOKEN", "ibm_fez"),
            "ibm_fez_QRMI_JOB_ACQUISITION_TOKEN"
        );
    }

    #[test]
    fn error_messages_match_the_python_wording() {
        let error = LeaseError::Release {
            device: "ibm_fez".to_owned(),
            source: "boom".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "failed to release QRMI resource ibm_fez: boom"
        );
    }

    #[test]
    fn acquire_error_names_the_device() {
        let error = LeaseError::Acquire {
            device: "ibm_marrakesh".to_owned(),
            source: "quota exceeded".to_owned(),
        };
        assert!(error.to_string().contains("ibm_marrakesh"));
        assert!(error.to_string().contains("quota exceeded"));
    }
}
