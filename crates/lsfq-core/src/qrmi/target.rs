// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Parsing a QRMI `target()` document into metrics.
//!
//! QRMI's `Target` carries a single `value: String` holding serialized JSON:
//! the backend's configuration and calibration properties, passed through
//! from the vendor service verbatim. This module turns that into a
//! [`QpuMetrics`], which is what the selection policies consume, and into
//! [`SystemMedians`] for the ELIM.
//!
//! It replaces `extract_qubit_metrics`, `extract_gate_errors`,
//! `extract_clops`, `extract_parameter_value`, `convert_time_to_us`,
//! `extract_processor_type`, `normalize_value` and `safe_median` from the
//! Python implementation -- eight helpers, four of which are defined twice
//! there, with the second silently shadowing the first at import.
//!
//! This module parses JSON only; it does not call QRMI, so it builds and
//! tests without the `qrmi` feature.

use qpu_select::{ProcessorType, QpuMetrics};
use serde_json::Value;

/// The medians the ELIM reports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemMedians {
    /// Median readout error across qubits.
    pub readout_error_median: Option<f64>,
    /// Median T1, microseconds.
    pub t1_median_us: Option<f64>,
    /// Median T2, microseconds.
    pub t2_median_us: Option<f64>,
    /// Median SX-gate error.
    pub sx_error_median: Option<f64>,
    /// Median CZ-gate error.
    pub cz_error_median: Option<f64>,
}

/// A parsed `target()` document.
#[derive(Debug, Clone, Default)]
pub struct Target {
    configuration: Value,
    properties: Value,
}

impl Target {
    /// Parse a `target()` payload.
    ///
    /// # Errors
    ///
    /// Returns [`serde_json::Error`] if the payload is not valid JSON. A
    /// payload that is valid JSON but missing the `configuration` or
    /// `properties` objects is accepted, with the absent parts yielding
    /// `None` metrics -- the Python does the same via
    /// `target.get("configuration") or {}`, and a partial document is a
    /// transient service condition rather than a bug to fail on.
    pub fn parse(payload: &str) -> Result<Self, serde_json::Error> {
        let document: Value = serde_json::from_str(payload)?;

        Ok(Self {
            configuration: document
                .get("configuration")
                .cloned()
                .unwrap_or(Value::Null),
            properties: document.get("properties").cloned().unwrap_or(Value::Null),
        })
    }

    /// The configuration object.
    #[must_use]
    pub const fn configuration(&self) -> &Value {
        &self.configuration
    }

    /// The properties (calibration) object.
    #[must_use]
    pub const fn properties(&self) -> &Value {
        &self.properties
    }

    /// Qubit count, from `n_qubits`.
    #[must_use]
    pub fn qubits(&self) -> Option<u32> {
        number(&self.configuration, "n_qubits").and_then(|value| {
            if value.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&value) {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some(value as u32)
            } else {
                None
            }
        })
    }

    /// Vendor version string, from `backend_version`.
    #[must_use]
    pub fn qpu_version(&self) -> Option<String> {
        text(&self.configuration, "backend_version")
    }

    /// CLOPS.
    ///
    /// Three spellings are accepted, in the Python's order: `clops` (which
    /// in the backend-list representation is an object with a `value` key),
    /// then the scalars `clops_h` and `clops_v`.
    #[must_use]
    pub fn clops(&self) -> Option<f64> {
        for key in ["clops", "clops_h", "clops_v"] {
            let Some(value) = self.configuration.get(key) else {
                continue;
            };

            // The object form: {"type": "hardware", "value": 12345}.
            let candidate = if value.is_object() {
                value.get("value")?
            } else {
                value
            };

            if let Some(number) = as_number(candidate) {
                return Some(number);
            }
        }

        None
    }

    /// Processor family and revision.
    ///
    /// Two representations are accepted:
    ///
    /// * a `processor_type` object, `{"family": "Heron", "revision": "2"}`;
    /// * a `sample_name` string, `"family: Heron, revision: 2"`.
    ///
    /// A revision may be published as a number or a string; both normalise
    /// to a string here, since it is compared textually.
    #[must_use]
    pub fn processor_type(&self) -> Option<ProcessorType> {
        if let Some(object) = self.configuration.get("processor_type") {
            if let Some(family) = object.get("family").and_then(scalar_to_string) {
                let revision = object.get("revision").and_then(scalar_to_string);
                return Some(ProcessorType { family, revision });
            }
        }

        let sample_name = text(&self.configuration, "sample_name")?;
        parse_sample_name(&sample_name)
    }

    /// The calibration medians.
    #[must_use]
    pub fn medians(&self) -> SystemMedians {
        let (readout, t1, t2) = self.qubit_metrics();

        SystemMedians {
            readout_error_median: median(readout),
            t1_median_us: median(t1),
            t2_median_us: median(t2),
            sx_error_median: median(self.gate_errors("sx")),
            cz_error_median: median(self.gate_errors("cz")),
        }
    }

    /// Readout errors, T1 and T2 values across qubits.
    ///
    /// The properties schema nests one list of parameter objects per qubit:
    ///
    /// ```text
    /// "qubits": [[{"name": "T1", "value": 141.2, "unit": "us"}, ...], ...]
    /// ```
    ///
    /// Times are normalised to microseconds via [`convert_time_to_us`]. A
    /// value whose unit is absent or unrecognised is skipped rather than
    /// assumed, because guessing a unit would silently scale a coherence
    /// time by a factor of a thousand.
    #[must_use]
    fn qubit_metrics(&self) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut readout_errors = Vec::new();
        let mut t1_values = Vec::new();
        let mut t2_values = Vec::new();

        let Some(qubits) = self.properties.get("qubits").and_then(Value::as_array) else {
            return (readout_errors, t1_values, t2_values);
        };

        for qubit in qubits {
            let Some(parameters) = qubit.as_array() else {
                continue;
            };

            for parameter in parameters {
                let Some(name) = parameter.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(value) = parameter.get("value").and_then(as_number) else {
                    continue;
                };

                let unit = parameter.get("unit").and_then(Value::as_str);

                // The properties API publishes T1/T2 uppercase and
                // readout_error lowercase; accept either case for both.
                match name.trim().to_ascii_lowercase().as_str() {
                    "readout_error" | "readout error" => readout_errors.push(value),
                    "t1" => {
                        if let Some(microseconds) = convert_time_to_us(value, unit) {
                            t1_values.push(microseconds);
                        }
                    }
                    "t2" => {
                        if let Some(microseconds) = convert_time_to_us(value, unit) {
                            t2_values.push(microseconds);
                        }
                    }
                    _ => {}
                }
            }
        }

        (readout_errors, t1_values, t2_values)
    }

    /// Every calibrated error for one gate.
    #[must_use]
    fn gate_errors(&self, gate_name: &str) -> Vec<f64> {
        let Some(gates) = self.properties.get("gates").and_then(Value::as_array) else {
            return Vec::new();
        };

        gates
            .iter()
            .filter(|gate| {
                gate.get("gate")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(gate_name))
            })
            .filter_map(|gate| {
                gate.get("parameters")?
                    .as_array()?
                    .iter()
                    .find_map(|parameter| {
                        let name = parameter.get("name")?.as_str()?;
                        if name.eq_ignore_ascii_case("gate_error") {
                            parameter.get("value").and_then(as_number)
                        } else {
                            None
                        }
                    })
            })
            .collect()
    }

    /// Build the metrics the selection policies consume.
    ///
    /// `pending_jobs` and `available` come from the separate `status()`
    /// document, so they are passed in.
    #[must_use]
    pub fn to_metrics(&self, pending_jobs: Option<u64>, available: bool) -> QpuMetrics {
        let medians = self.medians();

        QpuMetrics {
            qubits: self.qubits(),
            qpu_version: self.qpu_version(),
            processor_type: self.processor_type(),
            clops: self.clops(),
            pending_jobs,
            readout_error_median: medians.readout_error_median,
            sx_error_median: medians.sx_error_median,
            cz_error_median: medians.cz_error_median,
            t1_median_us: medians.t1_median_us,
            t2_median_us: medians.t2_median_us,
            available,
        }
    }
}

/// Convert a calibration time to microseconds.
///
/// Recognised units are `s`, `ms`, `us`, `µs` (U+00B5), `μs` (U+03BC) and
/// `ns`. An absent or unrecognised unit yields `None` rather than a guess.
#[must_use]
pub fn convert_time_to_us(value: f64, unit: Option<&str>) -> Option<f64> {
    let unit = unit?;

    // Both the micro sign and the Greek mu appear in real payloads.
    let normalised = unit.trim().to_ascii_lowercase().replace(['µ', 'μ'], "u");

    let multiplier = match normalised.as_str() {
        "s" => 1_000_000.0,
        "ms" => 1_000.0,
        "us" => 1.0,
        "ns" => 0.001,
        _ => return None,
    };

    Some(value * multiplier)
}

/// Parse a `sample_name` string into a processor type.
///
/// Accepts `"family: Heron, revision: 2"`, and tolerates a missing
/// revision.
#[must_use]
pub fn parse_sample_name(sample_name: &str) -> Option<ProcessorType> {
    let mut family = None;
    let mut revision = None;

    for part in sample_name.split(',') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };

        match key.trim().to_ascii_lowercase().as_str() {
            "family" => family = Some(value.trim().to_owned()),
            "revision" => revision = Some(value.trim().to_owned()),
            _ => {}
        }
    }

    family.map(|family| ProcessorType { family, revision })
}

/// The median of a set of samples, or `None` if there are none.
///
/// Matches Python's `statistics.median`: for an even count it averages the
/// two central values rather than taking the lower.
#[must_use]
pub fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|value| value.is_finite());

    if values.is_empty() {
        return None;
    }

    values.sort_unstable_by(f64::total_cmp);

    let middle = values.len() / 2;

    Some(if values.len().is_multiple_of(2) {
        // `f64::midpoint` would read better but is stable only since 1.85,
        // above this crate's MSRV. For these magnitudes it is bit-identical
        // to the average, and the average is also exactly what CPython's
        // statistics.median computes.
        f64::midpoint(values[middle - 1], values[middle])
    } else {
        values[middle]
    })
}

/// Read a numeric field, accepting a numeric string.
///
/// The Python `normalize_value` treats the strings `""`, `"none"`, `"null"`,
/// `"n/a"` and `"unavailable"` as absent; `as_number` applies the same rule
/// by simply failing to parse them.
fn number(object: &Value, key: &str) -> Option<f64> {
    object.get(key).and_then(as_number)
}

/// Read a string field, treating the null-ish spellings as absent.
fn text(object: &Value, key: &str) -> Option<String> {
    let value = object.get(key)?.as_str()?.trim();

    if is_null_string(value) {
        None
    } else {
        Some(value.to_owned())
    }
}

/// Whether a string means "no value", as `normalize_value` defines it.
fn is_null_string(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "none" | "null" | "n/a" | "unavailable"
    )
}

/// Interpret a JSON value as a number, accepting a numeric string.
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|number| number.is_finite()),
        Value::String(text) => {
            let text = text.trim();
            if is_null_string(text) {
                None
            } else {
                text.parse::<f64>().ok().filter(|number| number.is_finite())
            }
        }
        _ => None,
    }
}

/// Render a scalar JSON value as a string, for fields published either way.
fn scalar_to_string(value: &Value) -> Option<String> {
    let rendered = match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => return None,
    };

    if is_null_string(&rendered) {
        None
    } else {
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target payload shaped like the real one, trimmed to two qubits and
    /// two gates.
    const PAYLOAD: &str = r#"{
      "configuration": {
        "n_qubits": 156,
        "backend_version": "1.3.29",
        "clops_h": 320000,
        "sample_name": "family: Heron, revision: 2",
        "basis_gates": ["sx", "rz", "cz"]
      },
      "properties": {
        "qubits": [
          [
            {"name": "T1", "value": 140.0, "unit": "us"},
            {"name": "T2", "value": 100.0, "unit": "us"},
            {"name": "readout_error", "value": 0.009}
          ],
          [
            {"name": "T1", "value": 142.0, "unit": "us"},
            {"name": "T2", "value": 102.0, "unit": "us"},
            {"name": "readout_error", "value": 0.011}
          ]
        ],
        "gates": [
          {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.0002}]},
          {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.0004}]},
          {"gate": "cz", "parameters": [{"name": "gate_error", "value": 0.0026}]}
        ]
      }
    }"#;

    #[test]
    fn parses_configuration_fields() {
        let target = Target::parse(PAYLOAD).expect("parses");

        assert_eq!(target.qubits(), Some(156));
        assert_eq!(target.qpu_version().as_deref(), Some("1.3.29"));
        assert_eq!(target.clops(), Some(320_000.0));
    }

    #[test]
    fn parses_processor_type_from_sample_name() {
        let target = Target::parse(PAYLOAD).expect("parses");
        let processor = target.processor_type().expect("present");

        assert_eq!(processor.family, "Heron");
        assert_eq!(processor.revision.as_deref(), Some("2"));
    }

    #[test]
    fn processor_type_object_wins_over_sample_name() {
        let target = Target::parse(
            r#"{"configuration": {
                 "processor_type": {"family": "Nighthawk", "revision": "1"},
                 "sample_name": "family: Heron, revision: 2"
               }}"#,
        )
        .expect("parses");

        let processor = target.processor_type().expect("present");
        assert_eq!(processor.family, "Nighthawk");
        assert_eq!(processor.revision.as_deref(), Some("1"));
    }

    /// A revision published as a number must still compare as text.
    #[test]
    fn numeric_revision_normalises_to_text() {
        let target = Target::parse(
            r#"{"configuration": {"processor_type": {"family": "Heron", "revision": 2}}}"#,
        )
        .expect("parses");

        assert_eq!(
            target.processor_type().unwrap().revision.as_deref(),
            Some("2")
        );
    }

    #[test]
    fn computes_medians_as_python_does() {
        let target = Target::parse(PAYLOAD).expect("parses");
        let medians = target.medians();

        // Even counts average the two central values, like
        // statistics.median. The expected values below are the exact f64
        // results, captured from CPython running statistics.median on the
        // same samples -- binary floating point makes the midpoint of
        // 0.009 and 0.011 slightly less than 0.010, and asserting the
        // rounded value would be asserting something untrue.
        assert_eq!(medians.t1_median_us, Some(141.0));
        assert_eq!(medians.t2_median_us, Some(101.0));
        assert_eq!(
            medians.readout_error_median,
            Some(0.009_999_999_999_999_998)
        );
        assert_eq!(medians.sx_error_median, Some(0.000_300_000_000_000_000_03));
        assert_eq!(medians.cz_error_median, Some(0.0026));
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5.0]), Some(5.0));
        assert_eq!(median(vec![1.0, 3.0]), Some(2.0));
        assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
        // Non-finite samples are discarded, not propagated.
        assert_eq!(median(vec![1.0, f64::NAN, 3.0]), Some(2.0));
        assert_eq!(median(vec![f64::NAN]), None);
    }

    #[test]
    fn converts_every_recognised_time_unit() {
        assert_eq!(convert_time_to_us(1.0, Some("s")), Some(1_000_000.0));
        assert_eq!(convert_time_to_us(1.0, Some("ms")), Some(1_000.0));
        assert_eq!(convert_time_to_us(1.0, Some("us")), Some(1.0));
        assert_eq!(convert_time_to_us(1.0, Some("ns")), Some(0.001));
        // Both the micro sign (U+00B5) and the Greek mu (U+03BC).
        assert_eq!(convert_time_to_us(1.0, Some("µs")), Some(1.0));
        assert_eq!(convert_time_to_us(1.0, Some("μs")), Some(1.0));
        assert_eq!(convert_time_to_us(1.0, Some("US")), Some(1.0));
    }

    /// Guessing a missing unit could scale a coherence time by 1000.
    #[test]
    fn unknown_or_absent_units_are_refused() {
        assert_eq!(convert_time_to_us(1.0, None), None);
        assert_eq!(convert_time_to_us(1.0, Some("")), None);
        assert_eq!(convert_time_to_us(1.0, Some("minutes")), None);
    }

    #[test]
    fn times_with_unusable_units_are_dropped_from_medians() {
        let target = Target::parse(
            r#"{"properties": {"qubits": [[
                 {"name": "T1", "value": 100.0, "unit": "us"},
                 {"name": "T1", "value": 999.0, "unit": "fortnights"},
                 {"name": "T1", "value": 300.0}
               ]]}}"#,
        )
        .expect("parses");

        assert_eq!(target.medians().t1_median_us, Some(100.0));
    }

    #[test]
    fn nanosecond_times_are_normalised() {
        let target = Target::parse(
            r#"{"properties": {"qubits": [[
                 {"name": "T1", "value": 141000.0, "unit": "ns"}
               ]]}}"#,
        )
        .expect("parses");

        assert_eq!(target.medians().t1_median_us, Some(141.0));
    }

    #[test]
    fn clops_object_form_is_accepted() {
        let target =
            Target::parse(r#"{"configuration": {"clops": {"type": "hardware", "value": 12345}}}"#)
                .expect("parses");

        assert_eq!(target.clops(), Some(12345.0));
    }

    #[test]
    fn clops_falls_back_through_all_three_keys() {
        let target = Target::parse(r#"{"configuration": {"clops_v": 777}}"#).expect("parses");
        assert_eq!(target.clops(), Some(777.0));
    }

    #[test]
    fn missing_sections_yield_absent_metrics() {
        let target = Target::parse("{}").expect("parses");

        assert_eq!(target.qubits(), None);
        assert_eq!(target.clops(), None);
        assert_eq!(target.processor_type(), None);
        assert_eq!(target.medians(), SystemMedians::default());
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(Target::parse("not json").is_err());
        assert!(Target::parse("").is_err());
    }

    /// Null-ish strings must read as absent, not as the literal text.
    #[test]
    fn null_strings_read_as_absent() {
        let target = Target::parse(
            r#"{"configuration": {"backend_version": "none", "n_qubits": "unavailable"}}"#,
        )
        .expect("parses");

        assert_eq!(target.qpu_version(), None);
        assert_eq!(target.qubits(), None);
    }

    /// Vendors sometimes publish numbers as strings.
    #[test]
    fn numeric_strings_are_accepted() {
        let target =
            Target::parse(r#"{"configuration": {"n_qubits": "156", "clops_h": "320000"}}"#)
                .expect("parses");

        assert_eq!(target.qubits(), Some(156));
        assert_eq!(target.clops(), Some(320_000.0));
    }

    #[test]
    fn malformed_qubit_entries_are_skipped() {
        let target = Target::parse(
            r#"{"properties": {"qubits": [
                 "not a list",
                 [{"name": "T1", "value": "bad", "unit": "us"}],
                 [{"no_name": 1}],
                 [{"name": "T1", "value": 100.0, "unit": "us"}]
               ]}}"#,
        )
        .expect("parses");

        assert_eq!(target.medians().t1_median_us, Some(100.0));
    }

    #[test]
    fn gate_without_error_parameter_is_skipped() {
        let target = Target::parse(
            r#"{"properties": {"gates": [
                 {"gate": "sx", "parameters": [{"name": "gate_length", "value": 10.0}]},
                 {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.001}]}
               ]}}"#,
        )
        .expect("parses");

        assert_eq!(target.medians().sx_error_median, Some(0.001));
    }

    /// CZ is not calibrated on every processor family.
    #[test]
    fn absent_gate_yields_no_median() {
        let target = Target::parse(
            r#"{"properties": {"gates": [
                 {"gate": "sx", "parameters": [{"name": "gate_error", "value": 0.001}]}
               ]}}"#,
        )
        .expect("parses");

        assert_eq!(target.medians().cz_error_median, None);
        assert_eq!(target.medians().sx_error_median, Some(0.001));
    }

    #[test]
    fn builds_metrics_for_the_selectors() {
        let target = Target::parse(PAYLOAD).expect("parses");
        let metrics = target.to_metrics(Some(59), true);

        assert_eq!(metrics.qubits, Some(156));
        assert_eq!(metrics.pending_jobs, Some(59));
        assert_eq!(metrics.t1_median_us, Some(141.0));
        assert!(metrics.available);
        assert_eq!(
            metrics.processor_type.as_ref().map(|p| p.family.as_str()),
            Some("Heron")
        );
    }

    #[test]
    fn sample_name_without_revision_is_accepted() {
        let processor = parse_sample_name("family: Nighthawk").expect("family present");
        assert_eq!(processor.family, "Nighthawk");
        assert_eq!(processor.revision, None);
    }

    #[test]
    fn sample_name_without_family_is_rejected() {
        assert_eq!(parse_sample_name("revision: 2"), None);
        assert_eq!(parse_sample_name(""), None);
        assert_eq!(parse_sample_name("garbage"), None);
    }
}
