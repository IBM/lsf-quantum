// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Metric vocabulary: what a QPU reports, and which direction is better.

use alloc::borrow::ToOwned;
use alloc::string::String;
use core::fmt;

/// Whether a larger or a smaller value of a metric is preferable.
///
/// In the Python implementation this distinction lives in two string sets,
/// `MAXIMIZE_METRICS` and `MINIMIZE_METRICS`, which are declared three
/// separate times (module level, and again inside each of the two
/// `select_device_priority` implementations). Here it is one table, consulted
/// through [`Metric::direction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// A larger value is better (qubits, CLOPS, coherence times).
    Maximize,
    /// A smaller value is better (queue depth, error rates).
    Minimize,
}

/// A metric a QPU can be selected on.
///
/// Parsing a request yields these rather than free strings, so a typo in a
/// metric name is rejected at parse time instead of silently comparing
/// against a missing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Metric {
    /// Number of qubits on the QPU.
    Qubits,
    /// Vendor-reported QPU version string.
    QpuVersion,
    /// Processor family/revision (for example Heron r2).
    ProcessorType,
    /// Circuit layer operations per second.
    Clops,
    /// Jobs currently queued on the QPU.
    PendingJobs,
    /// Median readout error across qubits.
    ReadoutErrorMedian,
    /// Median SX-gate error.
    SxErrorMedian,
    /// Median CZ-gate error.
    CzErrorMedian,
    /// Median T1 coherence time, in microseconds.
    T1MedianUs,
    /// Median T2 coherence time, in microseconds.
    T2MedianUs,
}

impl Metric {
    /// Every metric, in declaration order.
    pub const ALL: [Self; 10] = [
        Self::Qubits,
        Self::QpuVersion,
        Self::ProcessorType,
        Self::Clops,
        Self::PendingJobs,
        Self::ReadoutErrorMedian,
        Self::SxErrorMedian,
        Self::CzErrorMedian,
        Self::T1MedianUs,
        Self::T2MedianUs,
    ];

    /// The canonical name, as used in request strings and ELIM output.
    ///
    /// These spellings are part of the external contract: they appear in
    /// `bsub -a "qrmi(qpu.<name>=...)"` arguments and in `ESUB_USER_REQ_QPU`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Qubits => "qubits",
            Self::QpuVersion => "qpu_version",
            Self::ProcessorType => "processor_type",
            Self::Clops => "clops",
            Self::PendingJobs => "pending_jobs",
            Self::ReadoutErrorMedian => "readout_error_median",
            Self::SxErrorMedian => "sx_error_median",
            Self::CzErrorMedian => "cz_error_median",
            Self::T1MedianUs => "T1_median_us",
            Self::T2MedianUs => "T2_median_us",
        }
    }

    /// Resolve a metric name as written in a request.
    ///
    /// Matching is ASCII-case-insensitive so that both `T1_median_us` and
    /// `t1_median_us` resolve, matching the Python code's tolerance. A
    /// dotted path such as `processor_type.family` must have its first
    /// segment passed here; see [`crate::request::Requirement`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|metric| metric.as_str().eq_ignore_ascii_case(name))
    }

    /// Which direction is better for this metric, if it is ranked numerically.
    ///
    /// `None` for the two descriptive metrics ([`Metric::QpuVersion`] and
    /// [`Metric::ProcessorType`]), which are matched for equality rather than
    /// ordered. This is the single source of truth that replaces the
    /// triplicated `MAXIMIZE_METRICS`/`MINIMIZE_METRICS` sets.
    #[must_use]
    pub const fn direction(self) -> Option<Direction> {
        match self {
            Self::Qubits | Self::Clops | Self::T1MedianUs | Self::T2MedianUs => {
                Some(Direction::Maximize)
            }
            Self::PendingJobs
            | Self::ReadoutErrorMedian
            | Self::SxErrorMedian
            | Self::CzErrorMedian => Some(Direction::Minimize),
            Self::QpuVersion | Self::ProcessorType => None,
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A QPU processor family and revision.
///
/// QRMI's `target()` document spells this two ways: a `processor_type`
/// object `{"family": "Heron", "revision": "2"}`, or a `sample_name` string
/// `"family: Heron, revision: 2"`. Both normalise to this type upstream (in
/// `lsfq-core`); this crate only compares them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessorType {
    /// Processor family, for example `Heron` or `Nighthawk`.
    pub family: String,
    /// Optional revision within the family.
    pub revision: Option<String>,
}

impl ProcessorType {
    /// Construct from a family name alone.
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            family: family.into(),
            revision: None,
        }
    }

    /// Attach a revision.
    #[must_use]
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = Some(revision.into());
        self
    }
}

/// One backend's observable metrics.
///
/// Every field is `Option` because vendors, QRMI versions, and transient
/// fetch failures all yield different subsets. A missing metric is
/// represented once, here, rather than by the mixture of `None`, `0`, `1.0`
/// and the string `"-"` that the Python code uses in different places.
///
/// # Example
///
/// ```
/// use qpu_select::{QpuMetrics, ProcessorType};
///
/// let metrics = QpuMetrics::builder()
///     .qubits(156)
///     .clops(320_000.0)
///     .pending_jobs(59)
///     .processor_type(ProcessorType::new("Heron").with_revision("2"))
///     .available(true)
///     .build();
///
/// assert_eq!(metrics.qubits, Some(156));
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QpuMetrics {
    /// Number of qubits.
    pub qubits: Option<u32>,
    /// Vendor-reported version string.
    pub qpu_version: Option<String>,
    /// Processor family and revision.
    pub processor_type: Option<ProcessorType>,
    /// Circuit layer operations per second.
    pub clops: Option<f64>,
    /// Jobs currently queued.
    pub pending_jobs: Option<u64>,
    /// Median readout error.
    pub readout_error_median: Option<f64>,
    /// Median SX-gate error.
    pub sx_error_median: Option<f64>,
    /// Median CZ-gate error.
    pub cz_error_median: Option<f64>,
    /// Median T1 coherence time, microseconds.
    pub t1_median_us: Option<f64>,
    /// Median T2 coherence time, microseconds.
    pub t2_median_us: Option<f64>,
    /// Whether the backend is accepting work.
    ///
    /// Corresponds to QRMI status `online`, which the REST API reported as
    /// the message `available`. Selectors skip unavailable backends.
    pub available: bool,
}

impl QpuMetrics {
    /// Start building a metrics record.
    #[must_use]
    pub fn builder() -> QpuMetricsBuilder {
        QpuMetricsBuilder::default()
    }

    /// Read one metric as a comparable value.
    ///
    /// Numeric metrics widen to `f64` so that a request may compare an
    /// integer metric against a fractional threshold (`qubits>=127.5`)
    /// without a type error, matching the Python behaviour of coercing both
    /// sides with `float()`.
    #[must_use]
    pub fn get(&self, metric: Metric) -> Option<MetricValue> {
        match metric {
            Metric::Qubits => self.qubits.map(|v| MetricValue::Number(f64::from(v))),
            Metric::Clops => self.clops.map(MetricValue::Number),
            #[allow(clippy::cast_precision_loss)]
            Metric::PendingJobs => self.pending_jobs.map(|v| MetricValue::Number(v as f64)),
            Metric::ReadoutErrorMedian => self.readout_error_median.map(MetricValue::Number),
            Metric::SxErrorMedian => self.sx_error_median.map(MetricValue::Number),
            Metric::CzErrorMedian => self.cz_error_median.map(MetricValue::Number),
            Metric::T1MedianUs => self.t1_median_us.map(MetricValue::Number),
            Metric::T2MedianUs => self.t2_median_us.map(MetricValue::Number),
            Metric::QpuVersion => self
                .qpu_version
                .as_deref()
                .map(|v| MetricValue::Text(v.to_owned())),
            Metric::ProcessorType => self
                .processor_type
                .as_ref()
                .map(|p| MetricValue::Text(p.family.clone())),
        }
    }

    /// Read a metric addressed by a possibly-dotted path.
    ///
    /// `processor_type` yields the family, and `processor_type.family` and
    /// `processor_type.revision` address the fields explicitly. This mirrors
    /// the Python `get_metric()` closure, which walks dotted attributes
    /// through nested dicts and then special-cases `processor_type` to its
    /// `family` key.
    #[must_use]
    pub fn get_path(&self, metric: Metric, field: Option<&str>) -> Option<MetricValue> {
        let Some(field) = field else {
            return self.get(metric);
        };

        if metric != Metric::ProcessorType {
            // Only processor_type is a structured value; any other dotted
            // path addresses a field that does not exist.
            return None;
        }

        let processor = self.processor_type.as_ref()?;

        match field {
            "family" => Some(MetricValue::Text(processor.family.clone())),
            "revision" => processor.revision.clone().map(MetricValue::Text),
            _ => None,
        }
    }
}

/// Builder for [`QpuMetrics`].
///
/// Keeps test fixtures and upstream conversion readable without making every
/// field mandatory.
#[derive(Debug, Clone, Default)]
pub struct QpuMetricsBuilder {
    metrics: QpuMetrics,
}

macro_rules! setter {
    ($name:ident, $ty:ty, $doc:literal) => {
        #[doc = $doc]
        #[must_use]
        pub fn $name(mut self, value: $ty) -> Self {
            self.metrics.$name = Some(value);
            self
        }
    };
}

impl QpuMetricsBuilder {
    setter!(qubits, u32, "Set the qubit count.");
    setter!(clops, f64, "Set CLOPS.");
    setter!(pending_jobs, u64, "Set the queue depth.");
    setter!(readout_error_median, f64, "Set the median readout error.");
    setter!(sx_error_median, f64, "Set the median SX-gate error.");
    setter!(cz_error_median, f64, "Set the median CZ-gate error.");
    setter!(t1_median_us, f64, "Set the median T1 time in microseconds.");
    setter!(t2_median_us, f64, "Set the median T2 time in microseconds.");
    setter!(processor_type, ProcessorType, "Set the processor type.");

    /// Set the vendor version string.
    #[must_use]
    pub fn qpu_version(mut self, value: impl Into<String>) -> Self {
        self.metrics.qpu_version = Some(value.into());
        self
    }

    /// Set whether the backend is accepting work.
    #[must_use]
    pub const fn available(mut self, value: bool) -> Self {
        self.metrics.available = value;
        self
    }

    /// Finish building.
    #[must_use]
    pub fn build(self) -> QpuMetrics {
        self.metrics
    }
}

/// A metric value in comparable form.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricValue {
    /// A numeric value; all numeric metrics widen to `f64`.
    Number(f64),
    /// A descriptive value, compared case-insensitively.
    Text(String),
}

impl MetricValue {
    /// The number, if this is numeric.
    #[must_use]
    pub const fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Text(_) => None,
        }
    }

    /// The text, if this is descriptive.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            Self::Number(_) => None,
        }
    }
}

impl fmt::Display for MetricValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Text(t) => f.write_str(t),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    #[test]
    fn metric_names_round_trip() {
        for metric in Metric::ALL {
            assert_eq!(Metric::from_name(metric.as_str()), Some(metric));
        }
    }

    #[test]
    fn metric_names_are_case_insensitive() {
        assert_eq!(Metric::from_name("T1_MEDIAN_US"), Some(Metric::T1MedianUs));
        assert_eq!(Metric::from_name("t1_median_us"), Some(Metric::T1MedianUs));
        assert_eq!(Metric::from_name("QuBiTs"), Some(Metric::Qubits));
    }

    #[test]
    fn unknown_metric_is_rejected() {
        assert_eq!(Metric::from_name("qbits"), None, "typo must not resolve");
        assert_eq!(Metric::from_name(""), None);
    }

    /// The directions must match the Python sets exactly, since a flipped
    /// direction silently inverts a user's request.
    #[test]
    fn directions_match_python_sets() {
        for metric in [
            Metric::Qubits,
            Metric::Clops,
            Metric::T1MedianUs,
            Metric::T2MedianUs,
        ] {
            assert_eq!(metric.direction(), Some(Direction::Maximize), "{metric}");
        }
        for metric in [
            Metric::PendingJobs,
            Metric::ReadoutErrorMedian,
            Metric::SxErrorMedian,
            Metric::CzErrorMedian,
        ] {
            assert_eq!(metric.direction(), Some(Direction::Minimize), "{metric}");
        }
        for metric in [Metric::QpuVersion, Metric::ProcessorType] {
            assert_eq!(metric.direction(), None, "{metric}");
        }
    }

    #[test]
    fn processor_type_reads_as_family() {
        let metrics = QpuMetrics::builder()
            .processor_type(ProcessorType::new("Heron").with_revision("2"))
            .build();

        assert_eq!(
            metrics.get(Metric::ProcessorType),
            Some(MetricValue::Text("Heron".to_owned()))
        );
        assert_eq!(
            metrics.get_path(Metric::ProcessorType, Some("revision")),
            Some(MetricValue::Text("2".to_owned()))
        );
        assert_eq!(metrics.get_path(Metric::ProcessorType, Some("nope")), None);
    }

    #[test]
    fn dotted_path_on_scalar_metric_is_none() {
        let metrics = QpuMetrics::builder().qubits(156).build();
        assert_eq!(metrics.get_path(Metric::Qubits, Some("family")), None);
    }

    #[test]
    fn missing_metrics_read_as_none() {
        let metrics = QpuMetrics::default();
        for metric in Metric::ALL {
            assert_eq!(metrics.get(metric), None, "{metric}");
        }
    }
}
