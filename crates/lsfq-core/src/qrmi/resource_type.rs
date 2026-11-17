// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! QRMI resource type names.
//!
//! `QRMI_JOB_QPU_TYPES` carries the resource type as a string. Mapping it to
//! QRMI's `ResourceType` needs the QRMI crate, but *validating* it does not,
//! so the name handling lives here, unfeatured, and the conversion sits in
//! the feature-gated `crate::qrmi::provider`.

use std::fmt;

/// A QRMI resource type, as spelled in `QRMI_JOB_QPU_TYPES`.
///
/// # Enumeration versus acquisition
///
/// The two jobstarter paths need different subsets, and the Python
/// implementation reflects that with two separate dictionaries:
///
/// * `build_provider()` maps three types, because *enumerating* backends and
///   comparing their metrics only makes sense where the metrics exist --
///   qubit counts, CLOPS and calibration medians are IBM-shaped;
/// * `build_quantum_resource()` maps six, because *acquiring* a named device
///   needs no metrics at all.
///
/// Both are modelled here: [`ResourceTypeName::ALL`] is the acquisition set
/// and [`ResourceTypeName::supports_enumeration`] marks the narrower one, so
/// the explicit-device path keeps working for a Pasqal or IQM resource while
/// the selection path still rejects it with a clear message rather than
/// selecting on absent metrics.
///
/// `pasqal-local` and `oqtopus` exist in QRMI but are absent from the Python
/// mapping, so they are not accepted here either; adding one is a deliberate
/// change, not an oversight to be filled in silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceTypeName {
    /// `ibm-quantum-system`.
    IbmQuantumSystem,
    /// `ibm-quantum-compute-service`.
    IbmQuantumComputeService,
    /// `qiskit-runtime-service`.
    QiskitRuntimeService,
    /// `pasqal-cloud`. Acquisition only.
    PasqalCloud,
    /// `iqm-server`. Acquisition only.
    IqmServer,
    /// `alice-bob-felis`. Acquisition only.
    AliceBobFelis,
}

impl ResourceTypeName {
    /// Every type that can be acquired, in the order a help message should
    /// list them.
    pub const ALL: [Self; 6] = [
        Self::IbmQuantumSystem,
        Self::IbmQuantumComputeService,
        Self::QiskitRuntimeService,
        Self::PasqalCloud,
        Self::IqmServer,
        Self::AliceBobFelis,
    ];

    /// The types that can also be enumerated for metric-based selection.
    pub const ENUMERABLE: [Self; 3] = [
        Self::IbmQuantumSystem,
        Self::IbmQuantumComputeService,
        Self::QiskitRuntimeService,
    ];

    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IbmQuantumSystem => "ibm-quantum-system",
            Self::IbmQuantumComputeService => "ibm-quantum-compute-service",
            Self::QiskitRuntimeService => "qiskit-runtime-service",
            Self::PasqalCloud => "pasqal-cloud",
            Self::IqmServer => "iqm-server",
            Self::AliceBobFelis => "alice-bob-felis",
        }
    }

    /// Whether backends of this type can be enumerated and compared.
    ///
    /// Selection ranks on qubit counts, CLOPS and calibration medians, which
    /// the non-IBM providers do not publish in that shape.
    #[must_use]
    pub const fn supports_enumeration(self) -> bool {
        matches!(
            self,
            Self::IbmQuantumSystem | Self::IbmQuantumComputeService | Self::QiskitRuntimeService
        )
    }

    /// Resolve a wire name.
    ///
    /// Surrounding whitespace is tolerated because the value arrives through
    /// the environment; the Python applies `.strip()` for the same reason.
    /// Matching is otherwise exact: these are protocol identifiers, not
    /// user-facing words.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

impl fmt::Display for ResourceTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unsupported resource type was requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedResourceType {
    /// The value that was not recognised.
    pub value: String,
    /// Whether the caller needed enumeration, which narrows the valid set.
    pub for_enumeration: bool,
}

impl UnsupportedResourceType {
    /// The types that would have been accepted.
    #[must_use]
    pub fn accepted(&self) -> &'static [ResourceTypeName] {
        if self.for_enumeration {
            &ResourceTypeName::ENUMERABLE
        } else {
            &ResourceTypeName::ALL
        }
    }
}

impl fmt::Display for UnsupportedResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.for_enumeration {
            // Distinguish "no such type" from "that type cannot be ranked",
            // because the fix differs: fix the spelling, or name a device
            // explicitly with device=.
            if ResourceTypeName::parse(&self.value).is_some() {
                write!(
                    f,
                    "QRMI QPU type {:?} cannot be enumerated for metric-based \
                     selection; request a device explicitly with device=<name>, \
                     or use one of: ",
                    self.value
                )?;
            } else {
                write!(
                    f,
                    "unsupported QRMI QPU type {:?} for backend enumeration; \
                     expected one of: ",
                    self.value
                )?;
            }
        } else {
            write!(
                f,
                "unsupported QRMI QPU type {:?}; expected one of: ",
                self.value
            )?;
        }

        let mut names = self.accepted().iter();
        if let Some(first) = names.next() {
            write!(f, "{first}")?;
        }
        for name in names {
            write!(f, ", {name}")?;
        }
        Ok(())
    }
}

impl std::error::Error for UnsupportedResourceType {}

impl TryFrom<&str> for ResourceTypeName {
    type Error = UnsupportedResourceType;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value).ok_or_else(|| UnsupportedResourceType {
            value: value.to_owned(),
            for_enumeration: false,
        })
    }
}

impl ResourceTypeName {
    /// Resolve a type that must also support enumeration.
    ///
    /// # Errors
    ///
    /// Returns [`UnsupportedResourceType`] with `for_enumeration` set, whose
    /// message distinguishes an unknown spelling from a known type that
    /// cannot be ranked.
    pub fn for_enumeration(value: &str) -> Result<Self, UnsupportedResourceType> {
        match Self::parse(value) {
            Some(name) if name.supports_enumeration() => Ok(name),
            _ => Err(UnsupportedResourceType {
                value: value.trim().to_owned(),
                for_enumeration: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_round_trip() {
        for name in ResourceTypeName::ALL {
            assert_eq!(ResourceTypeName::parse(name.as_str()), Some(name));
        }
    }

    /// These three strings are the external contract with the esub and with
    /// QRMI; changing one breaks deployed submissions.
    #[test]
    fn wire_names_are_the_contracted_spellings() {
        assert_eq!(
            ResourceTypeName::IbmQuantumSystem.as_str(),
            "ibm-quantum-system"
        );
        assert_eq!(
            ResourceTypeName::IbmQuantumComputeService.as_str(),
            "ibm-quantum-compute-service"
        );
        assert_eq!(
            ResourceTypeName::QiskitRuntimeService.as_str(),
            "qiskit-runtime-service"
        );
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(
            ResourceTypeName::parse("  qiskit-runtime-service\n"),
            Some(ResourceTypeName::QiskitRuntimeService)
        );
    }

    #[test]
    fn unknown_types_are_rejected() {
        assert_eq!(ResourceTypeName::parse(""), None);
        assert_eq!(ResourceTypeName::parse("nonesuch"), None);
        // Not case-insensitive: these are protocol identifiers.
        assert_eq!(ResourceTypeName::parse("IBM-Quantum-System"), None);
        // In QRMI but absent from the Python mapping, so not accepted here.
        assert_eq!(ResourceTypeName::parse("pasqal-local"), None);
        assert_eq!(ResourceTypeName::parse("oqtopus"), None);
    }

    /// The acquisition set is the Python `build_quantum_resource()` mapping;
    /// the enumeration set is `build_provider()`'s. Both are contracts.
    #[test]
    fn enumeration_set_is_narrower_than_acquisition_set() {
        assert_eq!(ResourceTypeName::ALL.len(), 6);
        assert_eq!(ResourceTypeName::ENUMERABLE.len(), 3);

        for name in ResourceTypeName::ENUMERABLE {
            assert!(name.supports_enumeration(), "{name}");
            assert!(
                ResourceTypeName::ALL.contains(&name),
                "{name} must be acquirable"
            );
        }

        for name in [
            ResourceTypeName::PasqalCloud,
            ResourceTypeName::IqmServer,
            ResourceTypeName::AliceBobFelis,
        ] {
            assert!(
                !name.supports_enumeration(),
                "{name} has no rankable metrics"
            );
        }
    }

    #[test]
    fn for_enumeration_accepts_only_rankable_types() {
        assert!(ResourceTypeName::for_enumeration("qiskit-runtime-service").is_ok());
        assert!(ResourceTypeName::for_enumeration("pasqal-cloud").is_err());
        assert!(ResourceTypeName::for_enumeration("nonesuch").is_err());
    }

    #[test]
    fn error_lists_the_supported_types() {
        let error = ResourceTypeName::try_from("nonesuch").expect_err("unsupported");
        let rendered = error.to_string();

        assert!(rendered.contains("nonesuch"), "{rendered}");
        // An acquisition error lists all six.
        assert!(rendered.contains("qiskit-runtime-service"), "{rendered}");
        assert!(rendered.contains("pasqal-cloud"), "{rendered}");
        assert_eq!(error.accepted().len(), 6);
    }

    #[test]
    fn enumeration_error_lists_only_the_rankable_types() {
        let error = ResourceTypeName::for_enumeration("pasqal-cloud").expect_err("unrankable");

        assert_eq!(error.accepted().len(), 3);
        let rendered = error.to_string();
        assert!(
            !rendered.contains("iqm-server"),
            "must not offer an unrankable type: {rendered}"
        );
    }
}
