// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Credentials and QRMI configuration loading.
//!
//! Covers both files the project reads:
//!
//! * the `.env` named by `file=` on the `esub` command line, and
//! * `$LSF_ENVDIR/env.qpu`, which the ELIM reads,
//!
//! including the legacy `APIKEY`/`CRN` spellings and the endpoint defaults.
//! The Python implementations use `dotenv_values()` and `load_dotenv()` from
//! `python-dotenv`; this is a small parser covering the same subset, so the
//! dependency disappears.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The QRMI settings an IBM Quantum Compute Service resource requires.
///
/// QRMI reads each one per resource, from `<resource_id>_<SETTING>`.
pub const REQUIRED_VARS: [&str; 4] = [
    "QRMI_IBM_QCS_IAM_APIKEY",
    "QRMI_IBM_QCS_SERVICE_CRN",
    "QRMI_IBM_QCS_ENDPOINT",
    "QRMI_IBM_QCS_IAM_ENDPOINT",
];

/// Defaults applied when the file supplies neither spelling of an endpoint,
/// for example when it carries only the legacy `APIKEY`/`CRN` pair.
pub const ENDPOINT_DEFAULTS: [(&str, &str); 2] = [
    (
        "QRMI_IBM_QCS_ENDPOINT",
        "https://quantum.cloud.ibm.com/api/v1",
    ),
    ("QRMI_IBM_QCS_IAM_ENDPOINT", "https://iam.cloud.ibm.com"),
];

/// The credentials file the ELIM reads, inside `$LSF_ENVDIR`.
pub const ENV_QPU_FILE: &str = "env.qpu";

/// Legacy `env.qpu` names, mapped onto the QRMI names.
pub const LEGACY_ALIASES: [(&str, &str); 2] = [
    ("APIKEY", "QRMI_IBM_QCS_IAM_APIKEY"),
    ("CRN", "QRMI_IBM_QCS_SERVICE_CRN"),
];

/// A parsed dotenv-style file.
///
/// `BTreeMap` so that iteration is deterministic: the `esub` serialises these
/// into `LSF_SUB4_SUB_ENV_VARS`, and a stable order makes that output
/// reproducible and diffable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    values: BTreeMap<String, String>,
}

impl Credentials {
    /// Parse dotenv-style text.
    ///
    /// Supports the subset the project's files actually use: `KEY=VALUE` per
    /// line, `#` comments, blank lines, an optional `export ` prefix, and
    /// single- or double-quoted values. A line without `=` is ignored rather
    /// than rejected, matching `python-dotenv`'s tolerance -- these files are
    /// hand-maintained by administrators and a stray line should not take the
    /// ELIM down.
    ///
    /// Variable interpolation (`${OTHER}`) is deliberately *not* supported:
    /// `python-dotenv` does expand it, but no file in this project uses it,
    /// and silently mis-expanding an API key would be worse than not
    /// expanding it. [`Credentials::has_interpolation`] reports whether any
    /// value looks like it wanted expansion, so a caller can warn.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut values = BTreeMap::new();

        for line in text.lines() {
            let line = line.trim();

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let key = key.trim();
            if key.is_empty() {
                continue;
            }

            values.insert(key.to_owned(), unquote(value.trim()));
        }

        Self { values }
    }

    /// Read and parse a file.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialsError::Missing`] if the path is not a file, and
    /// [`CredentialsError::Unreadable`] if it cannot be read -- the latter is
    /// the common case for `env.qpu`, which is installed mode `0400` owned by
    /// the LSF administrator, so a non-privileged process gets a permission
    /// error rather than a missing file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CredentialsError> {
        let path = path.as_ref();

        if !path.is_file() {
            return Err(CredentialsError::Missing {
                path: path.to_path_buf(),
            });
        }

        let text = fs::read_to_string(path).map_err(|source| CredentialsError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(Self::parse(&text))
    }

    /// Load `$LSF_ENVDIR/env.qpu`, the file the ELIM reads.
    ///
    /// The directory comes from `LSF_ENVDIR`, which LSF sets for its own
    /// daemons; an ELIM started by `lim` inherits it.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialsError::NoEnvDir`] if `LSF_ENVDIR` is unset, and
    /// otherwise whatever [`Credentials::load`] reports -- in particular
    /// [`CredentialsError::Unreadable`] when the file is present but
    /// unreadable, which is the usual outcome for a non-privileged process
    /// since `env.qpu` is installed mode `0400` owned by the LSF
    /// administrator.
    pub fn load_env_qpu() -> Result<(Self, PathBuf), CredentialsError> {
        let directory = std::env::var_os("LSF_ENVDIR")
            .map(PathBuf::from)
            .ok_or(CredentialsError::NoEnvDir)?;

        let path = directory.join(ENV_QPU_FILE);
        let credentials = Self::load(&path)?;

        Ok((credentials, path))
    }

    /// Look up a value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Look up a value, treating empty as absent.
    ///
    /// The Python checks are all `if not config.get(name)`, which rejects
    /// `""` as well as a missing key. A blank API key is a configuration
    /// error, not a credential, so this is the form the validators use.
    #[must_use]
    pub fn get_nonempty(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|value| !value.is_empty())
    }

    /// Insert or replace a value.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// Iterate over the entries, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Entries whose key starts with `prefix`, in key order.
    pub fn with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        self.iter().filter(move |(key, _)| key.starts_with(prefix))
    }

    /// Keys whose value contains an unexpanded `${...}` reference.
    ///
    /// See the note on interpolation in [`Credentials::parse`].
    #[must_use]
    pub fn has_interpolation(&self) -> Vec<&str> {
        self.iter()
            .filter(|(_, value)| value.contains("${"))
            .map(|(key, _)| key)
            .collect()
    }

    /// Check that every variable in [`REQUIRED_VARS`] is present and
    /// non-empty.
    ///
    /// Equivalent to the Python `validate_env_vars_from_config()`. No token
    /// is generated: QRMI acquires and renews the IAM bearer token
    /// internally.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialsError::MissingVars`] naming every absent
    /// variable, rather than only the first, so an operator can fix the file
    /// in one pass.
    pub fn validate_required(&self) -> Result<(), CredentialsError> {
        let missing: Vec<String> = REQUIRED_VARS
            .iter()
            .filter(|name| self.get_nonempty(name).is_none())
            .map(|name| (*name).to_owned())
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(CredentialsError::MissingVars {
                names: missing,
                device: None,
            })
        }
    }

    /// Resolve the device-prefixed QRMI settings the ELIM must export.
    ///
    /// QRMI resolves its settings per resource, from
    /// `<resource_id>_<SETTING>` -- for `ibm_marrakesh` it reads
    /// `ibm_marrakesh_QRMI_IBM_QCS_ENDPOINT` and never the bare name. There
    /// is no fallback, so the prefixed form has to be produced explicitly.
    ///
    /// Each setting is resolved by trying, in order:
    ///
    /// 1. `<device>_QRMI_IBM_QCS_*` — already prefixed, used as-is;
    /// 2. `QRMI_IBM_QCS_*` — unprefixed, promoted;
    /// 3. the legacy `APIKEY` / `CRN` names, mapped then promoted;
    /// 4. for the two endpoints only, the public defaults.
    ///
    /// Returns the variables to export, keyed by their prefixed name.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialsError::MissingVars`] if a setting cannot be
    /// resolved by any of those routes.
    pub fn resolve_for_device(&self, device: &str) -> Result<Credentials, CredentialsError> {
        let mut resolved = Credentials::default();
        let mut missing = Vec::new();

        for name in REQUIRED_VARS {
            let prefixed = format!("{device}_{name}");

            // 1. An already-prefixed value wins outright.
            if let Some(value) = self.get_nonempty(&prefixed) {
                resolved.insert(prefixed, value);
                continue;
            }

            // 2. The unprefixed spelling, promoted.
            // 3. A legacy alias for this setting, promoted.
            // 4. An endpoint default.
            let value = self
                .get_nonempty(name)
                .or_else(|| {
                    LEGACY_ALIASES
                        .iter()
                        .find(|(_, qrmi_name)| *qrmi_name == name)
                        .and_then(|(legacy, _)| self.get_nonempty(legacy))
                })
                .or_else(|| {
                    ENDPOINT_DEFAULTS
                        .iter()
                        .find(|(endpoint, _)| *endpoint == name)
                        .map(|(_, default)| *default)
                });

            match value {
                Some(value) => resolved.insert(prefixed, value),
                None => missing.push(name.to_owned()),
            }
        }

        if missing.is_empty() {
            Ok(resolved)
        } else {
            Err(CredentialsError::MissingVars {
                names: missing,
                device: Some(device.to_owned()),
            })
        }
    }
}

impl FromIterator<(String, String)> for Credentials {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self {
            values: iter.into_iter().collect(),
        }
    }
}

/// Strip one matching pair of surrounding quotes, if present.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();

    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if first == last && (first == b'"' || first == b'\'') {
            return value[1..value.len() - 1].to_owned();
        }
    }

    value.to_owned()
}

/// A credentials file could not be loaded or was incomplete.
#[derive(Debug)]
pub enum CredentialsError {
    /// `LSF_ENVDIR` is not set, so `env.qpu` cannot be located.
    NoEnvDir,
    /// The file does not exist.
    Missing {
        /// The path that was tried.
        path: PathBuf,
    },
    /// The file exists but could not be read, typically a permission error.
    Unreadable {
        /// The path that was tried.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Required variables were absent or empty.
    MissingVars {
        /// The variables that could not be resolved.
        names: Vec<String>,
        /// The device they were being resolved for, if any.
        device: Option<String>,
    },
}

impl fmt::Display for CredentialsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEnvDir => f.write_str("LSF_ENVDIR is not set, so env.qpu cannot be located"),
            Self::Missing { path } => write!(f, "file {} does not exist", path.display()),
            Self::Unreadable { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            Self::MissingVars { names, device } => {
                write!(f, "missing required variable(s)")?;
                if let Some(device) = device {
                    write!(f, " for {device}")?;
                }
                write!(f, ": {}", names.join(", "))?;
                if let Some(device) = device {
                    write!(f, " (accepted either as <name> or {device}_<name>)")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for CredentialsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_env_qpu_shape() {
        // Exactly the file the README tells administrators to create.
        let credentials = Credentials::parse(
            "QRMI_IBM_QCS_IAM_APIKEY=abc123\n\
             QRMI_IBM_QCS_SERVICE_CRN=crn:v1:bluemix:public:quantum\n\
             QRMI_IBM_QCS_ENDPOINT=https://quantum.cloud.ibm.com/api/v1\n\
             QRMI_IBM_QCS_IAM_ENDPOINT=https://iam.cloud.ibm.com\n",
        );

        assert_eq!(credentials.len(), 4);
        assert_eq!(credentials.get("QRMI_IBM_QCS_IAM_APIKEY"), Some("abc123"));
        assert!(credentials.validate_required().is_ok());
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let credentials = Credentials::parse(
            "# a comment\n\
             \n\
             KEY=value\n\
                # indented comment\n\
             \t\n",
        );
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials.get("KEY"), Some("value"));
    }

    #[test]
    fn strips_quotes_and_export_prefix() {
        let credentials =
            Credentials::parse("A=\"double\"\nB='single'\nexport C=exported\nD=un\"even\n");
        assert_eq!(credentials.get("A"), Some("double"));
        assert_eq!(credentials.get("B"), Some("single"));
        assert_eq!(credentials.get("C"), Some("exported"));
        // Mismatched quotes are left alone rather than half-stripped.
        assert_eq!(credentials.get("D"), Some("un\"even"));
    }

    #[test]
    fn values_may_contain_equals_signs() {
        // CRNs and base64 keys contain '=' and must survive intact.
        let credentials = Credentials::parse("CRN=crn:v1:a=b=c\nKEY=eyJhbGc=\n");
        assert_eq!(credentials.get("CRN"), Some("crn:v1:a=b=c"));
        assert_eq!(credentials.get("KEY"), Some("eyJhbGc="));
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        // An ELIM that exits takes load reporting down with it, so tolerance
        // here is deliberate.
        let credentials = Credentials::parse("GOOD=1\nthis line has no equals\n=novalue\nALSO=2\n");
        assert_eq!(credentials.len(), 2);
        assert_eq!(credentials.get("GOOD"), Some("1"));
        assert_eq!(credentials.get("ALSO"), Some("2"));
    }

    #[test]
    fn empty_values_count_as_absent_for_validation() {
        let credentials = Credentials::parse(
            "QRMI_IBM_QCS_IAM_APIKEY=\n\
             QRMI_IBM_QCS_SERVICE_CRN=crn\n\
             QRMI_IBM_QCS_ENDPOINT=e\n\
             QRMI_IBM_QCS_IAM_ENDPOINT=i\n",
        );

        assert_eq!(credentials.get("QRMI_IBM_QCS_IAM_APIKEY"), Some(""));
        assert_eq!(credentials.get_nonempty("QRMI_IBM_QCS_IAM_APIKEY"), None);

        let error = credentials
            .validate_required()
            .expect_err("blank key is invalid");
        assert!(error.to_string().contains("QRMI_IBM_QCS_IAM_APIKEY"));
    }

    #[test]
    fn validation_names_every_missing_variable() {
        let credentials = Credentials::parse("QRMI_IBM_QCS_ENDPOINT=e\n");
        let error = credentials.validate_required().expect_err("incomplete");
        let rendered = error.to_string();

        assert!(rendered.contains("QRMI_IBM_QCS_IAM_APIKEY"), "{rendered}");
        assert!(rendered.contains("QRMI_IBM_QCS_SERVICE_CRN"), "{rendered}");
        assert!(rendered.contains("QRMI_IBM_QCS_IAM_ENDPOINT"), "{rendered}");
    }

    // ---- resolve_for_device: the three-way spelling resolution ----

    #[test]
    fn prefixed_values_win() {
        let credentials = Credentials::parse(
            "ibm_fez_QRMI_IBM_QCS_IAM_APIKEY=prefixed\n\
             QRMI_IBM_QCS_IAM_APIKEY=unprefixed\n\
             QRMI_IBM_QCS_SERVICE_CRN=crn\n",
        );

        let resolved = credentials.resolve_for_device("ibm_fez").expect("resolves");
        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"),
            Some("prefixed")
        );
    }

    #[test]
    fn unprefixed_values_are_promoted() {
        let credentials =
            Credentials::parse("QRMI_IBM_QCS_IAM_APIKEY=key\nQRMI_IBM_QCS_SERVICE_CRN=crn\n");

        let resolved = credentials
            .resolve_for_device("ibm_marrakesh")
            .expect("resolves");
        assert_eq!(
            resolved.get("ibm_marrakesh_QRMI_IBM_QCS_IAM_APIKEY"),
            Some("key")
        );
        assert_eq!(
            resolved.get("ibm_marrakesh_QRMI_IBM_QCS_SERVICE_CRN"),
            Some("crn")
        );
    }

    /// The legacy spelling plus the endpoint defaults: the minimal env.qpu an
    /// older deployment would have.
    #[test]
    fn legacy_aliases_and_endpoint_defaults() {
        let credentials = Credentials::parse("APIKEY=legacy_key\nCRN=legacy_crn\n");

        let resolved = credentials.resolve_for_device("ibm_fez").expect("resolves");

        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"),
            Some("legacy_key")
        );
        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_SERVICE_CRN"),
            Some("legacy_crn")
        );
        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_ENDPOINT"),
            Some("https://quantum.cloud.ibm.com/api/v1")
        );
        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_IAM_ENDPOINT"),
            Some("https://iam.cloud.ibm.com")
        );
        assert_eq!(resolved.len(), 4);
    }

    /// An explicit QRMI name beats the legacy alias for the same setting.
    #[test]
    fn qrmi_name_beats_legacy_alias() {
        let credentials =
            Credentials::parse("APIKEY=legacy\nQRMI_IBM_QCS_IAM_APIKEY=modern\nCRN=crn\n");

        let resolved = credentials.resolve_for_device("ibm_fez").expect("resolves");
        assert_eq!(
            resolved.get("ibm_fez_QRMI_IBM_QCS_IAM_APIKEY"),
            Some("modern")
        );
    }

    /// The endpoints have defaults; the key and CRN do not.
    #[test]
    fn unresolvable_credentials_are_reported() {
        let credentials = Credentials::parse("QRMI_IBM_QCS_ENDPOINT=e\n");
        let error = credentials
            .resolve_for_device("ibm_fez")
            .expect_err("no key or CRN");
        let rendered = error.to_string();

        assert!(rendered.contains("QRMI_IBM_QCS_IAM_APIKEY"), "{rendered}");
        assert!(rendered.contains("QRMI_IBM_QCS_SERVICE_CRN"), "{rendered}");
        assert!(rendered.contains("ibm_fez"), "names the device: {rendered}");
        // The Python error spells out both accepted spellings; keep that.
        assert!(rendered.contains("ibm_fez_<name>"), "{rendered}");
    }

    #[test]
    fn resolution_emits_only_prefixed_names() {
        let credentials = Credentials::parse("APIKEY=k\nCRN=c\n");
        let resolved = credentials.resolve_for_device("ibm_fez").expect("resolves");

        for (key, _) in resolved.iter() {
            assert!(key.starts_with("ibm_fez_"), "{key} is not device-scoped");
        }
    }

    #[test]
    fn interpolation_is_reported_not_expanded() {
        let credentials = Credentials::parse("A=${HOME}/x\nB=plain\n");
        assert_eq!(credentials.get("A"), Some("${HOME}/x"));
        assert_eq!(credentials.has_interpolation(), vec!["A"]);
    }

    #[test]
    fn prefix_filter_selects_qrmi_vars() {
        let credentials = Credentials::parse("QRMI_A=1\nQRMI_B=2\nOTHER=3\n");
        let selected: Vec<_> = credentials.with_prefix("QRMI_").collect();
        assert_eq!(selected, vec![("QRMI_A", "1"), ("QRMI_B", "2")]);
    }

    #[test]
    fn missing_file_is_distinguished_from_unreadable() {
        let error = Credentials::load("/nonexistent/path/to/.env").expect_err("missing");
        assert!(matches!(error, CredentialsError::Missing { .. }));
        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn loads_from_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".env");
        std::fs::write(&path, "QRMI_IBM_QCS_IAM_APIKEY=from_disk\n").expect("write");

        let credentials = Credentials::load(&path).expect("loads");
        assert_eq!(
            credentials.get("QRMI_IBM_QCS_IAM_APIKEY"),
            Some("from_disk")
        );
    }

    #[test]
    fn env_qpu_filename_is_the_contracted_one() {
        // Installed as $LSF_ENVDIR/env.qpu by the administrator.
        assert_eq!(ENV_QPU_FILE, "env.qpu");
    }

    #[test]
    fn missing_env_dir_is_reported_distinctly() {
        // LSF_ENVDIR is process-global state, so this test does not mutate
        // it; it checks the error's own wording instead, which is what an
        // operator sees.
        let error = CredentialsError::NoEnvDir;
        assert!(error.to_string().contains("LSF_ENVDIR"), "{error}");
        assert!(error.to_string().contains("env.qpu"), "{error}");
    }

    #[test]
    fn iteration_is_deterministic() {
        // esub serialises these into LSF_SUB4_SUB_ENV_VARS; a stable order
        // keeps that output reproducible.
        let credentials = Credentials::parse("Z=1\nA=2\nM=3\n");
        let keys: Vec<_> = credentials.iter().map(|(key, _)| key).collect();
        assert_eq!(keys, vec!["A", "M", "Z"]);
    }
}
