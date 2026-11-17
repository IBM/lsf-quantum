// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The QRMI-backed [`QpuSource`].
//!
//! Holds one `QuantumResource` and reuses it for every cycle, as the
//! original does: QRMI renews the IAM bearer token internally, so there is
//! no token cache to maintain and no reason to rebuild the resource.

use lsfq_core::qrmi::qrmi_crate::{
    ibm::IBMQuantumComputeService, QuantumResource, ResourceStatusCode,
};

use crate::poll::{QpuSource, Reading};

/// Queries one QPU through QRMI.
pub struct QrmiSource {
    resource: IBMQuantumComputeService,
    device: String,
}

impl QrmiSource {
    /// Build a source for one device.
    ///
    /// The resource-scoped QRMI settings (`<device>_QRMI_IBM_QCS_*`) must
    /// already be in the environment: QRMI resolves its configuration only
    /// through those names, which is what
    /// [`lsfq_core::Credentials::resolve_for_device`] produces.
    ///
    /// # Errors
    ///
    /// Returns the QRMI error if the resource cannot be constructed, which
    /// usually means the credentials are absent or malformed.
    pub fn new(device: &str) -> Result<Self, String> {
        let resource = IBMQuantumComputeService::new(device)
            .map_err(|error| format!("failed to create QRMI resource for {device}: {error}"))?;

        Ok(Self {
            resource,
            device: device.to_owned(),
        })
    }
}

impl QpuSource for QrmiSource {
    async fn poll(&mut self) -> (Reading, Vec<String>) {
        let mut warnings = Vec::new();
        let mut reading = Reading::default();

        // Both QRMI methods take `&mut self`, so they cannot be joined on a
        // single resource -- the borrow checker rejects it, and rightly:
        // QRMI's client is not re-entrant. They are therefore awaited in
        // sequence, as the original does.
        //
        // This costs nothing here. An ELIM polls one QPU every ten seconds,
        // so two round-trips per cycle are not on any critical path. (The
        // concurrency that matters is across *backends*, which is the
        // jobstarter's enumeration in `lsfq_core::qrmi::provider` -- there
        // each backend has its own resource, so the queries do join.)
        let status = self.resource.status().await;
        let target = self.resource.target().await;

        match status {
            Ok(status) => {
                reading.status_available = true;
                reading.pending_jobs = status.pending_job_count;

                // Reported for the operator's benefit; the ELIM has no index
                // for it, as LSF gates on the resource being defined at all.
                if !matches!(status.status, ResourceStatusCode::Online) {
                    warnings.push(format!(
                        "{} reports status {:?}{}",
                        self.device,
                        status.status,
                        status
                            .status_reason
                            .as_deref()
                            .map_or_else(String::new, |reason| format!(": {reason}")),
                    ));
                }
            }
            Err(error) => warnings.push(format!("cannot read status: {error}")),
        }

        match target {
            Ok(target) => reading.target = Some(target.value),
            Err(error) => warnings.push(format!("cannot read target: {error}")),
        }

        (reading, warnings)
    }
}
