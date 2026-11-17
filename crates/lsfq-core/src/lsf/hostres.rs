// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Discovering which QPU an ELIM host serves.
//!
//! Each ELIM instance reports for exactly one QPU, and learns which from the
//! LSF host configuration: the host's `RESOURCES` list carries a resource
//! named after the backend. The Python implementation shells out to
//!
//! ```text
//! lshosts -o RESOURCES <host> | grep -v RESOURCES | sed 's/(//' | sed 's/)//'
//! ```
//!
//! through `shell=True`, then splits on whitespace, commas and semicolons and
//! takes the first token starting with `ibm_`. Here the pipeline collapses
//! into [`parse_resources`], so the parsing is unit-testable without a
//! scheduler and without a shell.

use std::fmt;
use std::process::Command;

/// The prefix that marks a QPU resource in a host's `RESOURCES` list.
pub const QPU_PREFIX: &str = "ibm_";

/// Extract the QPU resource name from `lshosts -o RESOURCES` output.
///
/// Accepts the raw output, including the `RESOURCES` header line and the
/// parentheses LSF wraps the list in. Returns the first token starting with
/// [`QPU_PREFIX`].
///
/// # Example
///
/// ```
/// use lsfq_core::lsf::hostres::parse_resources;
///
/// let output = "RESOURCES\n(bigmem ibm_fez linux)\n";
/// assert_eq!(parse_resources(output), Some("ibm_fez".to_owned()));
/// ```
#[must_use]
pub fn parse_resources(output: &str) -> Option<String> {
    output
        .lines()
        // Drop the header. The Python filters with `grep -v RESOURCES`,
        // which removes any line containing the word; matching the trimmed
        // line exactly is narrower and cannot discard a resource list that
        // happens to mention it.
        .filter(|line| line.trim() != "RESOURCES")
        .flat_map(|line| {
            line.split(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ';' | '(' | ')')
            })
        })
        .map(str::trim)
        .find(|token| token.starts_with(QPU_PREFIX) && token.len() > QPU_PREFIX.len())
        .map(ToOwned::to_owned)
}

/// Every QPU resource in the output, not just the first.
///
/// The Python takes only the first match. This is exposed so a caller can
/// detect the misconfiguration where one host is mapped to several QPUs --
/// which the single-match path would silently resolve to whichever came
/// first.
#[must_use]
pub fn parse_all_resources(output: &str) -> Vec<String> {
    let mut found = Vec::new();

    for token in output
        .lines()
        .filter(|line| line.trim() != "RESOURCES")
        .flat_map(|line| {
            line.split(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ';' | '(' | ')')
            })
        })
        .map(str::trim)
        .filter(|token| token.starts_with(QPU_PREFIX) && token.len() > QPU_PREFIX.len())
    {
        let token = token.to_owned();
        if !found.contains(&token) {
            found.push(token);
        }
    }

    found
}

/// Ask LSF which QPU this host serves.
///
/// Runs `lshosts -o RESOURCES <hostname>` directly, with no shell, and
/// parses the result with [`parse_resources`].
///
/// # Errors
///
/// Returns [`HostResourceError`] if the hostname cannot be determined,
/// `lshosts` is missing or fails, or no `ibm_*` resource is configured for
/// the host.
pub fn discover_qpu() -> Result<String, HostResourceError> {
    let hostname = hostname()?;
    discover_qpu_for(&hostname)
}

/// As [`discover_qpu`], for a named host.
///
/// # Errors
///
/// As [`discover_qpu`].
pub fn discover_qpu_for(hostname: &str) -> Result<String, HostResourceError> {
    let output = Command::new("lshosts")
        .arg("-o")
        .arg("RESOURCES")
        .arg(hostname)
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                HostResourceError::LshostsUnavailable
            } else {
                HostResourceError::Lshosts { source }
            }
        })?;

    if !output.status.success() {
        return Err(HostResourceError::LshostsFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    parse_resources(&stdout).ok_or_else(|| HostResourceError::NoQpuResource {
        hostname: hostname.to_owned(),
    })
}

/// This host's name.
///
/// # Errors
///
/// Returns [`HostResourceError::Hostname`] if it cannot be determined.
pub fn hostname() -> Result<String, HostResourceError> {
    // `hostname` is a POSIX utility; reading it this way avoids a crate
    // dependency for one call made once at ELIM startup.
    let output = Command::new("hostname")
        .output()
        .map_err(|source| HostResourceError::Hostname { source })?;

    if !output.status.success() {
        return Err(HostResourceError::Hostname {
            source: std::io::Error::other("hostname exited non-zero"),
        });
    }

    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();

    if name.is_empty() {
        return Err(HostResourceError::Hostname {
            source: std::io::Error::other("hostname returned nothing"),
        });
    }

    Ok(name)
}

/// Determining the host's QPU failed.
#[derive(Debug)]
pub enum HostResourceError {
    /// The hostname could not be determined.
    Hostname {
        /// The underlying error.
        source: std::io::Error,
    },
    /// `lshosts` is not on `PATH`.
    LshostsUnavailable,
    /// `lshosts` could not be run.
    Lshosts {
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// `lshosts` exited non-zero.
    LshostsFailed {
        /// Its exit code, if any.
        code: Option<i32>,
        /// Its stderr, trimmed.
        stderr: String,
    },
    /// The host has no `ibm_*` resource configured.
    NoQpuResource {
        /// The host that was queried.
        hostname: String,
    },
}

impl fmt::Display for HostResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hostname { source } => write!(f, "cannot determine hostname: {source}"),
            Self::LshostsUnavailable => f.write_str("lshosts is unavailable"),
            Self::Lshosts { source } => write!(f, "cannot run lshosts: {source}"),
            Self::LshostsFailed { code, stderr } => {
                write!(f, "lshosts failed")?;
                if let Some(code) = code {
                    write!(f, " with status {code}")?;
                }
                if !stderr.is_empty() {
                    write!(f, ": {stderr}")?;
                }
                Ok(())
            }
            Self::NoQpuResource { hostname } => write!(
                f,
                "no resource starting with '{QPU_PREFIX}' is configured for {hostname}"
            ),
        }
    }
}

impl std::error::Error for HostResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Hostname { source } | Self::Lshosts { source } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `lshosts -o RESOURCES` actually produces.
    #[test]
    fn parses_real_lshosts_output() {
        let output = "RESOURCES\n(mg lsfquantum ibm_fez)\n";
        assert_eq!(parse_resources(output), Some("ibm_fez".to_owned()));
    }

    #[test]
    fn strips_parentheses_and_header() {
        assert_eq!(
            parse_resources("RESOURCES\n(ibm_marrakesh)\n"),
            Some("ibm_marrakesh".to_owned())
        );
    }

    #[test]
    fn accepts_comma_and_semicolon_separators() {
        assert_eq!(parse_resources("(a,ibm_fez;b)"), Some("ibm_fez".to_owned()));
        assert_eq!(
            parse_resources("a ibm_boston b"),
            Some("ibm_boston".to_owned())
        );
    }

    #[test]
    fn returns_none_without_a_qpu_resource() {
        assert_eq!(parse_resources("RESOURCES\n(mg bigmem linux)\n"), None);
        assert_eq!(parse_resources(""), None);
        assert_eq!(parse_resources("RESOURCES\n"), None);
    }

    /// The bare prefix is not a backend name.
    #[test]
    fn bare_prefix_is_not_a_resource() {
        assert_eq!(parse_resources("(ibm_)"), None);
    }

    #[test]
    fn takes_the_first_qpu_resource() {
        assert_eq!(
            parse_resources("(ibm_aaa ibm_zzz)"),
            Some("ibm_aaa".to_owned())
        );
    }

    /// A host mapped to two QPUs is a misconfiguration the single-match path
    /// hides; expose it so the ELIM can warn.
    #[test]
    fn all_resources_reports_every_match() {
        assert_eq!(
            parse_all_resources("(ibm_aaa ibm_zzz ibm_aaa)"),
            vec!["ibm_aaa".to_owned(), "ibm_zzz".to_owned()]
        );
        assert!(parse_all_resources("(bigmem)").is_empty());
    }

    /// `grep -v RESOURCES` would drop a line merely containing the word; an
    /// exact match on the trimmed line does not.
    #[test]
    fn a_resource_list_mentioning_the_header_word_survives() {
        let output = "RESOURCES\n(ibm_fez RESOURCES_OK)\n";
        assert_eq!(parse_resources(output), Some("ibm_fez".to_owned()));
    }

    #[test]
    fn missing_lshosts_is_reported() {
        // Only meaningful where no scheduler is installed, which is the case
        // in CI and on a developer machine.
        if Command::new("lshosts").arg("--version").output().is_ok() {
            return;
        }

        assert!(matches!(
            discover_qpu_for("anyhost").expect_err("no lshosts"),
            HostResourceError::LshostsUnavailable
        ));
    }

    #[test]
    fn error_messages_are_legible() {
        let error = HostResourceError::NoQpuResource {
            hostname: "lsf-001".to_owned(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("ibm_"), "{rendered}");
        assert!(rendered.contains("lsf-001"), "{rendered}");
    }
}
