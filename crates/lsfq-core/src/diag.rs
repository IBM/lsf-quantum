// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Diagnostics and fatal-error reporting.
//!
//! One implementation, replacing the `print_debug`/`print_error` pair that is
//! defined separately in `qrmi-esub-jobstarter.py` and in `elim.qpu`.
//!
//! Everything goes to stderr. That is not a stylistic choice: `esub` may not
//! write to stdout (LSF reads it), and `elim`'s stdout *is* the load-index
//! protocol, so a stray line there corrupts the data LSF parses.

use std::env;
use std::fmt;
use std::io::{self, Write};

/// How much debug output to emit.
///
/// The two programs read different variables for this -- `LSF_QRMI_DEBUG` for
/// esub/jobstarter and `LSF_ELIM_DEBUG` for the ELIM -- and both accept only
/// the exact strings `level1` and `level2`; anything else means off. Both
/// quirks are part of the operator-facing contract, so they are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DebugLevel {
    /// No debug output.
    #[default]
    Off,
    /// Progress and decisions.
    Level1,
    /// Level 1 plus full metric and payload dumps.
    Level2,
}

impl DebugLevel {
    /// Parse a level, treating any unrecognised value as [`DebugLevel::Off`].
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("level1") => Self::Level1,
            Some("level2") => Self::Level2,
            _ => Self::Off,
        }
    }

    /// Whether any debug output is enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether full payload dumps are enabled.
    #[must_use]
    pub const fn is_verbose(self) -> bool {
        matches!(self, Self::Level2)
    }
}

/// Which program is reporting, which selects the debug variable it honours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// `esub.qrmi` and `jobstarter.qrmi`, which read `LSF_QRMI_DEBUG`.
    Qrmi,
    /// `elim.qpu`, which reads `LSF_ELIM_DEBUG`.
    Elim,
}

impl Program {
    /// The environment variable this program reads its debug level from.
    #[must_use]
    pub const fn debug_var(self) -> &'static str {
        match self {
            Self::Qrmi => "LSF_QRMI_DEBUG",
            Self::Elim => "LSF_ELIM_DEBUG",
        }
    }
}

/// A diagnostics sink.
///
/// Holds the debug level and the exit status to use for fatal errors, so the
/// callers do not each re-read the environment. Construct once at startup
/// with [`Diagnostics::from_env`].
#[derive(Debug, Clone)]
pub struct Diagnostics {
    level: DebugLevel,
    abort_value: Option<i32>,
}

impl Diagnostics {
    /// Build from the process environment.
    ///
    /// `LSB_SUB_ABORT_VALUE` is read here because LSF sets it for `esub` to
    /// say which exit status rejects the submission; the Python
    /// `print_error()` honours it and falls back to 1.
    #[must_use]
    pub fn from_env(program: Program) -> Self {
        let level = DebugLevel::parse(env::var(program.debug_var()).ok().as_deref());

        let abort_value = env::var("LSB_SUB_ABORT_VALUE")
            .ok()
            .and_then(|value| value.trim().parse::<i32>().ok());

        Self { level, abort_value }
    }

    /// Build explicitly, for tests.
    #[must_use]
    pub const fn new(level: DebugLevel, abort_value: Option<i32>) -> Self {
        Self { level, abort_value }
    }

    /// The configured debug level.
    #[must_use]
    pub const fn level(&self) -> DebugLevel {
        self.level
    }

    /// The exit status a fatal error should use.
    ///
    /// `LSB_SUB_ABORT_VALUE` when LSF provided a parseable one, else 1.
    #[must_use]
    pub fn abort_status(&self) -> i32 {
        self.abort_value.unwrap_or(1)
    }

    /// Emit a `[DEBUG]` line if debug output is on.
    pub fn debug(&self, message: &str) {
        if self.level.is_enabled() {
            Self::emit("[DEBUG]", message);
        }
    }

    /// Emit a `[DEBUG]` line with a trailing value, if debug output is on.
    ///
    /// Matches the Python `print_debug(message, var)` two-argument form.
    pub fn debug_with(&self, message: &str, value: &dyn fmt::Display) {
        if self.level.is_enabled() {
            Self::emit("[DEBUG]", &format!("{message} {value}"));
        }
    }

    /// Emit a `[DEBUG]` line only at [`DebugLevel::Level2`].
    pub fn trace(&self, message: &str) {
        if self.level.is_verbose() {
            Self::emit("[DEBUG]", message);
        }
    }

    /// Emit a `[WARNING]` line. Always shown.
    pub fn warn(&self, message: &str) {
        Self::emit("[WARNING]", message);
    }

    /// Emit an `[ERROR]` line. Always shown.
    ///
    /// This does not exit; the caller decides, usually by returning an error
    /// that `main` turns into [`Diagnostics::abort_status`]. The Python
    /// `print_error()` exits inside the helper, which is why it cannot be
    /// called from anything that needs to clean up first -- notably the
    /// jobstarter, which must still release an acquired resource.
    pub fn error(&self, message: &str) {
        Self::emit("[ERROR]", message);
    }

    fn emit(tag: &str, message: &str) {
        // A failed write to stderr is not worth escalating: the process is
        // usually mid-failure already, and panicking here would replace a
        // clear message with a worse one.
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "{tag} {message}");
    }
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self::new(DebugLevel::Off, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_documented_levels_enable_debug() {
        assert_eq!(DebugLevel::parse(Some("level1")), DebugLevel::Level1);
        assert_eq!(DebugLevel::parse(Some("level2")), DebugLevel::Level2);

        // Everything else is off, including plausible-looking values. The
        // Python code compares against the two literals and assigns None
        // otherwise, so this tolerance is deliberate rather than an oversight
        // to be "fixed".
        for value in ["", "1", "true", "yes", "LEVEL1", "level3", "debug", "on"] {
            assert_eq!(DebugLevel::parse(Some(value)), DebugLevel::Off, "{value:?}");
        }
        assert_eq!(DebugLevel::parse(None), DebugLevel::Off);
    }

    #[test]
    fn level_predicates() {
        assert!(!DebugLevel::Off.is_enabled());
        assert!(DebugLevel::Level1.is_enabled());
        assert!(DebugLevel::Level2.is_enabled());

        assert!(!DebugLevel::Off.is_verbose());
        assert!(!DebugLevel::Level1.is_verbose());
        assert!(DebugLevel::Level2.is_verbose());
    }

    #[test]
    fn each_program_reads_its_own_variable() {
        // These differ in the Python sources and must not be unified.
        assert_eq!(Program::Qrmi.debug_var(), "LSF_QRMI_DEBUG");
        assert_eq!(Program::Elim.debug_var(), "LSF_ELIM_DEBUG");
    }

    #[test]
    fn abort_status_defaults_to_one() {
        let diagnostics = Diagnostics::new(DebugLevel::Off, None);
        assert_eq!(diagnostics.abort_status(), 1);
    }

    #[test]
    fn abort_status_honours_lsf_value() {
        let diagnostics = Diagnostics::new(DebugLevel::Off, Some(99));
        assert_eq!(diagnostics.abort_status(), 99);
    }

    #[test]
    fn levels_are_ordered() {
        assert!(DebugLevel::Level2 > DebugLevel::Level1);
        assert!(DebugLevel::Level1 > DebugLevel::Off);
    }
}
