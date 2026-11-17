// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Translating a child process's fate into an exit status for LSF.
//!
//! The jobstarter runs the real job as a child and must report its outcome
//! upwards so LSF records the job correctly. The shell convention, which LSF
//! expects, is that a process killed by signal `n` exits with `128 + n`.
//!
//! This is given its own module with its own tests because it is a one-line
//! calculation that has already been wrong once: commit `8a3d1ce`
//! ("Preserve signal-derived exit status in QRMI jobstarter") fixed it, and
//! `tests/test_jobstarter_exit_status.py` guards it. The Python expression
//! is `128 - return_code`, which looks like a bug and is not: `subprocess`
//! reports signal death as a *negative* value, so `128 - (-9)` is 137.
//!
//! Rust's `ExitStatus` keeps the code and the signal in separate accessors,
//! so no sign trick is needed -- and the translation becomes explicit rather
//! than arithmetic the next reader has to decode.

/// The offset applied to a signal number to form an exit status.
pub const SIGNAL_OFFSET: i32 = 128;

/// The status to exit with when a child could not be launched at all.
///
/// 126 is the shell's "command found but not executable"; 127 is "not
/// found". The jobstarter cannot always distinguish the two, so it reports
/// 126 for any spawn failure, which is what a shell does for a command it
/// located but could not run.
pub const SPAWN_FAILURE: i32 = 126;

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It exited normally with this code.
    Exited(i32),
    /// It was killed by this signal.
    Signalled(i32),
}

impl Outcome {
    /// Read the outcome from a `std::process::ExitStatus`.
    ///
    /// On Unix the signal is available directly. A status that reports
    /// neither a code nor a signal cannot occur on a waited-for child, but
    /// is mapped to [`Outcome::Exited`] with 0 rather than panicking, since
    /// the job has already run and aborting here would lose its result.
    #[must_use]
    pub fn from_status(status: &std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signalled(signal);
            }
        }

        Self::Exited(status.code().unwrap_or(0))
    }

    /// The exit status to pass to LSF.
    ///
    /// # Example
    ///
    /// ```
    /// use lsfq_core::exit::Outcome;
    ///
    /// assert_eq!(Outcome::Exited(0).to_exit_status(), 0);
    /// assert_eq!(Outcome::Exited(3).to_exit_status(), 3);
    ///
    /// // SIGKILL is 9, so 128 + 9.
    /// assert_eq!(Outcome::Signalled(9).to_exit_status(), 137);
    /// ```
    #[must_use]
    pub const fn to_exit_status(self) -> i32 {
        match self {
            Self::Exited(code) => code,
            Self::Signalled(signal) => SIGNAL_OFFSET + signal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_exits_pass_through() {
        assert_eq!(Outcome::Exited(0).to_exit_status(), 0);
        assert_eq!(Outcome::Exited(1).to_exit_status(), 1);
        assert_eq!(Outcome::Exited(42).to_exit_status(), 42);
        assert_eq!(Outcome::Exited(255).to_exit_status(), 255);
    }

    /// The conversion the Python expresses as `128 - (-signal)`.
    #[test]
    fn signals_become_128_plus_signal() {
        assert_eq!(Outcome::Signalled(1).to_exit_status(), 129); // SIGHUP
        assert_eq!(Outcome::Signalled(2).to_exit_status(), 130); // SIGINT
        assert_eq!(Outcome::Signalled(9).to_exit_status(), 137); // SIGKILL
        assert_eq!(Outcome::Signalled(15).to_exit_status(), 143); // SIGTERM
    }

    /// The regression commit 8a3d1ce exists because this was once wrong.
    #[test]
    fn sigkill_is_137_not_119() {
        // 119 is what `128 - 9` yields if the sign convention is misread.
        assert_eq!(Outcome::Signalled(9).to_exit_status(), 137);
        assert_ne!(Outcome::Signalled(9).to_exit_status(), 119);
    }

    #[cfg(unix)]
    #[test]
    fn reads_a_real_childs_normal_exit() {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .status()
            .expect("runs");

        assert_eq!(Outcome::from_status(&status), Outcome::Exited(7));
        assert_eq!(Outcome::from_status(&status).to_exit_status(), 7);
    }

    #[cfg(unix)]
    #[test]
    fn reads_a_real_childs_signal_death() {
        // The shell kills itself with SIGKILL.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("kill -9 $$")
            .status()
            .expect("runs");

        assert_eq!(Outcome::from_status(&status), Outcome::Signalled(9));
        assert_eq!(Outcome::from_status(&status).to_exit_status(), 137);
    }

    #[test]
    fn spawn_failure_status_is_the_shell_convention() {
        assert_eq!(SPAWN_FAILURE, 126);
    }
}
