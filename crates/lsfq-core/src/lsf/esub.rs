// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! The `esub` side of the LSF submission protocol.
//!
//! An `esub` communicates with LSF by appending assignments to the file named
//! by `$LSB_SUB_MODIFY_FILE`. The only directive this project uses is
//! `LSF_SUB4_SUB_ENV_VARS`, which injects environment variables into the
//! submitted job.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Builds the `LSF_SUB4_SUB_ENV_VARS` assignment.
///
/// The variables are serialised as a single comma-separated, double-quoted
/// value, which is the format LSF parses:
///
/// ```text
/// LSF_SUB4_SUB_ENV_VARS="KEY=value,OTHER=value,"
/// ```
///
/// The trailing comma before the closing quote is what the Python
/// implementation emits (it appends `key=value,` per entry, then closes the
/// quote), and LSF accepts it. It is reproduced exactly rather than tidied,
/// because this string is the external contract with the scheduler and a
/// deployed cluster is parsing it today.
#[derive(Debug, Clone, Default)]
pub struct EnvVarDirective {
    values: BTreeMap<String, String>,
}

impl EnvVarDirective {
    /// Start an empty directive.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace one variable.
    ///
    /// A value containing a comma or a double quote would corrupt the
    /// directive, so such values are rejected by
    /// [`EnvVarDirective::render`] rather than silently truncating the
    /// job's environment.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.values.insert(key.into(), value.into());
        self
    }

    /// Add every entry of an iterator.
    pub fn extend<K, V, I>(&mut self, entries: I) -> &mut Self
    where
        K: Into<String>,
        V: Into<String>,
        I: IntoIterator<Item = (K, V)>,
    {
        for (key, value) in entries {
            self.set(key, value);
        }
        self
    }

    /// Whether nothing has been set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Number of variables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Render the directive.
    ///
    /// # Errors
    ///
    /// Returns [`EsubError::UnsafeValue`] if a key or value contains a
    /// character that would break the serialisation -- a comma, a double
    /// quote, a newline, or (in a key) an `=`. The Python implementation
    /// concatenates without checking, so such a value would silently corrupt
    /// every variable after it; failing loudly at submission time is the
    /// safer behaviour and does not change what a well-formed request
    /// produces.
    pub fn render(&self) -> Result<String, EsubError> {
        for (key, value) in &self.values {
            Self::check(key, key, true)?;
            Self::check(key, value, false)?;
        }

        let mut rendered = String::from("LSF_SUB4_SUB_ENV_VARS=\"");
        for (key, value) in &self.values {
            rendered.push_str(key);
            rendered.push('=');
            rendered.push_str(value);
            rendered.push(',');
        }
        rendered.push('"');

        Ok(rendered)
    }

    fn check(key: &str, text: &str, is_key: bool) -> Result<(), EsubError> {
        let bad = text.chars().find(|character| {
            matches!(character, ',' | '"' | '\n' | '\r') || (is_key && *character == '=')
        });

        match bad {
            Some(character) => Err(EsubError::UnsafeValue {
                key: key.to_owned(),
                character,
            }),
            None => Ok(()),
        }
    }

    /// Append the directive to the file LSF named in `$LSB_SUB_MODIFY_FILE`.
    ///
    /// # Errors
    ///
    /// Returns [`EsubError::NoModifyFile`] if the variable is unset -- which
    /// means this is not running as an `esub` -- [`EsubError::UnsafeValue`]
    /// as described on [`EnvVarDirective::render`], or
    /// [`EsubError::Write`] if the file cannot be appended to.
    pub fn append_to_modify_file(&self) -> Result<PathBuf, EsubError> {
        let path = env::var_os("LSB_SUB_MODIFY_FILE")
            .map(PathBuf::from)
            .ok_or(EsubError::NoModifyFile)?;

        self.append_to(&path)?;
        Ok(path)
    }

    /// Append the directive to a specific path.
    ///
    /// # Errors
    ///
    /// As [`EnvVarDirective::append_to_modify_file`], minus the missing-variable case.
    pub fn append_to(&self, path: impl AsRef<Path>) -> Result<(), EsubError> {
        let path = path.as_ref();
        let rendered = self.render()?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| EsubError::Write {
                path: path.to_path_buf(),
                source,
            })?;

        // The Python writes the directive with no trailing newline. LSF
        // tolerates either, but a newline is added here so that a second
        // directive appended by another esub in the chain cannot be glued
        // onto this one's closing quote.
        writeln!(file, "{rendered}").map_err(|source| EsubError::Write {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(())
    }
}

/// Writing the `esub` directive failed.
#[derive(Debug)]
pub enum EsubError {
    /// `$LSB_SUB_MODIFY_FILE` was not set, so this is not running as an esub.
    NoModifyFile,
    /// A key or value contains a character that would corrupt the directive.
    UnsafeValue {
        /// The variable involved.
        key: String,
        /// The offending character.
        character: char,
    },
    /// The directive could not be appended.
    Write {
        /// The file being written.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
}

impl fmt::Display for EsubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModifyFile => f.write_str("cannot get LSB_SUB_MODIFY_FILE variable"),
            Self::UnsafeValue { key, character } => write!(
                f,
                "value of {key} contains {character:?}, which cannot be \
                 represented in LSF_SUB4_SUB_ENV_VARS"
            ),
            Self::Write { path, source } => {
                write!(f, "cannot write to {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for EsubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Write { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_documented_format() {
        let mut directive = EnvVarDirective::new();
        directive
            .set("QRMI_IBM_QCS_IAM_APIKEY", "key")
            .set("ESUB_USER_REQ_SELECTOR", "health");

        // Keys are emitted in sorted order, each followed by a comma, with
        // the whole list double-quoted.
        assert_eq!(
            directive.render().expect("renders"),
            "LSF_SUB4_SUB_ENV_VARS=\"ESUB_USER_REQ_SELECTOR=health,QRMI_IBM_QCS_IAM_APIKEY=key,\""
        );
    }

    #[test]
    fn empty_directive_still_renders_validly() {
        let directive = EnvVarDirective::new();
        assert!(directive.is_empty());
        assert_eq!(
            directive.render().expect("renders"),
            "LSF_SUB4_SUB_ENV_VARS=\"\""
        );
    }

    #[test]
    fn later_set_replaces_earlier() {
        let mut directive = EnvVarDirective::new();
        directive.set("K", "first").set("K", "second");
        assert_eq!(directive.len(), 1);
        assert!(directive.render().expect("renders").contains("K=second"));
    }

    #[test]
    fn extend_adds_many() {
        let mut directive = EnvVarDirective::new();
        directive.extend([("A", "1"), ("B", "2")]);
        assert_eq!(directive.len(), 2);
    }

    /// A comma in a value would be read by LSF as a variable separator,
    /// silently corrupting everything after it.
    #[test]
    fn comma_in_value_is_rejected() {
        let mut directive = EnvVarDirective::new();
        directive.set("K", "a,b");

        let error = directive.render().expect_err("comma is unsafe");
        assert!(matches!(
            error,
            EsubError::UnsafeValue { character: ',', .. }
        ));
        assert!(error.to_string().contains('K'));
    }

    #[test]
    fn quote_and_newline_in_value_are_rejected() {
        for bad in ["a\"b", "a\nb", "a\rb"] {
            let mut directive = EnvVarDirective::new();
            directive.set("K", bad);
            assert!(directive.render().is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn equals_in_key_is_rejected() {
        let mut directive = EnvVarDirective::new();
        directive.set("A=B", "v");
        assert!(matches!(
            directive.render().expect_err("key with = is unsafe"),
            EsubError::UnsafeValue { character: '=', .. }
        ));
    }

    /// A value may contain '=' -- CRNs and base64 keys do.
    #[test]
    fn equals_in_value_is_allowed() {
        let mut directive = EnvVarDirective::new();
        directive.set("CRN", "crn:v1:a=b");
        assert!(directive
            .render()
            .expect("renders")
            .contains("CRN=crn:v1:a=b"));
    }

    #[test]
    fn appends_to_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("modify");

        let mut directive = EnvVarDirective::new();
        directive.set("A", "1");
        directive.append_to(&path).expect("appends");

        let mut second = EnvVarDirective::new();
        second.set("B", "2");
        second.append_to(&path).expect("appends again");

        let contents = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = contents.lines().collect();

        // Two directives, each on its own line: appending must not glue the
        // second onto the first's closing quote.
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("A=1"));
        assert!(lines[1].contains("B=2"));
    }

    #[test]
    fn append_reports_an_unwritable_path() {
        let directive = EnvVarDirective::new();
        let error = directive
            .append_to("/nonexistent/directory/modify")
            .expect_err("unwritable");
        assert!(matches!(error, EsubError::Write { .. }));
    }

    #[test]
    fn unsafe_value_is_caught_before_the_file_is_touched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("modify");

        let mut directive = EnvVarDirective::new();
        directive.set("K", "a,b");
        assert!(directive.append_to(&path).is_err());

        // Nothing was created: validation happens before the open.
        assert!(
            !path.exists(),
            "a rejected directive must not create the file"
        );
    }
}
