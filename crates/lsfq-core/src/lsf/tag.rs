// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The QPU the scheduler plug-in chose, as recorded on the job.
//!
//! `schmod_qrmi` can only write to a job through its resource requirement
//! (`extsched_modifyJob`), so it adds `rusage[qrmi_<qpu>=1]`. The jobstarter
//! reads that back with `bjobs -json -o effective_resreq`.
//!
//! The esub, by contrast, hands the request *to* the plug-in through the
//! `-extsched` string; [`extsched_request`] builds it.

use std::fmt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::bpost::JobReference;

/// Prefix of the per-QPU shared resource the plug-in adds to the job.
///
/// Must match `TAG_PREFIX` in `scheduler-plugin/schmod_qrmi.c`.
pub const TAG_PREFIX: &str = "qrmi_";

/// How long to wait for `bjobs`. The jobstarter is on the launch path.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Find the QPU named by a `qrmi_<qpu>=<n>` usage in a resource requirement.
///
/// Only a usage counts (the name must be followed by `=`, and must not be
/// the tail of a longer identifier), so a user's own `select[...]` mentioning
/// something similar is not mistaken for a tag.
///
/// ```
/// use lsfq_core::lsf::tag::find_tag;
///
/// let resreq = "select[type==local] rusage[mem=100:qrmi_ibm_fez=1]";
/// assert_eq!(find_tag(resreq), Some("ibm_fez".to_owned()));
/// assert_eq!(find_tag("select[type==local]"), None);
/// ```
#[must_use]
pub fn find_tag(text: &str) -> Option<String> {
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = text.as_bytes();
    let mut from = 0;

    while let Some(offset) = text[from..].find(TAG_PREFIX) {
        let start = from + offset;
        let name_start = start + TAG_PREFIX.len();
        from = name_start;

        if start > 0 && is_ident(bytes[start - 1]) {
            continue; // the tail of a longer name, e.g. `my_qrmi_x=1`
        }

        let name_end = bytes[name_start..]
            .iter()
            .position(|&byte| !is_ident(byte))
            .map_or(bytes.len(), |length| name_start + length);

        if name_end > name_start && bytes.get(name_end) == Some(&b'=') {
            return Some(text[name_start..name_end].to_owned());
        }
    }
    None
}

/// Ask LSF which QPU the plug-in assigned to a job.
///
/// `Ok(None)` means the job carries no tag: the plug-in is not installed, or
/// the job did not ask for it.
///
/// # Errors
///
/// Returns [`TagError`] if `bjobs` is missing, fails, or does not answer in
/// time.
pub fn read(reference: &JobReference) -> Result<Option<String>, TagError> {
    read_with(Command::new("bjobs"), reference)
}

/// As [`read`], with a caller-supplied command, so the invocation can be
/// tested against a stub.
///
/// # Errors
///
/// As [`read`].
pub fn read_with(
    mut command: Command,
    reference: &JobReference,
) -> Result<Option<String>, TagError> {
    // -json: the default table truncates a long field with "...", which could
    // cut the tag off; JSON does not.
    command
        .args(["-json", "-o", "effective_resreq"])
        .arg(reference.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            TagError::Unavailable
        } else {
            TagError::Spawn { source }
        }
    })?;

    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child
            .try_wait()
            .map_err(|source| TagError::Spawn { source })?
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(TagError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let output = child
        .wait_with_output()
        .map_err(|source| TagError::Spawn { source })?;

    if !output.status.success() {
        return Err(TagError::Failed {
            code: output.status.code(),
        });
    }

    Ok(find_tag(&String::from_utf8_lossy(&output.stdout)))
}

/// Reading the tag failed.
#[derive(Debug)]
pub enum TagError {
    /// `bjobs` is not on `PATH`.
    Unavailable,
    /// `bjobs` could not be started.
    Spawn {
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// `bjobs` did not answer within [`TIMEOUT`].
    TimedOut,
    /// `bjobs` exited non-zero.
    Failed {
        /// The exit code, or `None` if killed by a signal.
        code: Option<i32>,
    },
}

impl fmt::Display for TagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("bjobs is unavailable"),
            Self::Spawn { source } => write!(f, "bjobs could not be started: {source}"),
            Self::TimedOut => f.write_str("bjobs timed out"),
            Self::Failed { code: Some(code) } => write!(f, "bjobs exited with status {code}"),
            Self::Failed { code: None } => f.write_str("bjobs was terminated by a signal"),
        }
    }
}

impl std::error::Error for TagError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source } => Some(source),
            _ => None,
        }
    }
}

/// A value that cannot be carried in the `-extsched` request.
#[derive(Debug, PartialEq, Eq)]
pub struct UnsafeRequest {
    /// The field involved.
    pub field: &'static str,
    /// The offending character.
    pub character: char,
}

impl fmt::Display for UnsafeRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} contains {:?}, which cannot be passed to the scheduler plug-in",
            self.field, self.character
        )
    }
}

impl std::error::Error for UnsafeRequest {}

/// Build the `QRMI[...]` string the plug-in reads from `bsub -extsched`.
///
/// Segments are separated by `|`, each `key=value`; the `qpu` value is the
/// `;`-separated attribute string and may itself contain `=`.
///
/// # Errors
///
/// Returns [`UnsafeRequest`] if a value contains `|`, `]`, `"`, or a newline,
/// any of which would corrupt the string.
///
/// ```
/// use lsfq_core::lsf::tag::extsched_request;
///
/// assert_eq!(
///     extsched_request("priority", None, Some("qubits=128;pending_jobs=100")).unwrap(),
///     "QRMI[selector=priority|qpu=qubits=128;pending_jobs=100]"
/// );
/// assert_eq!(
///     extsched_request("basic", Some("ibm_fez"), None).unwrap(),
///     "QRMI[selector=basic|device=ibm_fez]"
/// );
/// ```
pub fn extsched_request(
    selector: &str,
    device: Option<&str>,
    qpu: Option<&str>,
) -> Result<String, UnsafeRequest> {
    let check = |field: &'static str, value: &str| {
        value
            .chars()
            .find(|character| matches!(character, '|' | ']' | '"' | '\n' | '\r'))
            .map_or(Ok(()), |character| Err(UnsafeRequest { field, character }))
    };

    check("selector", selector)?;
    let mut text = format!("QRMI[selector={selector}");
    if let Some(device) = device {
        check("device", device)?;
        text.push_str("|device=");
        text.push_str(device);
    }
    if let Some(qpu) = qpu {
        check("qpu", qpu)?;
        text.push_str("|qpu=");
        text.push_str(qpu);
    }
    text.push(']');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_tag_in_a_realistic_effective_resreq() {
        let resreq = "select[(type == local)] order[r15s:pg] \
                      rusage[mem=100:qrmi_ibm_marrakesh=1] span[hosts=1]";
        assert_eq!(find_tag(resreq), Some("ibm_marrakesh".to_owned()));
    }

    #[test]
    fn finds_the_tag_in_bjobs_json() {
        let json = r#"{"COMMAND":"bjobs","JOBS":1,"RECORDS":[{"JOBID":"101",
            "EFFECTIVE_RESREQ":"select[type==any] rusage[qrmi_ibm_fez=1] "}]}"#;
        assert_eq!(find_tag(json), Some("ibm_fez".to_owned()));
    }

    #[test]
    fn a_tag_alone_or_first_in_the_rusage_is_found() {
        assert_eq!(
            find_tag("rusage[qrmi_ibm_fez=1]"),
            Some("ibm_fez".to_owned())
        );
        assert_eq!(find_tag("rusage[qrmi_a=1:mem=5]"), Some("a".to_owned()));
    }

    #[test]
    fn lookalikes_are_not_tags() {
        assert_eq!(find_tag("rusage[my_qrmi_ibm_fez=1]"), None);
        assert_eq!(find_tag("select[qrmi_ibm_fez]"), None); // not a usage
        assert_eq!(find_tag("rusage[qrmi_=1]"), None);
        assert_eq!(find_tag(""), None);
        // A lookalike does not hide a real tag after it.
        assert_eq!(
            find_tag("rusage[my_qrmi_x=1:qrmi_ibm_fez=1]"),
            Some("ibm_fez".to_owned())
        );
    }

    #[test]
    fn missing_bjobs_is_reported_as_unavailable() {
        let reference = JobReference::new(1, 0).expect("valid");
        assert!(matches!(
            read_with(Command::new("definitely-not-a-real-bjobs"), &reference)
                .expect_err("not installed"),
            TagError::Unavailable
        ));
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let reference = JobReference::new(1, 0).expect("valid");
        assert!(matches!(
            read_with(Command::new("false"), &reference).expect_err("exits 1"),
            TagError::Failed { code: Some(1) }
        ));
    }

    /// The real invocation is `bjobs -json -o effective_resreq <jobref>`, and
    /// the answer is parsed from its stdout.
    #[test]
    fn invokes_bjobs_and_parses_its_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("fake-bjobs");
        let record = dir.path().join("args");

        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n\
                 echo '{{\"RECORDS\":[{{\"EFFECTIVE_RESREQ\":\"rusage[qrmi_ibm_fez=1]\"}}]}}'\n",
                record.display()
            ),
        )
        .expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let reference = JobReference::new(999, 4).expect("valid");
        let tag = read_with(Command::new(&script), &reference).expect("runs");
        assert_eq!(tag, Some("ibm_fez".to_owned()));

        let logged = std::fs::read_to_string(&record).expect("read");
        let args: Vec<&str> = logged.lines().collect();
        assert_eq!(args, vec!["-json", "-o", "effective_resreq", "999[4]"]);
    }

    #[test]
    fn an_untagged_job_is_ok_none() {
        let reference = JobReference::new(1, 0).expect("valid");
        // `echo` ignores its arguments' meaning and prints them.
        assert_eq!(
            read_with(Command::new("echo"), &reference).expect("runs"),
            None
        );
    }

    #[test]
    fn extsched_request_rejects_separators() {
        for bad in ["a|b", "a]b", "a\"b", "a\nb"] {
            assert!(
                extsched_request("basic", None, Some(bad)).is_err(),
                "{bad:?}"
            );
            assert!(
                extsched_request("basic", Some(bad), None).is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(
            extsched_request("a|b", None, None).unwrap_err().field,
            "selector"
        );
    }
}
