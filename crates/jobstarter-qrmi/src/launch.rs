// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! Running the job and reporting its outcome.
//!
//! Split out from `main` so the exit-status translation can be tested against
//! real child processes without needing a QPU. That translation has already
//! been wrong once --- commit `8a3d1ce`, "Preserve signal-derived exit status
//! in QRMI jobstarter" --- and `tests/test_jobstarter_exit_status.py` guards
//! it in the Python, so the Rust port keeps equivalent coverage.

use std::ffi::OsString;
use std::process::Command;

use lsfq_core::{EnvMap, Outcome};

/// The job to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The program and its arguments, as given to the jobstarter.
    pub argv: Vec<OsString>,
}

impl Job {
    /// Take the job from this process's arguments.
    ///
    /// LSF invokes a jobstarter as `jobstarter.qrmi <command> [args...]`, so
    /// everything after `argv[0]` is the job.
    #[must_use]
    pub fn from_args() -> Self {
        Self {
            argv: std::env::args_os().skip(1).collect(),
        }
    }

    /// Whether no command was given.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.argv.is_empty()
    }
}

/// Run the job with exactly the given environment and report how it ended.
///
/// The environment is applied with `env_clear()` first, so the child receives
/// precisely the map that was computed rather than inheriting whatever the
/// jobstarter's own environment happens to hold. That is the whole point of
/// [`lsfq_core::jobenv`]: the variables QRMI injected for backends that were
/// not selected live in this process's environment and would otherwise be
/// passed straight through.
///
/// # Errors
///
/// Returns the spawn error if the command cannot be started. A job that runs
/// and fails is not an error here --- that outcome is the return value.
pub fn run(job: &Job, environment: &EnvMap) -> std::io::Result<Outcome> {
    let (program, arguments) = job
        .argv
        .split_first()
        .expect("caller must reject an empty job");

    let status = Command::new(program)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .status()?;

    Ok(Outcome::from_status(&status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(argv: &[&str]) -> Job {
        Job {
            argv: argv.iter().map(OsString::from).collect(),
        }
    }

    fn env(entries: &[(&str, &str)]) -> EnvMap {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn empty_job_is_detected() {
        assert!(job(&[]).is_empty());
        assert!(!job(&["true"]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn normal_exit_codes_are_propagated() {
        // The same codes the Python regression test checks.
        for code in [0, 1, 42, 143, 255] {
            let outcome = run(
                &job(&["sh", "-c", &format!("exit {code}")]),
                &env(&[("PATH", "/usr/bin:/bin")]),
            )
            .expect("spawns");

            assert_eq!(outcome, Outcome::Exited(code));
            assert_eq!(outcome.to_exit_status(), code);
        }
    }

    /// A job killed by a signal must yield 128 + signal, which is what LSF
    /// records. This is the case commit 8a3d1ce fixed.
    #[cfg(unix)]
    #[test]
    fn signal_deaths_become_128_plus_signal() {
        for (signal, expected) in [(15, 143), (2, 130), (9, 137)] {
            let outcome = run(
                &job(&["sh", "-c", &format!("kill -{signal} $$")]),
                &env(&[("PATH", "/usr/bin:/bin")]),
            )
            .expect("spawns");

            assert_eq!(outcome, Outcome::Signalled(signal), "signal {signal}");
            assert_eq!(outcome.to_exit_status(), expected, "signal {signal}");
        }
    }

    /// Exit code 143 and death by SIGTERM both reach LSF as 143, but they are
    /// distinct outcomes and must not be conflated internally.
    #[cfg(unix)]
    #[test]
    fn exit_143_and_sigterm_are_distinguished() {
        let exited = run(
            &job(&["sh", "-c", "exit 143"]),
            &env(&[("PATH", "/usr/bin:/bin")]),
        )
        .expect("spawns");

        let signalled = run(
            &job(&["sh", "-c", "kill -15 $$"]),
            &env(&[("PATH", "/usr/bin:/bin")]),
        )
        .expect("spawns");

        assert_eq!(exited, Outcome::Exited(143));
        assert_eq!(signalled, Outcome::Signalled(15));
        // Same status to LSF, different causes.
        assert_eq!(exited.to_exit_status(), signalled.to_exit_status());
    }

    /// The child must receive exactly the computed environment: this is what
    /// keeps an unselected backend's QRMI variables away from the job.
    #[cfg(unix)]
    #[test]
    fn the_child_environment_is_exactly_what_was_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("env.txt");

        let environment = env(&[
            ("PATH", "/usr/bin:/bin"),
            ("ibm_fez_QRMI_IBM_QCS_ENDPOINT", "https://selected"),
        ]);

        // A sentinel in the parent's own environment that must NOT be
        // inherited, standing in for a variable QRMI injected for an
        // unselected backend.
        std::env::set_var("JOBSTARTER_TEST_LEAK", "must-not-appear");

        run(
            &job(&["sh", "-c", &format!("env > {}", output.display())]),
            &environment,
        )
        .expect("spawns");

        std::env::remove_var("JOBSTARTER_TEST_LEAK");

        let contents = std::fs::read_to_string(&output).expect("read");

        assert!(
            contents.contains("ibm_fez_QRMI_IBM_QCS_ENDPOINT=https://selected"),
            "the selected device's variables must reach the job:\n{contents}"
        );
        assert!(
            !contents.contains("JOBSTARTER_TEST_LEAK"),
            "the parent's environment must not leak into the job:\n{contents}"
        );
    }

    #[test]
    fn a_missing_command_is_a_spawn_error() {
        let error = run(
            &job(&["definitely-not-a-real-command-xyz"]),
            &env(&[("PATH", "/usr/bin:/bin")]),
        )
        .expect_err("not found");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn arguments_are_passed_through_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("args.txt");

        run(
            &job(&[
                "sh",
                "-c",
                &format!(r#"printf '%s\n' "$@" > {}"#, output.display()),
                "sh",
                "first arg",
                "second=value",
                "--flag",
            ]),
            &env(&[("PATH", "/usr/bin:/bin")]),
        )
        .expect("spawns");

        let contents = std::fs::read_to_string(&output).expect("read");
        let lines: Vec<&str> = contents.lines().collect();

        // Arguments with spaces and '=' must survive intact.
        assert_eq!(lines, vec!["first arg", "second=value", "--flag"]);
    }
}
