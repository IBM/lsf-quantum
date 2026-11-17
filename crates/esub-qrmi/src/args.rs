// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Parsing the `bsub -a "qrmi(...)"` argument list.
//!
//! LSF splits the parenthesised list on commas and passes the pieces as
//! `argv`, so the esub receives `key=value` tokens:
//!
//! ```text
//! bsub -a "qrmi(file=.env,qpu.qubits=120,selector=priority)" my_app
//!   -> argv: ["file=.env", "qpu.qubits=120", "selector=priority"]
//! ```
//!
//! The Python builds this with `OmegaConf`: a structured `Config` dataclass,
//! `set_struct(True)` to reject unknown keys, `from_cli()` to read the
//! dotlist, and `missing_keys()` to catch an absent `file=`. That is a lot
//! of machinery for one flat key/value list, and it is not a conventional
//! CLI grammar -- `clap` would have to be fought rather than used -- so it
//! is written out here.
//!
//! Every externally visible behaviour is preserved, including two quirks
//! documented where they occur: the field-order normalisation in
//! [`QpuAttributes::to_lsf_string`], and the acceptance of attribute names
//! that the help text spells differently.
//!
//! One rule is deliberately stricter than the Python: `selector=` combined
//! with `device=` is rejected rather than silently ignored. See the check in
//! [`parse`] for why.

use std::fmt;
use std::path::PathBuf;

/// The QPU attributes a user may constrain.
///
/// Declaration order matters: it is the order
/// [`QpuAttributes::to_lsf_string`] emits, which the Python inherits from
/// `dataclasses.fields()`. See that method for why this is a contract.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QpuAttributes {
    /// Minimum number of qubits.
    pub qubits: Option<u32>,
    /// Required QPU version.
    pub qpu_version: Option<String>,
    /// Required processor family.
    pub processor_type: Option<String>,
    /// Minimum CLOPS.
    pub clops: Option<f64>,
    /// Maximum pending jobs.
    pub pending_jobs: Option<u64>,
    /// Maximum median readout error.
    pub readout_error_median: Option<f64>,
    /// Maximum median SX-gate error.
    pub sx_error_median: Option<f64>,
    /// Maximum median CZ-gate error.
    pub cz_error_median: Option<f64>,
    /// Minimum median T1, microseconds.
    pub t1_median_us: Option<f64>,
    /// Minimum median T2, microseconds.
    pub t2_median_us: Option<f64>,
}

impl QpuAttributes {
    /// Whether no attribute was given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Serialise for `ESUB_USER_REQ_QPU`.
    ///
    /// # Field order is normalised, deliberately
    ///
    /// The attributes are emitted in the order they are declared above, not
    /// the order the user typed them. That is what the Python does --
    /// `to_lsf_string()` iterates `dataclasses.fields()`, which yields
    /// declaration order -- and it has a visible consequence: the
    /// `priority` selector ranks on requirements *in the order of the
    /// request string*, so
    ///
    /// ```text
    /// qpu.clops=240000,qpu.qubits=120
    /// qpu.qubits=120,qpu.clops=240000
    /// ```
    ///
    /// both arrive at the jobstarter as `qubits=120;clops=240000` and
    /// therefore select identically. A user cannot express "rank on CLOPS
    /// first" through the esub.
    ///
    /// This is reproduced rather than fixed. Preserving CLI order would be
    /// a silent change in which backend a job lands on, for every site
    /// already using `selector=priority` -- a scheduling change disguised as
    /// a refactor. It belongs in a separate, announced change; the test
    /// `cli_order_does_not_affect_the_request_string` pins the current
    /// behaviour so that change is deliberate when it comes.
    #[must_use]
    pub fn to_lsf_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // The names here are the ones the jobstarter's request parser
        // accepts (`qpu_select::Metric`), which are not always the spelling
        // the help text uses; see `Attribute::parse`.
        if let Some(value) = self.qubits {
            parts.push(format!("qubits={value}"));
        }
        if let Some(value) = &self.qpu_version {
            parts.push(format!("qpu_version={value}"));
        }
        if let Some(value) = &self.processor_type {
            parts.push(format!("processor_type={value}"));
        }
        if let Some(value) = self.clops {
            parts.push(format!("clops={}", format_number(value)));
        }
        if let Some(value) = self.pending_jobs {
            parts.push(format!("pending_jobs={value}"));
        }
        if let Some(value) = self.readout_error_median {
            parts.push(format!("readout_error_median={}", format_number(value)));
        }
        if let Some(value) = self.sx_error_median {
            parts.push(format!("sx_error_median={}", format_number(value)));
        }
        if let Some(value) = self.cz_error_median {
            parts.push(format!("cz_error_median={}", format_number(value)));
        }
        if let Some(value) = self.t1_median_us {
            parts.push(format!("T1_median_us={}", format_number(value)));
        }
        if let Some(value) = self.t2_median_us {
            parts.push(format!("T2_median_us={}", format_number(value)));
        }

        parts.join(";")
    }

    /// Validate the attributes that were supplied.
    ///
    /// Only populated attributes are checked, as the Python's
    /// `validate_qpu_attributes()` does.
    ///
    /// # Errors
    ///
    /// Returns [`ArgsError::InvalidValue`] for a non-positive qubit count or
    /// a negative metric.
    pub fn validate(&self) -> Result<(), ArgsError> {
        if self.qubits == Some(0) {
            return Err(ArgsError::InvalidValue {
                attribute: "qpu.qubits".to_owned(),
                reason: "must be greater than zero".to_owned(),
            });
        }

        let non_negative = [
            ("qpu.clops", self.clops),
            ("qpu.readout_error_median", self.readout_error_median),
            ("qpu.sx_error_median", self.sx_error_median),
            ("qpu.cz_error_median", self.cz_error_median),
            ("qpu.T1_median_us", self.t1_median_us),
            ("qpu.T2_median_us", self.t2_median_us),
        ];

        for (name, value) in non_negative {
            if let Some(value) = value {
                if value < 0.0 {
                    return Err(ArgsError::InvalidValue {
                        attribute: name.to_owned(),
                        reason: "must not be negative".to_owned(),
                    });
                }
                if !value.is_finite() {
                    return Err(ArgsError::InvalidValue {
                        attribute: name.to_owned(),
                        reason: "must be a finite number".to_owned(),
                    });
                }
            }
        }

        Ok(())
    }
}

/// Render a float without a trailing `.0`, so an integral threshold reaches
/// the jobstarter as an integer.
///
/// Python formats `240000.0` as `240000.0`, which the request parser reads
/// as a float and compares numerically -- so either spelling selects the
/// same backend. The shorter form is used because it is what a user typed.
fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// What the user asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The credentials file, as named by `file=`.
    pub file: PathBuf,
    /// The selection policy.
    pub selector: String,
    /// An explicitly requested device, if any.
    pub device: Option<String>,
    /// The requested attributes, if any.
    pub qpu: Option<QpuAttributes>,
}

/// One recognised attribute name.
#[derive(Clone, Copy)]
enum Attribute {
    Qubits,
    QpuVersion,
    ProcessorType,
    Clops,
    PendingJobs,
    ReadoutErrorMedian,
    SxErrorMedian,
    CzErrorMedian,
    T1MedianUs,
    T2MedianUs,
}

impl Attribute {
    /// Resolve an attribute name from `qpu.<name>`.
    ///
    /// Matching is ASCII-case-insensitive, which reconciles a documentation
    /// inconsistency rather than inventing tolerance: the help text and the
    /// README advertise `qpu.t1_median_us` in lower case, while the
    /// dataclass field -- and so the name `OmegaConf` actually accepts -- is
    /// `T1_median_us`. A user following the documentation would be rejected
    /// by the Python. Accepting either spelling makes both work; the
    /// canonical form is still what reaches the jobstarter.
    fn parse(name: &str) -> Option<Self> {
        let lowered = name.trim().to_ascii_lowercase();

        Some(match lowered.as_str() {
            "qubits" => Self::Qubits,
            "qpu_version" => Self::QpuVersion,
            "processor_type" => Self::ProcessorType,
            "clops" => Self::Clops,
            "pending_jobs" => Self::PendingJobs,
            "readout_error_median" => Self::ReadoutErrorMedian,
            "sx_error_median" => Self::SxErrorMedian,
            "cz_error_median" => Self::CzErrorMedian,
            "t1_median_us" => Self::T1MedianUs,
            "t2_median_us" => Self::T2MedianUs,
            _ => return None,
        })
    }

    /// Every accepted name, in the help text's spelling.
    const NAMES: [&'static str; 10] = [
        "qubits",
        "qpu_version",
        "processor_type",
        "clops",
        "pending_jobs",
        "readout_error_median",
        "sx_error_median",
        "cz_error_median",
        "T1_median_us",
        "T2_median_us",
    ];
}

/// Parse the argument list.
///
/// # Errors
///
/// Returns [`ArgsError`] for an unparseable token, an unknown key, a missing
/// `file=`, a value that fails validation, the mutually-exclusive
/// `device=`/`qpu.*` combination, or `selector=` given alongside `device=`.
pub fn parse<I, S>(arguments: I) -> Result<Request, ArgsError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut file: Option<PathBuf> = None;
    // Tracked as an Option so that an explicit `selector=basic` is
    // distinguishable from the default; the mutual-exclusion check below
    // rejects only a selector the user actually asked for. The Python's
    // Config dataclass defaults this to "basic" unconditionally.
    let mut selector: Option<String> = None;
    let mut device: Option<String> = None;
    let mut qpu = QpuAttributes::default();

    for argument in arguments {
        let argument = argument.as_ref().trim();

        if argument.is_empty() {
            // LSF can hand through an empty token when the list has a
            // trailing comma; ignoring it matches OmegaConf.
            continue;
        }

        let Some((key, value)) = argument.split_once('=') else {
            return Err(ArgsError::NotAnAssignment {
                argument: argument.to_owned(),
            });
        };

        let key = key.trim();
        let value = value.trim();

        if let Some(attribute_name) = key.strip_prefix("qpu.") {
            let attribute =
                Attribute::parse(attribute_name).ok_or_else(|| ArgsError::UnknownAttribute {
                    name: attribute_name.to_owned(),
                })?;

            assign(&mut qpu, attribute, attribute_name, value)?;
            continue;
        }

        match key {
            "file" => file = Some(PathBuf::from(value)),
            "selector" => selector = Some(value.to_owned()),
            "device" => device = Some(value.to_owned()),
            // `set_struct(True)` makes OmegaConf reject an unknown key; so
            // does this. A typo in a submission should be reported, not
            // ignored into a default.
            "qpu" => {
                return Err(ArgsError::UnknownKey {
                    key: "qpu".to_owned(),
                    hint: Some("use qpu.<attribute>=<value>".to_owned()),
                })
            }
            other => {
                return Err(ArgsError::UnknownKey {
                    key: other.to_owned(),
                    hint: None,
                })
            }
        }
    }

    // `missing_keys()` in the Python: `file` is MISSING with no default.
    let file = file.ok_or(ArgsError::MissingFile)?;

    if let Some(name) = &selector {
        if lsfq_core::qpu_select::selector_by_name(name).is_none() {
            return Err(ArgsError::UnknownSelector { name: name.clone() });
        }
    }

    let qpu = if qpu.is_empty() { None } else { Some(qpu) };

    // The Python's `if device_present == qpu_present`, which rejects both
    // and neither with one message.
    match (&device, &qpu) {
        (Some(_), Some(_)) | (None, None) => return Err(ArgsError::DeviceXorQpu),
        _ => {}
    }

    // `device=` names the QPU outright, so there is nothing left to select:
    // the jobstarter takes its explicit-device path and never reads the
    // selector (see jobstarter-qrmi/src/config.rs). Accepting one here would
    // silently ignore it, so an explicit selector alongside `device=` is an
    // error rather than a no-op. The Python had no such check; this is a
    // deliberate addition, not a ported quirk.
    if device.is_some() {
        if let Some(name) = &selector {
            return Err(ArgsError::SelectorWithDevice { name: name.clone() });
        }
    }

    // Applied after validation so the default is never mistaken for a
    // user-supplied value.
    let selector = selector.unwrap_or_else(|| "basic".to_owned());

    if let Some(qpu) = &qpu {
        qpu.validate()?;
    }

    Ok(Request {
        file,
        selector,
        device,
        qpu,
    })
}

/// Store one parsed attribute value.
fn assign(
    qpu: &mut QpuAttributes,
    attribute: Attribute,
    name: &str,
    value: &str,
) -> Result<(), ArgsError> {
    let invalid = |reason: &str| ArgsError::InvalidValue {
        attribute: format!("qpu.{name}"),
        reason: reason.to_owned(),
    };

    match attribute {
        Attribute::Qubits => {
            qpu.qubits = Some(
                value
                    .parse::<u32>()
                    .map_err(|_| invalid("expected a non-negative whole number"))?,
            );
        }
        Attribute::PendingJobs => {
            qpu.pending_jobs = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| invalid("expected a non-negative whole number"))?,
            );
        }
        Attribute::QpuVersion => qpu.qpu_version = Some(value.to_owned()),
        Attribute::ProcessorType => qpu.processor_type = Some(value.to_owned()),
        Attribute::Clops => {
            qpu.clops = Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
        Attribute::ReadoutErrorMedian => {
            qpu.readout_error_median =
                Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
        Attribute::SxErrorMedian => {
            qpu.sx_error_median = Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
        Attribute::CzErrorMedian => {
            qpu.cz_error_median = Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
        Attribute::T1MedianUs => {
            qpu.t1_median_us = Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
        Attribute::T2MedianUs => {
            qpu.t2_median_us = Some(value.parse().map_err(|_| invalid("expected a number"))?);
        }
    }

    Ok(())
}

/// The argument list was not acceptable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsError {
    /// A token was not `key=value`.
    NotAnAssignment {
        /// The token.
        argument: String,
    },
    /// An unrecognised key.
    UnknownKey {
        /// The key.
        key: String,
        /// Optional advice.
        hint: Option<String>,
    },
    /// An unrecognised `qpu.<name>`.
    UnknownAttribute {
        /// The attribute name.
        name: String,
    },
    /// `file=` was not given.
    MissingFile,
    /// A value could not be parsed, or failed validation.
    InvalidValue {
        /// Which attribute.
        attribute: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The selection policy does not exist.
    UnknownSelector {
        /// The name given.
        name: String,
    },
    /// Neither or both of `device=` and `qpu.*` were given.
    DeviceXorQpu,
    /// `selector=` was given alongside `device=`, which ignores it.
    SelectorWithDevice {
        /// The selector name given.
        name: String,
    },
}

impl fmt::Display for ArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnAssignment { argument } => {
                write!(f, "not a key=value argument: {argument:?}")
            }
            Self::UnknownKey { key, hint } => {
                write!(f, "unknown argument: {key:?}")?;
                if let Some(hint) = hint {
                    write!(f, " ({hint})")?;
                }
                Ok(())
            }
            Self::UnknownAttribute { name } => {
                write!(f, "unknown QPU attribute: qpu.{name}; expected one of: ")?;
                let mut names = Attribute::NAMES.iter();
                if let Some(first) = names.next() {
                    write!(f, "{first}")?;
                }
                for name in names {
                    write!(f, ", {name}")?;
                }
                Ok(())
            }
            Self::MissingFile => f.write_str("missing mandatory argument(s): file"),
            Self::InvalidValue { attribute, reason } => write!(f, "{attribute} {reason}"),
            Self::UnknownSelector { name } => {
                write!(f, "invalid selector {name:?}; expected one of: ")?;
                let mut names = lsfq_core::qpu_select::SELECTOR_NAMES.iter();
                if let Some(first) = names.next() {
                    write!(f, "{first}")?;
                }
                for name in names {
                    write!(f, ", {name}")?;
                }
                Ok(())
            }
            Self::DeviceXorQpu => {
                f.write_str("specify exactly one of device=<name> or qpu.<attribute>=<value>")
            }
            Self::SelectorWithDevice { name } => write!(
                f,
                "selector={name} cannot be combined with device=<name>: \
                 an explicit device is used as given, so the selector would be ignored"
            ),
        }
    }
}

impl std::error::Error for ArgsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_readme_examples() {
        // bsub -a "qrmi(file=.env, qpu.qubits=120,qpu.processor_type=Nighthawk)"
        let request = parse([
            "file=.env",
            "qpu.qubits=120",
            "qpu.processor_type=Nighthawk",
        ])
        .expect("valid");
        assert_eq!(request.file, PathBuf::from(".env"));
        assert_eq!(request.selector, "basic");
        assert_eq!(request.device, None);
        let qpu = request.qpu.expect("attributes");
        assert_eq!(qpu.qubits, Some(120));
        assert_eq!(qpu.processor_type.as_deref(), Some("Nighthawk"));

        // bsub -a "qrmi(file=.env, device=ibm_marrakesh)"
        let request = parse(["file=.env", "device=ibm_marrakesh"]).expect("valid");
        assert_eq!(request.device.as_deref(), Some("ibm_marrakesh"));
        assert_eq!(request.qpu, None);

        // bsub -a "qrmi(file=.env, qpu.qubits=120,selector=priority)"
        let request = parse(["file=.env", "qpu.qubits=120", "selector=priority"]).expect("valid");
        assert_eq!(request.selector, "priority");
    }

    #[test]
    fn selector_defaults_to_basic() {
        let request = parse(["file=.env", "qpu.qubits=1"]).expect("valid");
        assert_eq!(request.selector, "basic");
    }

    #[test]
    fn all_three_selectors_are_accepted() {
        for name in ["basic", "health", "priority"] {
            let request =
                parse(["file=.env", "qpu.qubits=1", &format!("selector={name}")]).expect("valid");
            assert_eq!(request.selector, name);
        }
    }

    #[test]
    fn every_documented_attribute_parses() {
        let request = parse([
            "file=.env",
            "qpu.qubits=128",
            "qpu.qpu_version=1.3.29",
            "qpu.processor_type=Heron",
            "qpu.clops=240000",
            "qpu.pending_jobs=100",
            "qpu.readout_error_median=0.01",
            "qpu.sx_error_median=0.0003",
            "qpu.cz_error_median=0.002",
            "qpu.T1_median_us=150",
            "qpu.T2_median_us=120",
        ])
        .expect("valid");

        let qpu = request.qpu.expect("attributes");
        assert_eq!(qpu.qubits, Some(128));
        assert_eq!(qpu.qpu_version.as_deref(), Some("1.3.29"));
        assert_eq!(qpu.clops, Some(240_000.0));
        assert_eq!(qpu.pending_jobs, Some(100));
        assert_eq!(qpu.t1_median_us, Some(150.0));
        assert_eq!(qpu.t2_median_us, Some(120.0));
    }

    /// The help text and README advertise `qpu.t1_median_us` in lower case
    /// while the dataclass field is `T1_median_us`; both must work.
    #[test]
    fn attribute_names_accept_either_documented_spelling() {
        for name in ["qpu.T1_median_us=150", "qpu.t1_median_us=150"] {
            let request = parse(["file=.env", name]).expect(name);
            assert_eq!(request.qpu.expect("attributes").t1_median_us, Some(150.0));
        }
    }

    // ---- the mutual-exclusion rule ----

    #[test]
    fn device_and_qpu_together_are_rejected() {
        assert_eq!(
            parse(["file=.env", "device=ibm_fez", "qpu.qubits=120"]).expect_err("both"),
            ArgsError::DeviceXorQpu
        );
    }

    #[test]
    fn neither_device_nor_qpu_is_rejected() {
        assert_eq!(
            parse(["file=.env"]).expect_err("neither"),
            ArgsError::DeviceXorQpu
        );
    }

    #[test]
    fn selector_with_device_is_rejected() {
        // The reported case: both were accepted, and the selector was then
        // silently dropped by the jobstarter's explicit-device path.
        let error = parse(["file=.env", "device=ibm_marrakesh", "selector=health"])
            .expect_err("device + selector");

        assert_eq!(
            error,
            ArgsError::SelectorWithDevice {
                name: "health".to_owned()
            }
        );

        let message = error.to_string();
        assert!(message.contains("health"), "{message}");
        assert!(message.contains("device="), "{message}");
    }

    #[test]
    fn every_selector_is_rejected_alongside_device() {
        for name in lsfq_core::qpu_select::SELECTOR_NAMES {
            let error = parse(["file=.env", "device=ibm_fez", &format!("selector={name}")])
                .expect_err(name);
            assert_eq!(
                error,
                ArgsError::SelectorWithDevice {
                    name: name.to_owned()
                }
            );
        }
    }

    /// An explicit `selector=basic` is rejected too: it is the default value,
    /// but the user did ask for it, and silently ignoring it is the bug.
    #[test]
    fn explicit_default_selector_with_device_is_still_rejected() {
        assert_eq!(
            parse(["file=.env", "device=ibm_fez", "selector=basic"]).expect_err("explicit basic"),
            ArgsError::SelectorWithDevice {
                name: "basic".to_owned()
            }
        );
    }

    /// The default must not trip the new rule, or every device= submission
    /// that worked before would start failing.
    #[test]
    fn device_alone_still_parses_and_defaults_the_selector() {
        let request = parse(["file=.env", "device=ibm_marrakesh"]).expect("device alone");
        assert_eq!(request.device.as_deref(), Some("ibm_marrakesh"));
        assert_eq!(request.selector, "basic");
    }

    /// Order must not matter: the check runs after the whole list is read.
    #[test]
    fn selector_before_device_is_also_rejected() {
        assert_eq!(
            parse(["file=.env", "selector=priority", "device=ibm_fez"]).expect_err("reordered"),
            ArgsError::SelectorWithDevice {
                name: "priority".to_owned()
            }
        );
    }

    /// A bad selector name is reported as such even with device=, so the user
    /// learns about the typo rather than only about the combination.
    #[test]
    fn unknown_selector_outranks_the_device_combination() {
        assert_eq!(
            parse(["file=.env", "device=ibm_fez", "selector=nonesuch"]).expect_err("typo"),
            ArgsError::UnknownSelector {
                name: "nonesuch".to_owned()
            }
        );
    }

    /// qpu.* with a selector is the documented pairing and must keep working.
    #[test]
    fn selector_with_qpu_attributes_is_unaffected() {
        let request =
            parse(["file=.env", "qpu.qubits=120", "selector=health"]).expect("qpu + selector");
        assert_eq!(request.selector, "health");
        assert_eq!(request.device, None);
    }

    #[test]
    fn missing_file_is_rejected() {
        assert_eq!(
            parse(["qpu.qubits=120"]).expect_err("no file"),
            ArgsError::MissingFile
        );
        assert!(ArgsError::MissingFile.to_string().contains("file"));
    }

    // ---- unknown keys: set_struct(True) in the Python ----

    #[test]
    fn unknown_keys_are_rejected() {
        let error = parse(["file=.env", "qpu.qubits=1", "slector=basic"]).expect_err("typo");
        assert!(matches!(error, ArgsError::UnknownKey { .. }));
        assert!(error.to_string().contains("slector"), "{error}");
    }

    #[test]
    fn bare_qpu_key_suggests_the_dotted_form() {
        let error = parse(["file=.env", "qpu=120"]).expect_err("bare qpu");
        assert!(error.to_string().contains("qpu.<attribute>"), "{error}");
    }

    #[test]
    fn unknown_attributes_list_the_valid_names() {
        // The help text's own example of a plausible typo.
        let error = parse(["file=.env", "qpu.qbits=120"]).expect_err("typo");
        let rendered = error.to_string();
        assert!(rendered.contains("qbits"), "{rendered}");
        assert!(rendered.contains("qubits"), "{rendered}");
    }

    #[test]
    fn non_assignment_tokens_are_rejected() {
        let error = parse(["file=.env", "qpu.qubits=1", "garbage"]).expect_err("no equals");
        assert!(matches!(error, ArgsError::NotAnAssignment { .. }));
    }

    #[test]
    fn unknown_selector_is_rejected_with_the_valid_names() {
        let error =
            parse(["file=.env", "qpu.qubits=1", "selector=nonesuch"]).expect_err("bad selector");
        let rendered = error.to_string();
        assert!(rendered.contains("nonesuch"), "{rendered}");
        assert!(rendered.contains("priority"), "{rendered}");
    }

    // ---- value validation ----

    #[test]
    fn zero_qubits_is_rejected() {
        let error = parse(["file=.env", "qpu.qubits=0"]).expect_err("zero");
        assert!(error.to_string().contains("greater than zero"), "{error}");
    }

    #[test]
    fn negative_values_are_rejected() {
        // A negative qubit count fails to parse as u32 ...
        assert!(parse(["file=.env", "qpu.qubits=-1"]).is_err());
        // ... and a negative metric fails validation.
        let error = parse(["file=.env", "qpu.clops=-5"]).expect_err("negative clops");
        assert!(
            error.to_string().contains("must not be negative"),
            "{error}"
        );
    }

    #[test]
    fn non_numeric_values_are_rejected() {
        let error = parse(["file=.env", "qpu.qubits=many"]).expect_err("not a number");
        assert!(error.to_string().contains("qpu.qubits"), "{error}");

        assert!(parse(["file=.env", "qpu.clops=fast"]).is_err());
    }

    #[test]
    fn non_finite_values_are_rejected() {
        // `"inf".parse::<f64>()` succeeds, so this needs an explicit check.
        for value in ["inf", "-inf", "NaN"] {
            assert!(
                parse(["file=.env", &format!("qpu.clops={value}")]).is_err(),
                "{value} must be rejected"
            );
        }
    }

    #[test]
    fn whitespace_around_tokens_is_tolerated() {
        // LSF hands through the spaces in `qrmi(file=.env, qpu.qubits=120)`.
        let request = parse([" file=.env ", " qpu.qubits = 120 "]).expect("valid");
        assert_eq!(request.file, PathBuf::from(".env"));
        assert_eq!(request.qpu.expect("attributes").qubits, Some(120));
    }

    #[test]
    fn empty_tokens_are_ignored() {
        let request = parse(["file=.env", "", "qpu.qubits=1", "  "]).expect("valid");
        assert_eq!(request.qpu.expect("attributes").qubits, Some(1));
    }

    // ---- to_lsf_string ----

    #[test]
    fn request_string_uses_the_jobstarters_metric_names() {
        let qpu = parse(["file=.env", "qpu.qubits=128", "qpu.T1_median_us=150"])
            .expect("valid")
            .qpu
            .expect("attributes");

        assert_eq!(qpu.to_lsf_string(), "qubits=128;T1_median_us=150");
    }

    /// Every name this emits must be one the jobstarter's parser accepts,
    /// or the request would be rejected on the execution host -- after the
    /// job was already admitted.
    #[test]
    fn every_emitted_name_is_parseable_by_the_selector() {
        let qpu = QpuAttributes {
            qubits: Some(128),
            qpu_version: Some("1.3".to_owned()),
            processor_type: Some("Heron".to_owned()),
            clops: Some(240_000.0),
            pending_jobs: Some(100),
            readout_error_median: Some(0.01),
            sx_error_median: Some(0.0003),
            cz_error_median: Some(0.002),
            t1_median_us: Some(150.0),
            t2_median_us: Some(120.0),
        };

        let rendered = qpu.to_lsf_string();
        let parsed = lsfq_core::qpu_select::Request::parse(&rendered)
            .expect("the jobstarter must accept what the esub emits");

        assert_eq!(parsed.requirements().len(), 10);
    }

    /// Pins the field-order normalisation described on `to_lsf_string`.
    /// Changing this changes which backend `selector=priority` picks.
    #[test]
    fn cli_order_does_not_affect_the_request_string() {
        let first = parse(["file=.env", "qpu.clops=240000", "qpu.qubits=120"])
            .expect("valid")
            .qpu
            .expect("attributes")
            .to_lsf_string();

        let second = parse(["file=.env", "qpu.qubits=120", "qpu.clops=240000"])
            .expect("valid")
            .qpu
            .expect("attributes")
            .to_lsf_string();

        assert_eq!(first, second);
        // Declaration order, not CLI order: qubits precedes clops.
        assert_eq!(first, "qubits=120;clops=240000");
    }

    #[test]
    fn integral_numbers_lose_the_trailing_zero() {
        let qpu = parse(["file=.env", "qpu.clops=240000"])
            .expect("valid")
            .qpu
            .expect("attributes");

        // Not "240000.0": both parse the same, but this is what was typed.
        assert_eq!(qpu.to_lsf_string(), "clops=240000");
    }

    #[test]
    fn fractional_numbers_are_preserved() {
        let qpu = parse(["file=.env", "qpu.readout_error_median=0.0094"])
            .expect("valid")
            .qpu
            .expect("attributes");

        assert_eq!(qpu.to_lsf_string(), "readout_error_median=0.0094");
    }

    #[test]
    fn empty_attributes_render_as_an_empty_string() {
        assert_eq!(QpuAttributes::default().to_lsf_string(), "");
        assert!(QpuAttributes::default().is_empty());
    }

    /// A value containing a comma could never arrive (LSF splits on commas),
    /// but a semicolon could -- and would corrupt the request string.
    #[test]
    fn a_semicolon_in_a_value_is_visible_in_the_output() {
        let qpu = parse(["file=.env", "qpu.processor_type=a;b"])
            .expect("parses")
            .qpu
            .expect("attributes");

        // Documented rather than silently accepted: the jobstarter would
        // read this as two requirements. The esub writer rejects the
        // directive separately if it cannot be represented.
        assert_eq!(qpu.to_lsf_string(), "processor_type=a;b");
    }
}
