// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Posting the QRMI acquisition ID to the LSF job record.
//!
//! The jobstarter records which QPU a job acquired by attaching a JSON
//! message to the job at message index 1, via `bpost`. Operators read it back
//! with `bread`, so the key names are an external contract.

use std::env;
use std::fmt;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long to wait for `bpost` before giving up.
///
/// Matches the Python `timeout=10`. The jobstarter is on the critical path of
/// a job launch, so a hung scheduler command must not block it indefinitely.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The message attached to the job.
///
/// Serialised compactly (no spaces after separators), as the Python does with
/// `separators=(",", ":")`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AcquisitionMessage<'a> {
    /// The backend that was acquired.
    pub qrmi_resource: &'a str,
    /// The QRMI resource type, for example `qiskit-runtime-service`.
    pub qrmi_resource_type: &'a str,
    /// The acquisition token QRMI returned.
    pub qrmi_acquisition_id: &'a str,
}

impl AcquisitionMessage<'_> {
    /// Render the message as compact JSON.
    ///
    /// # Errors
    ///
    /// Returns [`serde_json::Error`] only if serialisation fails, which for
    /// these three string fields it does not in practice.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// How a job is addressed on the `bpost` command line.
///
/// A plain job is `12345`; an element of a job array is `12345[7]`. Index 0
/// means "not an array element" and is rendered without brackets, matching
/// the Python's `if int(job_index) > 0` test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReference {
    id: u64,
    index: u64,
}

impl JobReference {
    /// Build a reference from an id and an array index.
    ///
    /// # Errors
    ///
    /// Returns [`BpostError::InvalidJobId`] for an id of zero, which LSF
    /// never assigns.
    pub fn new(id: u64, index: u64) -> Result<Self, BpostError> {
        if id == 0 {
            return Err(BpostError::InvalidJobId);
        }
        Ok(Self { id, index })
    }

    /// Read `LSB_JOBID` and `LSB_JOBINDEX` from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`BpostError::InvalidJobId`] if `LSB_JOBID` is absent, not a
    /// number, or zero, and [`BpostError::InvalidJobIndex`] if
    /// `LSB_JOBINDEX` is present but not a number. A missing `LSB_JOBINDEX`
    /// defaults to 0, as the Python does.
    pub fn from_env() -> Result<Self, BpostError> {
        let id: u64 = env::var("LSB_JOBID")
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .ok_or(BpostError::InvalidJobId)?;

        let index: u64 = match env::var("LSB_JOBINDEX") {
            Ok(value) => value
                .trim()
                .parse()
                .map_err(|_| BpostError::InvalidJobIndex)?,
            Err(_) => 0,
        };

        Self::new(id, index)
    }
}

impl fmt::Display for JobReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.index > 0 {
            write!(f, "{}[{}]", self.id, self.index)
        } else {
            write!(f, "{}", self.id)
        }
    }
}

/// Post an acquisition message to a job's message index 1.
///
/// This is best-effort by design: the job is already running, or about to,
/// and a failure to annotate it must not fail the job. Every failure is
/// returned for the caller to log as a warning, exactly as the Python's
/// nested `warn()` does.
///
/// # Errors
///
/// Returns [`BpostError`] if the message cannot be built, `bpost` is not
/// installed, it times out, or it exits non-zero.
pub fn post(reference: &JobReference, message: &AcquisitionMessage<'_>) -> Result<(), BpostError> {
    let payload = message.to_json().map_err(|_| BpostError::Unserialisable)?;
    post_with(Command::new("bpost"), reference, &payload)
}

/// Post a pre-rendered payload using a caller-supplied command.
///
/// Exists so the invocation can be tested against a stub `bpost` without a
/// scheduler installed.
///
/// # Errors
///
/// As [`post`].
pub fn post_with(
    mut command: Command,
    reference: &JobReference,
    payload: &str,
) -> Result<(), BpostError> {
    command
        .arg("-i")
        .arg("1")
        .arg("-d")
        .arg(payload)
        .arg(reference.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            BpostError::Unavailable
        } else {
            BpostError::Spawn { source }
        }
    })?;

    // std::process has no timed wait, so poll. The interval is small
    // relative to TIMEOUT and this path runs once per job.
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|source| BpostError::Spawn { source })?
        {
            return if status.success() {
                Ok(())
            } else {
                Err(BpostError::Failed {
                    code: status.code(),
                })
            };
        }

        if std::time::Instant::now() >= deadline {
            // Reap the stuck child rather than leaving a zombie.
            let _ = child.kill();
            let _ = child.wait();
            return Err(BpostError::TimedOut);
        }

        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Posting the acquisition message failed.
///
/// Every variant is non-fatal to the job; the caller logs and continues.
#[derive(Debug)]
pub enum BpostError {
    /// `LSB_JOBID` was missing, unparseable, or zero.
    InvalidJobId,
    /// `LSB_JOBINDEX` was present but not a number.
    InvalidJobIndex,
    /// The message could not be serialised.
    Unserialisable,
    /// `bpost` is not on `PATH`.
    Unavailable,
    /// `bpost` could not be started.
    Spawn {
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// `bpost` did not finish within [`TIMEOUT`].
    TimedOut,
    /// `bpost` exited with a non-zero status.
    Failed {
        /// The exit code, or `None` if it was killed by a signal.
        code: Option<i32>,
    },
}

impl fmt::Display for BpostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJobId => f.write_str("missing or invalid LSB_JOBID"),
            Self::InvalidJobIndex => f.write_str("invalid LSB_JOBINDEX"),
            Self::Unserialisable => f.write_str("message could not be posted"),
            Self::Unavailable => f.write_str("bpost is unavailable"),
            Self::Spawn { source } => write!(f, "bpost could not be started: {source}"),
            Self::TimedOut => f.write_str("bpost timed out"),
            Self::Failed { code } => match code {
                Some(code) => write!(f, "bpost exited with status {code}"),
                None => f.write_str("bpost was terminated by a signal"),
            },
        }
    }
}

impl std::error::Error for BpostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_is_compact_json_with_the_contracted_keys() {
        let message = AcquisitionMessage {
            qrmi_resource: "ibm_fez",
            qrmi_resource_type: "qiskit-runtime-service",
            qrmi_acquisition_id: "abc-123",
        };

        // Key names and compactness are both part of the contract: operators
        // parse this with bread.
        assert_eq!(
            message.to_json().expect("serialises"),
            r#"{"qrmi_resource":"ibm_fez","qrmi_resource_type":"qiskit-runtime-service","qrmi_acquisition_id":"abc-123"}"#
        );
    }

    #[test]
    fn plain_job_reference_has_no_brackets() {
        assert_eq!(
            JobReference::new(12345, 0).expect("valid").to_string(),
            "12345"
        );
    }

    #[test]
    fn array_element_reference_is_bracketed() {
        assert_eq!(
            JobReference::new(12345, 7).expect("valid").to_string(),
            "12345[7]"
        );
    }

    #[test]
    fn job_id_zero_is_rejected() {
        assert!(matches!(
            JobReference::new(0, 0).expect_err("zero is invalid"),
            BpostError::InvalidJobId
        ));
    }

    #[test]
    fn error_messages_match_the_python_warnings() {
        // These strings appear in operator-facing warnings today.
        assert_eq!(
            BpostError::InvalidJobId.to_string(),
            "missing or invalid LSB_JOBID"
        );
        assert_eq!(
            BpostError::InvalidJobIndex.to_string(),
            "invalid LSB_JOBINDEX"
        );
        assert_eq!(BpostError::Unavailable.to_string(), "bpost is unavailable");
        assert_eq!(BpostError::TimedOut.to_string(), "bpost timed out");
        assert_eq!(
            BpostError::Failed { code: Some(3) }.to_string(),
            "bpost exited with status 3"
        );
    }

    #[test]
    fn missing_bpost_is_reported_as_unavailable() {
        let reference = JobReference::new(1, 0).expect("valid");
        let command = Command::new("definitely-not-a-real-bpost-binary");

        assert!(matches!(
            post_with(command, &reference, "{}").expect_err("not installed"),
            BpostError::Unavailable
        ));
    }

    #[test]
    fn successful_command_is_reported_as_success() {
        let reference = JobReference::new(42, 0).expect("valid");
        // `true` ignores its arguments and exits 0.
        assert!(post_with(Command::new("true"), &reference, "{}").is_ok());
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let reference = JobReference::new(42, 0).expect("valid");
        assert!(matches!(
            post_with(Command::new("false"), &reference, "{}").expect_err("exits 1"),
            BpostError::Failed { code: Some(1) }
        ));
    }

    /// The real invocation must be `bpost -i 1 -d <json> <jobref>`.
    #[test]
    fn arguments_are_passed_in_the_expected_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-bpost");
        let record = dir.path().join("args");

        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", record.display()),
        )
        .expect("write");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let reference = JobReference::new(999, 4).expect("valid");
        post_with(Command::new(&script), &reference, r#"{"a":1}"#).expect("runs");

        let logged = std::fs::read_to_string(&record).expect("read");
        let args: Vec<&str> = logged.lines().collect();
        assert_eq!(args, vec!["-i", "1", "-d", r#"{"a":1}"#, "999[4]"]);
    }
}
