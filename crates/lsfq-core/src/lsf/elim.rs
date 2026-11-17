// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The ELIM load-index protocol.
//!
//! An LSF ELIM writes one line per interval to stdout:
//!
//! ```text
//! <count> <name> <value> <name> <value> ...
//! ```
//!
//! `<count>` is the number of name/value pairs. LSF reads `-` as "value not
//! currently available" for a numeric resource.
//!
//! Two invariants are structural here rather than left to the caller, because
//! the ELIM's own source comments call out both as hazards:
//!
//! * the field count is always [`INDEX_COUNT`], even when data is missing --
//!   a short line is misparsed by LSF; and
//! * a transient fetch failure substitutes `-` rather than skipping a field.

use std::fmt;

/// The index names an ELIM line carries, in order.
///
/// The `µs` spellings are deliberate: they are what `lsf.shared` is
/// configured with on deployed clusters, so changing them to `us` would
/// silently stop LSF matching the indices. (Note the asymmetry with the
/// request grammar, where the same metric is spelled `T1_median_us`; the two
/// are separate contracts.)
pub const INDEX_NAMES: [&str; 10] = [
    "qubits",
    "clops",
    "qpu_version",
    "processor_type",
    "pending_jobs",
    "readout_error_median",
    "sx_error_median",
    "cz_error_median",
    "T1_median_µs",
    "T2_median_µs",
];

/// How many name/value pairs an ELIM line carries.
pub const INDEX_COUNT: usize = INDEX_NAMES.len();

/// The value LSF reads as "not currently available".
pub const UNAVAILABLE: &str = "-";

/// One ELIM reporting cycle's values.
///
/// Every field is optional; an absent one is rendered as [`UNAVAILABLE`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadIndices {
    /// Qubit count.
    pub qubits: Option<u32>,
    /// CLOPS.
    pub clops: Option<f64>,
    /// Vendor version string.
    pub qpu_version: Option<String>,
    /// Processor type, rendered as a single token such as `Heron_r2`.
    pub processor_type: Option<String>,
    /// Queue depth.
    pub pending_jobs: Option<u64>,
    /// Median readout error.
    pub readout_error_median: Option<f64>,
    /// Median SX-gate error.
    pub sx_error_median: Option<f64>,
    /// Median CZ-gate error.
    pub cz_error_median: Option<f64>,
    /// Median T1, microseconds.
    pub t1_median_us: Option<f64>,
    /// Median T2, microseconds.
    pub t2_median_us: Option<f64>,
}

impl LoadIndices {
    /// Render the full ELIM line, without a trailing newline.
    ///
    /// The line always has [`INDEX_COUNT`] pairs and a trailing space after
    /// the last value, both matching the Python implementation's output
    /// byte-for-byte.
    ///
    /// # Example
    ///
    /// ```
    /// use lsfq_core::lsf::elim::LoadIndices;
    ///
    /// let indices = LoadIndices { qubits: Some(156), ..Default::default() };
    /// let line = indices.render();
    ///
    /// assert!(line.starts_with("10 qubits 156 clops - "));
    /// ```
    #[must_use]
    pub fn render(&self) -> String {
        let values = [
            format_plain(self.qubits.as_ref()),
            format_plain(self.clops.as_ref()),
            format_plain(self.qpu_version.as_ref()),
            format_plain(self.processor_type.as_ref()),
            format_plain(self.pending_jobs.as_ref()),
            format_scientific(self.readout_error_median),
            format_scientific(self.sx_error_median),
            format_scientific(self.cz_error_median),
            format_fixed(self.t1_median_us),
            format_fixed(self.t2_median_us),
        ];

        let mut line = String::with_capacity(192);
        line.push_str(&INDEX_COUNT.to_string());
        line.push(' ');

        for (name, value) in INDEX_NAMES.iter().zip(values.iter()) {
            line.push_str(name);
            line.push(' ');
            line.push_str(value);
            line.push(' ');
        }

        line
    }

    /// A line reporting every index as unavailable.
    ///
    /// Used when a fetch fails outright: the ELIM must keep emitting
    /// well-formed lines, because an ELIM that stops reporting -- or exits --
    /// takes every index for its host down with it.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::default()
    }
}

/// Format a plain value, or `-` when absent.
///
/// LSF treats `-` as "value not currently available".
fn format_plain<T: fmt::Display>(value: Option<&T>) -> String {
    value.map_or_else(|| UNAVAILABLE.to_owned(), ToString::to_string)
}

/// Format in scientific notation with one decimal, as LSF expects.
///
/// Reproduces the Python `format_scientific()` byte-for-byte, which is
/// `format(value, '.1e')` with `e-0`/`e+0` collapsed to `e-`/`e+`. Python
/// pads the exponent to two digits and always writes its sign, so after the
/// fix-up it emits `9.4e-3` for a negative exponent but `0.0e+0` and
/// `1.2e+5` for zero and positive ones.
///
/// Rust's `{:.1e}` neither pads nor signs a non-negative exponent, giving
/// `0.0e0` and `1.2e5`. The two therefore agree only on negative exponents
/// -- which is every real calibration figure, but not every value this
/// function can be handed -- so the `+` is reinserted explicitly rather than
/// assumed away. Verified against `CPython` for both sign cases; see the
/// tests.
fn format_scientific(value: Option<f64>) -> String {
    match value {
        None => UNAVAILABLE.to_owned(),
        Some(value) if !value.is_finite() => UNAVAILABLE.to_owned(),
        Some(value) => {
            let rendered = format!("{value:.1e}");

            // Rust writes "9.4e-3" and "1.2e5"; Python writes "9.4e-3" and
            // "1.2e+5". Insert the sign Rust omits.
            match rendered.split_once('e') {
                Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                    format!("{mantissa}e+{exponent}")
                }
                _ => rendered,
            }
        }
    }
}

/// Format with two decimals, as LSF expects for the coherence times.
fn format_fixed(value: Option<f64>) -> String {
    match value {
        None => UNAVAILABLE.to_owned(),
        Some(value) if !value.is_finite() => UNAVAILABLE.to_owned(),
        Some(value) => format!("{value:.2}"),
    }
}

/// Render a processor type as the single token the ELIM reports.
///
/// The ELIM publishes `Heron_r2` rather than a structured value, because an
/// LSF resource value cannot contain spaces. A family with no revision is
/// reported as the family alone, and an absent family as `Unknown` -- which
/// is what the Python's `except (IndexError, ValueError)` branch produces.
#[must_use]
pub fn format_processor_type(family: Option<&str>, revision: Option<&str>) -> String {
    match (family, revision) {
        (Some(family), Some(revision)) => format!("{family}_r{revision}"),
        (Some(family), None) => family.to_owned(),
        (None, _) => "Unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> LoadIndices {
        LoadIndices {
            qubits: Some(156),
            clops: Some(320_000.0),
            qpu_version: Some("1.3.29".to_owned()),
            processor_type: Some("Heron_r2".to_owned()),
            pending_jobs: Some(59),
            readout_error_median: Some(0.009_399_414_062_5),
            sx_error_median: Some(0.000_272_840_430_114_404_4),
            cz_error_median: Some(0.002_648_294_774_599_657_7),
            t1_median_us: Some(141.29),
            t2_median_us: Some(101.06),
        }
    }

    /// The README records real ELIM output; the shape must match it.
    #[test]
    fn renders_the_readme_line_shape() {
        let line = full().render();

        assert!(
            line.starts_with("10 qubits 156 clops 320000 qpu_version 1.3.29 "),
            "{line}"
        );
        assert!(line.contains("processor_type Heron_r2 "), "{line}");
        assert!(line.contains("pending_jobs 59 "), "{line}");
        assert!(line.contains("T1_median_µs 141.29 "), "{line}");
        assert!(line.contains("T2_median_µs 101.06 "), "{line}");
        assert!(line.ends_with(' '), "the Python emits a trailing space");
    }

    /// LSF parses by field count, so this must hold for every input.
    #[test]
    fn field_count_is_always_ten() {
        for indices in [full(), LoadIndices::unavailable()] {
            let line = indices.render();
            let fields: Vec<&str> = line.split_whitespace().collect();

            // 1 count + 10 name/value pairs.
            assert_eq!(fields.len(), 1 + 2 * INDEX_COUNT, "{line}");
            assert_eq!(fields[0], "10");

            for (position, name) in INDEX_NAMES.iter().enumerate() {
                assert_eq!(fields[1 + 2 * position], *name);
            }
        }
    }

    #[test]
    fn missing_values_render_as_dash() {
        let line = LoadIndices::unavailable().render();
        assert_eq!(
            line,
            "10 qubits - clops - qpu_version - processor_type - pending_jobs - \
             readout_error_median - sx_error_median - cz_error_median - \
             T1_median_µs - T2_median_µs - "
        );
    }

    /// Must match the Python `format_scientific()` byte-for-byte. The
    /// expected values below were captured from `CPython` running the original
    /// expression, including the signed non-negative exponents that Rust's
    /// native `{:.1e}` does not produce.
    #[test]
    fn scientific_notation_matches_python() {
        // Negative exponents: the real calibration figures.
        assert_eq!(format_scientific(Some(0.009_399_414_062_5)), "9.4e-3");
        assert_eq!(format_scientific(Some(0.000_272_840_430_1)), "2.7e-4");
        assert_eq!(format_scientific(Some(0.002_648_294_774_6)), "2.6e-3");
        assert_eq!(format_scientific(Some(0.5)), "5.0e-1");
        assert_eq!(format_scientific(Some(9.99e-5)), "1.0e-4");
        // Two-digit exponent: no padding zero to strip.
        assert_eq!(format_scientific(Some(1e-10)), "1.0e-10");

        // Zero and positive exponents: Python writes the '+', Rust does not.
        assert_eq!(format_scientific(Some(0.0)), "0.0e+0");
        assert_eq!(format_scientific(Some(1.0)), "1.0e+0");
        assert_eq!(format_scientific(Some(123_456.0)), "1.2e+5");

        assert_eq!(format_scientific(None), "-");
    }

    #[test]
    fn fixed_notation_has_two_decimals() {
        assert_eq!(format_fixed(Some(141.294)), "141.29");
        assert_eq!(format_fixed(Some(101.0)), "101.00");
        assert_eq!(format_fixed(None), "-");
    }

    /// A NaN or infinity would render as "NaN"/"inf", which LSF cannot parse;
    /// report it as unavailable instead.
    #[test]
    fn non_finite_values_render_as_dash() {
        assert_eq!(format_fixed(Some(f64::NAN)), "-");
        assert_eq!(format_fixed(Some(f64::INFINITY)), "-");
        assert_eq!(format_scientific(Some(f64::NAN)), "-");
        assert_eq!(format_scientific(Some(f64::NEG_INFINITY)), "-");
    }

    #[test]
    fn processor_type_is_a_single_token() {
        assert_eq!(format_processor_type(Some("Heron"), Some("2")), "Heron_r2");
        assert_eq!(format_processor_type(Some("Nighthawk"), None), "Nighthawk");
        assert_eq!(format_processor_type(None, None), "Unknown");
        assert_eq!(format_processor_type(None, Some("2")), "Unknown");
    }

    /// No rendered value may contain a space, or the field count breaks.
    #[test]
    fn no_value_contains_whitespace() {
        let line = full().render();
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 21);

        let indices = LoadIndices {
            qpu_version: Some("1.3.29".to_owned()),
            processor_type: Some(format_processor_type(Some("Heron"), Some("2"))),
            ..Default::default()
        };
        assert_eq!(indices.render().split_whitespace().count(), 21);
    }

    #[test]
    fn index_names_are_the_contracted_spellings() {
        // Changing any of these silently stops LSF matching the index.
        assert_eq!(INDEX_NAMES[8], "T1_median_µs");
        assert_eq!(INDEX_NAMES[9], "T2_median_µs");
        assert_eq!(INDEX_COUNT, 10);
    }
}
