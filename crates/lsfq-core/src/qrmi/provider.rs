// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Enumerating backends through QRMI and collecting their metrics.
//!
//! # Concurrency
//!
//! This is the module where the Rust rewrite earns its keep on latency.
//! QRMI's `QuantumResource` trait is `async`; the Python wrapper hides that,
//! so `get_all_backend_metrics_qrmi()` fetches each backend's `target()` and
//! `status()` strictly one after another -- N serial round-trips to IBM
//! Quantum Platform, on the critical path of every job submission.
//!
//! Here the per-backend queries are joined concurrently, so collecting
//! metrics for N backends costs roughly one round-trip rather than N.
//!
//! # Error handling
//!
//! A backend that cannot be queried is skipped with a warning, never fatal.
//! This matches the Python and is the right policy: one unreachable backend
//! must not prevent scheduling onto the others.

use std::collections::HashMap;

use futures::stream::{FuturesUnordered, StreamExt};
use qpu_select::{Candidates, QpuMetrics};
use qrmi::models::ResourceType;
use qrmi::{QuantumResource, ResourceProvider};

use super::resource_type::{ResourceTypeName, UnsupportedResourceType};
use super::target::Target;

/// Map a wire name to QRMI's `ResourceType`.
///
/// This conversion is the reason [`ResourceTypeName`] exists separately: it
/// is the only place the QRMI enum is named, so a rename upstream is a
/// one-line change here.
///
/// # Errors
///
/// Returns [`UnsupportedResourceType`] for a name this project does not
/// support.
pub fn resource_type(name: &str) -> Result<ResourceType, UnsupportedResourceType> {
    Ok(to_qrmi(ResourceTypeName::try_from(name)?))
}

/// As [`resource_type`], but only for types that can be enumerated.
///
/// Used by the selection path. The explicit-device path uses
/// [`resource_type`], which accepts the wider acquisition set.
///
/// # Errors
///
/// Returns [`UnsupportedResourceType`] with `for_enumeration` set.
pub fn enumerable_resource_type(name: &str) -> Result<ResourceType, UnsupportedResourceType> {
    Ok(to_qrmi(ResourceTypeName::for_enumeration(name)?))
}

/// Map our name onto QRMI's enum.
///
/// Total by construction: [`ResourceTypeName`] models exactly the types this
/// project accepts, each of which QRMI exposes. If QRMI renames or removes a
/// variant, this match stops compiling -- which is the point of writing it
/// out rather than using a wildcard.
fn to_qrmi(name: ResourceTypeName) -> ResourceType {
    match name {
        ResourceTypeName::IbmQuantumSystem => ResourceType::IBMQuantumSystem,
        ResourceTypeName::IbmQuantumComputeService => ResourceType::IBMQuantumComputeService,
        // NOTE: the variant is `QiskitRuntimeService`. The Python writes
        // `ResourceType.IBMQiskitRuntimeService`, which does not exist in
        // QRMI and raises AttributeError -- so `qpu_type=qiskit-runtime-service`
        // is broken in both Python code paths today. Using the real name
        // here fixes it.
        ResourceTypeName::QiskitRuntimeService => ResourceType::QiskitRuntimeService,
        ResourceTypeName::PasqalCloud => ResourceType::PasqalCloud,
        ResourceTypeName::IqmServer => ResourceType::IQMServer,
        ResourceTypeName::AliceBobFelis => ResourceType::AliceBobFelis,
    }
}

/// Build a QRMI provider for enumerating backends.
///
/// `environment` supplies the provider's configuration; only `QRMI_*` names
/// are passed through, as the Python's dict comprehension does.
///
/// No bearer token is produced here: QRMI acquires and renews the IAM token
/// internally on each call, which is what replaced the hand-rolled token
/// cache the original script maintained.
///
/// # Errors
///
/// Returns [`ProviderError`] if the type is unsupported or QRMI rejects the
/// configuration.
pub fn build<'a, I>(
    qpu_type: &str,
    environment: I,
) -> Result<Box<dyn ResourceProvider>, ProviderError>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let resource_type = enumerable_resource_type(qpu_type)?;

    let settings: HashMap<String, String> = environment
        .into_iter()
        .filter(|(key, _)| key.starts_with("QRMI_"))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

    qrmi::create_provider(&resource_type, &settings).map_err(|source| ProviderError::Create {
        source: source.to_string(),
    })
}

/// One backend's metrics, or the reason it could not be queried.
#[derive(Debug)]
pub enum Queried {
    /// The backend answered.
    Ok {
        /// Its resource id.
        name: String,
        /// Its metrics.
        metrics: QpuMetrics,
    },
    /// The backend could not be queried and was skipped.
    Skipped {
        /// Its resource id, if that much was known.
        name: Option<String>,
        /// Why it was skipped.
        reason: String,
    },
}

/// Enumerate backends and collect each one's metrics, concurrently.
///
/// Returns the candidates the selection policies consume, plus the backends
/// that were skipped so the caller can warn about them.
///
/// # Errors
///
/// Returns [`ProviderError`] only if the enumeration itself fails. An
/// individual backend's failure is reported as [`Queried::Skipped`], not an
/// error.
pub async fn collect_metrics(
    provider: &dyn ResourceProvider,
    filters: Option<String>,
) -> Result<(Candidates, Vec<Queried>), ProviderError> {
    let resources =
        provider
            .resources(filters)
            .await
            .map_err(|source| ProviderError::Enumerate {
                source: source.to_string(),
            })?;

    // Each backend needs two round-trips (status and target). Issuing them
    // for all backends at once is the whole point of this function.
    let mut pending = FuturesUnordered::new();
    for resource in resources {
        pending.push(query_one(resource));
    }

    let mut candidates = Candidates::new();
    let mut skipped = Vec::new();

    while let Some(result) = pending.next().await {
        match result {
            Queried::Ok { name, metrics } => {
                candidates.insert(name, metrics);
            }
            skip @ Queried::Skipped { .. } => skipped.push(skip),
        }
    }

    Ok((candidates, skipped))
}

/// Query one backend's status and target.
async fn query_one(mut resource: Box<dyn QuantumResource + Send + Sync>) -> Queried {
    let name = match resource.resource_id().await {
        Ok(name) => name,
        Err(error) => {
            return Queried::Skipped {
                name: None,
                reason: error.to_string(),
            };
        }
    };

    let status = match resource.status().await {
        Ok(status) => status,
        Err(error) => {
            return Queried::Skipped {
                name: Some(name),
                reason: error.to_string(),
            };
        }
    };

    let target = match resource.target().await {
        Ok(target) => target,
        Err(error) => {
            return Queried::Skipped {
                name: Some(name),
                reason: error.to_string(),
            };
        }
    };

    let parsed = match Target::parse(&target.value) {
        Ok(parsed) => parsed,
        Err(error) => {
            return Queried::Skipped {
                name: Some(name),
                reason: format!("target payload is not valid JSON: {error}"),
            };
        }
    };

    // QRMI reports `online` where the REST status endpoint reported the
    // message `available`.
    let available = matches!(status.status, qrmi::ResourceStatusCode::Online);

    let pending_jobs = status.pending_job_count;

    Queried::Ok {
        name,
        metrics: parsed.to_metrics(pending_jobs, available),
    }
}

/// Talking to QRMI failed.
#[derive(Debug)]
pub enum ProviderError {
    /// The requested resource type is not supported.
    UnsupportedType(UnsupportedResourceType),
    /// QRMI rejected the provider configuration.
    Create {
        /// QRMI's message.
        source: String,
    },
    /// Enumerating backends failed.
    Enumerate {
        /// QRMI's message.
        source: String,
    },
}

impl From<UnsupportedResourceType> for ProviderError {
    fn from(error: UnsupportedResourceType) -> Self {
        Self::UnsupportedType(error)
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedType(error) => write!(f, "{error}"),
            Self::Create { source } => write!(f, "cannot build QRMI provider: {source}"),
            Self::Enumerate { source } => write!(f, "cannot enumerate backends: {source}"),
        }
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping onto QRMI's enum. This is the assertion that would fail
    /// first if QRMI renamed or reordered its variants.
    #[test]
    fn wire_names_map_onto_qrmi_types() {
        assert!(matches!(
            resource_type("ibm-quantum-system").expect("supported"),
            ResourceType::IBMQuantumSystem
        ));
        assert!(matches!(
            resource_type("ibm-quantum-compute-service").expect("supported"),
            ResourceType::IBMQuantumComputeService
        ));
        assert!(matches!(
            resource_type("qiskit-runtime-service").expect("supported"),
            ResourceType::QiskitRuntimeService
        ));
    }

    /// Acquisition accepts the wider set; enumeration does not.
    #[test]
    fn non_ibm_types_acquire_but_do_not_enumerate() {
        for name in ["pasqal-cloud", "iqm-server", "alice-bob-felis"] {
            assert!(resource_type(name).is_ok(), "{name} must be acquirable");
            assert!(
                enumerable_resource_type(name).is_err(),
                "{name} has no rankable metrics"
            );
        }
    }

    #[test]
    fn unsupported_type_is_rejected_by_both() {
        assert!(resource_type("").is_err());
        assert!(resource_type("nonesuch").is_err());
        assert!(enumerable_resource_type("").is_err());
        assert!(enumerable_resource_type("nonesuch").is_err());
    }

    /// The two enumeration failures need different advice, so they must not
    /// share a message.
    #[test]
    fn enumeration_errors_distinguish_unknown_from_unrankable() {
        let unknown = enumerable_resource_type("nonesuch").expect_err("unknown");
        assert!(unknown.to_string().contains("unsupported"), "{unknown}");

        let unrankable = enumerable_resource_type("pasqal-cloud").expect_err("unrankable");
        let rendered = unrankable.to_string();
        assert!(rendered.contains("cannot be enumerated"), "{rendered}");
        assert!(
            rendered.contains("device="),
            "must suggest the fix: {rendered}"
        );
    }

    #[test]
    fn whitespace_is_tolerated_as_in_python() {
        assert!(resource_type("  qiskit-runtime-service  ").is_ok());
    }
}
