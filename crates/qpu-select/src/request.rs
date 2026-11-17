// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The request grammar: `qubits=120;processor_type=Nighthawk;clops=240000`.
//!
//! One parser, replacing the `parse_value` / `parse_requirement` /
//! `normalize` / `is_number` closures nested inside the Python
//! `select_device_priority`, and the parallel module-level implementation
//! (`REQUIREMENT_PATTERN`, `_parse_requirements`, ...) that shadowing left
//! unreachable.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::metrics::{Direction, Metric, MetricValue};

/// How a requirement compares the reported value against the requested one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Comparison {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Strictly greater.
    Gt,
    /// Greater or equal.
    Ge,
    /// Strictly less.
    Lt,
    /// Less or equal.
    Le,
}

impl Comparison {
    /// The operator as written in a request.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    /// Apply this comparison to an [`Ordering`](core::cmp::Ordering).
    const fn admits(self, ordering: core::cmp::Ordering) -> bool {
        use core::cmp::Ordering::{Equal, Greater, Less};
        matches!(
            (self, ordering),
            (Self::Eq, Equal)
                | (Self::Ne, Less | Greater)
                | (Self::Gt, Greater)
                | (Self::Ge, Greater | Equal)
                | (Self::Lt, Less)
                | (Self::Le, Less | Equal)
        )
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value as written on the right-hand side of a requirement.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestedValue {
    /// A number.
    Number(f64),
    /// A quoted or bare string.
    Text(String),
    /// A boolean (`true` / `false`).
    Boolean(bool),
    /// An explicit `none` / `null`.
    Null,
}

impl RequestedValue {
    /// Parse the right-hand side of a requirement.
    ///
    /// Order matters and follows the Python `parse_value`: quoted strings
    /// keep their contents verbatim, then `none`/`null`, then booleans, then
    /// integer, then float, then bare text.
    fn parse(text: &str) -> Self {
        let text = text.trim();

        // A quoted value is taken literally, so `processor_type="true"`
        // compares against the string rather than a boolean.
        let mut chars = text.chars();
        if text.len() >= 2 {
            let first = chars.next();
            let last = chars.next_back();
            if first == last && matches!(first, Some('\'' | '"')) {
                return Self::Text(text[1..text.len() - 1].to_owned());
            }
        }

        let lowered = text.to_ascii_lowercase();
        match lowered.as_str() {
            "none" | "null" => return Self::Null,
            "true" => return Self::Boolean(true),
            "false" => return Self::Boolean(false),
            _ => {}
        }

        // Python tries int() then float(); both land in f64 here. Rust's
        // parser additionally accepts "inf" and "NaN", which Python's float()
        // also accepts, so the behaviour matches -- but a NaN threshold can
        // never compare true, which is the same outcome as Python.
        if let Ok(number) = text.parse::<f64>() {
            return Self::Number(number);
        }

        Self::Text(text.to_owned())
    }

    /// Whether this is a number, used to decide how a bare `=` is widened.
    const fn is_number(&self) -> bool {
        matches!(self, Self::Number(_))
    }
}

impl fmt::Display for RequestedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(t) => f.write_str(t),
            Self::Boolean(b) => write!(f, "{b}"),
            Self::Null => f.write_str("none"),
        }
    }
}

/// One parsed requirement.
#[derive(Debug, Clone, PartialEq)]
pub struct Requirement {
    /// The metric being constrained.
    pub metric: Metric,
    /// A sub-field, for dotted paths such as `processor_type.revision`.
    pub field: Option<String>,
    /// The comparison to apply.
    pub comparison: Comparison,
    /// The requested value.
    pub value: RequestedValue,
}

impl Requirement {
    /// Whether a backend's reported value satisfies this requirement.
    ///
    /// Reproduces the Python `satisfies()` semantics deliberately:
    ///
    /// * A missing reported value satisfies only `== none` (and `!=` against
    ///   a present value); every ordering comparison against a missing value
    ///   is false, so a backend that does not report a metric is filtered
    ///   out rather than ranked.
    /// * Numbers compare numerically.
    /// * Strings compare case-insensitively.
    /// * A type mismatch (number against text) is false rather than an
    ///   error, matching Python's `except TypeError: return False`.
    #[must_use]
    // The arms are kept separate so each case carries its own explanation.
    #[allow(clippy::match_same_arms)]
    pub fn is_satisfied_by(&self, reported: Option<&MetricValue>) -> bool {
        match (reported, &self.value) {
            // Both absent: equal.
            (None, RequestedValue::Null) => {
                matches!(
                    self.comparison,
                    Comparison::Eq | Comparison::Ge | Comparison::Le
                )
            }
            // Exactly one side absent: they are unequal, so only `!=`
            // holds and every ordering comparison is false.
            (None, _) | (Some(_), RequestedValue::Null) => self.comparison == Comparison::Ne,

            (Some(MetricValue::Number(actual)), RequestedValue::Number(required)) => {
                // partial_cmp is None only for NaN, where every comparison is
                // false except `!=`, matching IEEE-754 and Python.
                actual
                    .partial_cmp(required)
                    .map_or(self.comparison == Comparison::Ne, |ordering| {
                        self.comparison.admits(ordering)
                    })
            }

            (Some(MetricValue::Text(actual)), RequestedValue::Text(required)) => {
                let ordering = actual.to_lowercase().cmp(&required.to_lowercase());
                self.comparison.admits(ordering)
            }

            // Mixed kinds never satisfy an ordering, and are unequal.
            (Some(_), _) => self.comparison == Comparison::Ne,
        }
    }
}

/// A parsed user request: an ordered list of requirements.
///
/// Order is significant for the `priority` selector, which applies
/// requirements in the order written and ranks on them lexicographically.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Request {
    requirements: Vec<Requirement>,
}

impl Request {
    /// Parse a request string.
    ///
    /// Requirements are separated by `;`. Empty segments are skipped, so a
    /// trailing separator is accepted.
    ///
    /// A bare `=` is widened by the metric's [`Direction`]: `>=` for
    /// maximize-metrics and `<=` for minimize-metrics, both only when the
    /// requested value is numeric; otherwise `==`. This is the rule the
    /// README documents, and which the Python code expresses through set
    /// membership tests in three places.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] for an unknown metric, a malformed segment, or
    /// a request with no requirements at all.
    ///
    /// # Example
    ///
    /// ```
    /// use qpu_select::{Comparison, Metric, Request};
    ///
    /// let request = Request::parse("qubits=120;pending_jobs=10")?;
    /// let reqs = request.requirements();
    ///
    /// // qubits is a maximize-metric, so bare `=` means `>=`
    /// assert_eq!(reqs[0].comparison, Comparison::Ge);
    /// // pending_jobs is a minimize-metric, so bare `=` means `<=`
    /// assert_eq!(reqs[1].comparison, Comparison::Le);
    /// # Ok::<(), qpu_select::ParseError>(())
    /// ```
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let mut requirements = Vec::new();

        for segment in input.split(';') {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }
            requirements.push(Self::parse_requirement(segment)?);
        }

        if requirements.is_empty() {
            return Err(ParseError::Empty);
        }

        Ok(Self { requirements })
    }

    fn parse_requirement(segment: &str) -> Result<Requirement, ParseError> {
        // Longest operators first: `>=` must win over `>`, and `==` over `=`.
        const OPERATORS: [(&str, Option<Comparison>); 7] = [
            (">=", Some(Comparison::Ge)),
            ("<=", Some(Comparison::Le)),
            ("==", Some(Comparison::Eq)),
            ("!=", Some(Comparison::Ne)),
            (">", Some(Comparison::Gt)),
            ("<", Some(Comparison::Lt)),
            ("=", None), // bare `=`: widened below
        ];

        for (token, comparison) in OPERATORS {
            let Some(position) = segment.find(token) else {
                continue;
            };

            let (attribute, rest) = segment.split_at(position);
            let raw_value = &rest[token.len()..];

            let attribute = attribute.trim();
            let raw_value = raw_value.trim();

            if attribute.is_empty() {
                return Err(ParseError::MissingAttribute {
                    segment: segment.to_owned(),
                });
            }
            if raw_value.is_empty() {
                return Err(ParseError::MissingValue {
                    segment: segment.to_owned(),
                });
            }

            let (name, field) = match attribute.split_once('.') {
                Some((head, tail)) => (head, Some(tail.to_owned())),
                None => (attribute, None),
            };

            let metric = Metric::from_name(name).ok_or_else(|| ParseError::UnknownMetric {
                name: name.to_owned(),
            })?;

            let value = RequestedValue::parse(raw_value);

            let comparison = comparison.unwrap_or_else(|| {
                // Widen a bare `=` only for a numeric threshold on a ranked
                // metric; a textual value always means equality.
                match (metric.direction(), value.is_number()) {
                    (Some(Direction::Maximize), true) => Comparison::Ge,
                    (Some(Direction::Minimize), true) => Comparison::Le,
                    _ => Comparison::Eq,
                }
            });

            return Ok(Requirement {
                metric,
                field,
                comparison,
                value,
            });
        }

        Err(ParseError::MissingOperator {
            segment: segment.to_owned(),
        })
    }

    /// The requirements, in the order written.
    #[must_use]
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Whether the request has no requirements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requirements.is_empty()
    }

    /// The qubit floor, if the request constrains qubits.
    ///
    /// Used by the `basic` and `health` selectors, which rank on queue depth
    /// and health respectively but filter on a qubit minimum. Replaces the
    /// Python `extract_qubits()`, which parsed the request string a second
    /// time with its own ad-hoc splitting.
    #[must_use]
    pub fn required_qubits(&self) -> Option<u32> {
        self.requirements
            .iter()
            .find(|requirement| requirement.metric == Metric::Qubits)
            .and_then(|requirement| match requirement.value {
                RequestedValue::Number(n) if n.is_finite() && n >= 0.0 => ceil_to_u32(n),
                _ => None,
            })
    }
}

/// Round a non-negative finite float up to a `u32`, saturating.
///
/// `f64::ceil` is provided by `std`, not `core`, so it is unavailable in this
/// `no_std` crate; integer arithmetic gives the same result for the range
/// that matters here. A fractional qubit floor such as `127.5` means "at
/// least 128".
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless
)]
fn ceil_to_u32(value: f64) -> Option<u32> {
    if value > f64::from(u32::MAX) {
        return Some(u32::MAX);
    }
    let truncated = value as u32;
    if f64::from(truncated) < value {
        truncated.checked_add(1)
    } else {
        Some(truncated)
    }
}

/// A request that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The request contained no requirements.
    Empty,
    /// A segment had no recognised comparison operator.
    MissingOperator {
        /// The offending segment.
        segment: String,
    },
    /// A segment had an operator but no attribute before it.
    MissingAttribute {
        /// The offending segment.
        segment: String,
    },
    /// A segment had an operator but no value after it.
    MissingValue {
        /// The offending segment.
        segment: String,
    },
    /// The attribute did not name a known metric.
    UnknownMetric {
        /// The unrecognised name.
        name: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the request contains no requirements"),
            Self::MissingOperator { segment } => {
                write!(f, "no comparison operator in requirement: {segment:?}")
            }
            Self::MissingAttribute { segment } => {
                write!(f, "missing attribute in requirement: {segment:?}")
            }
            Self::MissingValue { segment } => {
                write!(f, "missing value in requirement: {segment:?}")
            }
            Self::UnknownMetric { name } => {
                write!(f, "unknown metric {name:?}; expected one of: ")?;
                let mut names = Metric::ALL.iter();
                if let Some(first) = names.next() {
                    write!(f, "{first}")?;
                }
                for metric in names {
                    write!(f, ", {metric}")?;
                }
                Ok(())
            }
        }
    }
}

impl core::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn only(input: &str) -> Requirement {
        let request = Request::parse(input).expect("parses");
        assert_eq!(request.requirements().len(), 1);
        request.requirements()[0].clone()
    }

    #[test]
    fn bare_equals_widens_by_direction() {
        // These four cases are the documented contract in README.md.
        assert_eq!(only("qubits=120").comparison, Comparison::Ge);
        assert_eq!(only("clops=240000").comparison, Comparison::Ge);
        assert_eq!(only("T1_median_us=100").comparison, Comparison::Ge);
        assert_eq!(only("pending_jobs=10").comparison, Comparison::Le);
        assert_eq!(only("readout_error_median=0.01").comparison, Comparison::Le);
    }

    #[test]
    fn bare_equals_on_text_means_equality() {
        let requirement = only("processor_type=Nighthawk");
        assert_eq!(requirement.comparison, Comparison::Eq);
        assert_eq!(
            requirement.value,
            RequestedValue::Text("Nighthawk".to_owned())
        );
    }

    /// A non-numeric value on a ranked metric must not widen to `>=`, or a
    /// typo would turn into a nonsensical ordering comparison.
    #[test]
    fn bare_equals_on_ranked_metric_with_text_is_equality() {
        assert_eq!(only("qubits=many").comparison, Comparison::Eq);
    }

    #[test]
    fn explicit_operators_are_preserved() {
        assert_eq!(only("qubits>=120").comparison, Comparison::Ge);
        assert_eq!(only("qubits>120").comparison, Comparison::Gt);
        assert_eq!(only("qubits<=120").comparison, Comparison::Le);
        assert_eq!(only("qubits<120").comparison, Comparison::Lt);
        assert_eq!(only("qubits==120").comparison, Comparison::Eq);
        assert_eq!(only("qubits!=120").comparison, Comparison::Ne);
    }

    /// `>=` must not be parsed as `>` followed by a value of `=120`.
    #[test]
    fn longest_operator_wins() {
        let requirement = only("qubits>=120");
        assert_eq!(requirement.comparison, Comparison::Ge);
        assert_eq!(requirement.value, RequestedValue::Number(120.0));
    }

    #[test]
    fn order_is_preserved() {
        let request = Request::parse("clops=1;qubits=2;pending_jobs=3").expect("parses");
        let metrics: Vec<_> = request.requirements().iter().map(|r| r.metric).collect();
        assert_eq!(
            metrics,
            vec![Metric::Clops, Metric::Qubits, Metric::PendingJobs]
        );
    }

    #[test]
    fn whitespace_and_trailing_separators_are_tolerated() {
        let request = Request::parse("  qubits = 120 ; clops=3 ; ").expect("parses");
        assert_eq!(request.requirements().len(), 2);
        assert_eq!(
            request.requirements()[0].value,
            RequestedValue::Number(120.0)
        );
    }

    #[test]
    fn dotted_path_is_split() {
        let requirement = only("processor_type.revision=2");
        assert_eq!(requirement.metric, Metric::ProcessorType);
        assert_eq!(requirement.field.as_deref(), Some("revision"));
    }

    #[test]
    fn quoted_values_are_literal() {
        assert_eq!(
            only("processor_type='true'").value,
            RequestedValue::Text("true".to_owned())
        );
        assert_eq!(
            only("qpu_version=\"1.3\"").value,
            RequestedValue::Text("1.3".to_owned())
        );
    }

    #[test]
    fn null_and_boolean_values_parse() {
        assert_eq!(only("processor_type=none").value, RequestedValue::Null);
        assert_eq!(only("processor_type=NULL").value, RequestedValue::Null);
    }

    #[test]
    fn errors_are_reported() {
        assert_eq!(Request::parse(""), Err(ParseError::Empty));
        assert_eq!(Request::parse("   ;  "), Err(ParseError::Empty));
        assert!(matches!(
            Request::parse("qbits=120"),
            Err(ParseError::UnknownMetric { .. })
        ));
        assert!(matches!(
            Request::parse("qubits"),
            Err(ParseError::MissingOperator { .. })
        ));
        assert!(matches!(
            Request::parse("=120"),
            Err(ParseError::MissingAttribute { .. })
        ));
        assert!(matches!(
            Request::parse("qubits="),
            Err(ParseError::MissingValue { .. })
        ));
    }

    #[test]
    fn unknown_metric_error_lists_valid_names() {
        let error = Request::parse("qbits=1").unwrap_err();
        let rendered = error.to_string();
        assert!(
            rendered.contains("qbits"),
            "names the bad metric: {rendered}"
        );
        assert!(
            rendered.contains("qubits"),
            "suggests valid ones: {rendered}"
        );
    }

    #[test]
    fn required_qubits_is_extracted() {
        assert_eq!(
            Request::parse("qubits=128").unwrap().required_qubits(),
            Some(128)
        );
        assert_eq!(
            Request::parse("clops=1;qubits=64")
                .unwrap()
                .required_qubits(),
            Some(64)
        );
        assert_eq!(Request::parse("clops=1").unwrap().required_qubits(), None);
        // A fractional floor rounds up: 127.5 qubits means at least 128.
        assert_eq!(
            Request::parse("qubits=127.5").unwrap().required_qubits(),
            Some(128)
        );
    }

    #[test]
    fn satisfaction_on_numbers() {
        let requirement = only("qubits>=128");
        assert!(requirement.is_satisfied_by(Some(&MetricValue::Number(156.0))));
        assert!(requirement.is_satisfied_by(Some(&MetricValue::Number(128.0))));
        assert!(!requirement.is_satisfied_by(Some(&MetricValue::Number(127.0))));
    }

    #[test]
    fn satisfaction_on_text_is_case_insensitive() {
        let requirement = only("processor_type=heron");
        assert!(requirement.is_satisfied_by(Some(&MetricValue::Text("Heron".to_owned()))));
        assert!(!requirement.is_satisfied_by(Some(&MetricValue::Text("Eagle".to_owned()))));
    }

    /// A backend that does not report the metric must be filtered out, not
    /// treated as satisfying the bound.
    #[test]
    fn missing_value_fails_ordering_comparisons() {
        let requirement = only("qubits>=128");
        assert!(!requirement.is_satisfied_by(None));

        let requirement = only("readout_error_median<=0.01");
        assert!(!requirement.is_satisfied_by(None));
    }

    #[test]
    fn missing_value_matches_explicit_none() {
        let requirement = only("processor_type==none");
        assert!(requirement.is_satisfied_by(None));

        let requirement = only("processor_type!=none");
        assert!(requirement.is_satisfied_by(Some(&MetricValue::Text("Heron".to_owned()))));
        assert!(!requirement.is_satisfied_by(None));
    }

    #[test]
    fn mismatched_kinds_do_not_satisfy() {
        let requirement = only("qubits>=128");
        assert!(!requirement.is_satisfied_by(Some(&MetricValue::Text("many".to_owned()))));
    }

    #[test]
    fn nan_threshold_never_satisfies() {
        let requirement = only("clops>=NaN");
        assert!(!requirement.is_satisfied_by(Some(&MetricValue::Number(1.0))));
    }
}
