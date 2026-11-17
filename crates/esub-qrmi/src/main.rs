// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! `esub.qrmi` --- an LSF submission filter for quantum jobs.
//!
//! Runs on the submission host, inside `bsub`. It parses the
//! `-a "qrmi(...)"` argument list, reads the user's QRMI credentials, and
//! appends one `LSF_SUB4_SUB_ENV_VARS` directive to
//! `$LSB_SUB_MODIFY_FILE` --- which is how LSF injects those values into the
//! job's environment, where `jobstarter.qrmi` reads them back.
//!
//! # Why this one is migrated last
//!
//! An `esub` runs in the submission path. A defect here does not degrade a
//! metric or mis-place one job: it rejects the submission outright, or
//! admits it with a malformed environment that fails later on the execution
//! host. By this point the request parser and the selection library it
//! depends on have already been exercised in production by the ELIM and the
//! jobstarter.
//!
//! The corollary is that this program does as little as possible. It makes
//! no network calls, builds no QRMI resource, and needs no async runtime ---
//! it parses, reads one file, and writes one line.

mod args;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lsfq_core::lsf::esub::EnvVarDirective;
use lsfq_core::{esub_vars, Credentials, Diagnostics, Program};

fn main() -> ExitCode {
    let diagnostics = Diagnostics::from_env(Program::Qrmi);

    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // `esub.qrmi` with no arguments, or -h/--help, prints usage and
    // succeeds. LSF itself never invokes it that way; a user does, to read
    // the syntax.
    if arguments.is_empty() || arguments.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }

    match run(&diagnostics, &arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            diagnostics.error(&message);

            // LSF names the status that rejects a submission in
            // LSB_SUB_ABORT_VALUE; using anything else can leave the job
            // queued in a state the user did not ask for.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            ExitCode::from(diagnostics.abort_status() as u8)
        }
    }
}

/// Parse, read the credentials, and write the directive.
fn run(diagnostics: &Diagnostics, arguments: &[String]) -> Result<(), String> {
    let request = args::parse(arguments).map_err(|error| error.to_string())?;

    diagnostics.debug_with("Selector :", &request.selector);
    diagnostics.debug_with("Credentials file:", &request.file.display());
    if let Some(device) = &request.device {
        diagnostics.debug_with("Requested device:", device);
    }
    if let Some(qpu) = &request.qpu {
        diagnostics.debug_with("Requested attributes:", &qpu.to_lsf_string());
    }

    let path = resolve_credentials_path(&request.file);
    let credentials = Credentials::load(&path).map_err(|error| error.to_string())?;

    if credentials.is_empty() {
        return Err(format!("cannot read credentials file {}", path.display()));
    }

    // The esub cannot verify these -- it never contacts the service -- but a
    // missing resource type would strand the job on the execution host, so
    // it is worth checking the one variable the jobstarter cannot default.
    if credentials.get_nonempty(esub_vars::QPU_TYPES).is_none() {
        return Err(format!(
            "{} does not define {}; see the README for the template",
            path.display(),
            esub_vars::QPU_TYPES,
        ));
    }

    let unexpanded = credentials.has_interpolation();
    if !unexpanded.is_empty() {
        diagnostics.warn(&format!(
            "{} contains unexpanded ${{...}} reference(s) in: {}; \
             variable interpolation is not performed",
            path.display(),
            unexpanded.join(", "),
        ));
    }

    let mut directive = EnvVarDirective::new();

    // Everything from the credentials file, verbatim: the file carries the
    // QRMI_* provider settings and the job's resource type.
    directive.extend(credentials.iter());

    // The request, under the names the jobstarter reads. `file=` is
    // deliberately not forwarded: it names a path on the submission host,
    // which may not exist on the execution host.
    directive.set(esub_vars::SELECTOR, &request.selector);

    if let Some(device) = &request.device {
        directive.set(esub_vars::DEVICE, device);
    }
    if let Some(qpu) = &request.qpu {
        directive.set(esub_vars::QPU, qpu.to_lsf_string());
    }

    // Carried through so the jobstarter logs at the same level, which is
    // what makes `export LSF_QRMI_DEBUG=level1; bsub ...` useful.
    if diagnostics.level().is_enabled() {
        let level = if diagnostics.level().is_verbose() {
            "level2"
        } else {
            "level1"
        };
        directive.set("LSF_QRMI_DEBUG", level);
    }

    let rendered = directive.render().map_err(|error| error.to_string())?;
    diagnostics.debug_with("lsf_var:", &rendered);

    let written = directive
        .append_to_modify_file()
        .map_err(|error| error.to_string())?;

    diagnostics.debug_with(
        "Passed creds and requests to a jobstarter via",
        &written.display(),
    );

    Ok(())
}

/// Resolve the credentials path.
///
/// The README is explicit: "esub.qrmi expects a short file name for QRMI
/// templates and assumes that it is in $CWD." The Python implements that by
/// building `os.getcwd() + "/" + cfile.name` --- note `.name`, which
/// discards any directory the user gave, so `file=../secrets/.env` silently
/// becomes `$CWD/.env`.
///
/// That truncation is not reproduced. Silently reading a different file than
/// the one named is the kind of surprise that is worse than an error,
/// especially for credentials: a user who writes a path and gets `$CWD/.env`
/// may ship the wrong key without noticing. A relative path is instead
/// resolved against `$CWD`, which is identical for the documented
/// bare-filename case and honours anything else the user wrote.
fn resolve_credentials_path(file: &Path) -> PathBuf {
    if file.is_absolute() {
        return file.to_path_buf();
    }

    match std::env::current_dir() {
        Ok(directory) => directory.join(file),
        // Without a working directory the relative path is all there is.
        Err(_) => file.to_path_buf(),
    }
}

/// Print the usage text.
///
/// Goes to stdout, unlike every diagnostic: this is the response to
/// `--help`, not an error. The Python prints it with `print()` too.
fn print_help() {
    let mut stdout = io::stdout().lock();

    let _ = writeln!(
        stdout,
        r#"Usage:
  esub.qrmi arguments
  esub.qrmi -h | --help

Arguments syntax:
            file=<filename>,
            device=<device_name>|qpu.<attribute>=<value>,..,qpu.<attribute>=<value>,
            [selector=basic|health|priority]

Arguments description:
  file                       Path to QRMI credentials, relative to $CWD
  device                     QPU name
  selector                   Selection policy: basic, health or priority
  qpu.qubits                 Minimum number of qubits
  qpu.qpu_version            Required QPU version
  qpu.processor_type         Required processor family
  qpu.clops                  Minimum CLOPS value
  qpu.pending_jobs           Maximum number of pending jobs
  qpu.readout_error_median   Maximum median readout error
  qpu.sx_error_median        Maximum median SX error
  qpu.cz_error_median        Maximum median CZ error
  qpu.T1_median_us           Minimum T1 median, microseconds
  qpu.T2_median_us           Minimum T2 median, microseconds

Note: device and qpu arguments are mutually exclusive
Note: selector cannot be combined with device

Examples:
  bsub -a "qrmi(file=.env,qpu.qubits=120,qpu.processor_type=Nighthawk)" my_app
  bsub -a "qrmi(file=.env,device=ibm_marrakesh)" my_app
  bsub -a "qrmi(file=.env,qpu.qubits=120,selector=priority)" my_app"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_filename_resolves_against_the_working_directory() {
        let resolved = resolve_credentials_path(Path::new(".env"));
        let expected = std::env::current_dir().expect("cwd").join(".env");
        assert_eq!(resolved, expected);
    }

    /// Unlike the Python, a directory component is honoured rather than
    /// discarded; see `resolve_credentials_path`.
    #[test]
    fn a_relative_path_keeps_its_directory() {
        let resolved = resolve_credentials_path(Path::new("secrets/.env"));
        let expected = std::env::current_dir().expect("cwd").join("secrets/.env");
        assert_eq!(resolved, expected);
        assert!(
            resolved.ends_with("secrets/.env"),
            "the directory must not be truncated away: {resolved:?}"
        );
    }

    #[test]
    fn an_absolute_path_is_used_as_given() {
        let resolved = resolve_credentials_path(Path::new("/etc/qrmi/.env"));
        assert_eq!(resolved, PathBuf::from("/etc/qrmi/.env"));
    }

    #[test]
    fn help_text_covers_every_accepted_attribute() {
        // A user following the help must be able to submit successfully, so
        // every attribute it advertises has to parse.
        let help = r"qubits qpu_version processor_type clops pending_jobs
            readout_error_median sx_error_median cz_error_median
            T1_median_us T2_median_us";

        for name in help.split_whitespace() {
            let value = if name.contains("version") || name.contains("processor") {
                "x"
            } else {
                "1"
            };
            args::parse(["file=.env", &format!("qpu.{name}={value}")])
                .unwrap_or_else(|error| panic!("help advertises qpu.{name}: {error}"));
        }
    }
}
