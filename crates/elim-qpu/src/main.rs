// (C) Copyright 2025-2026 IBM. All Rights Reserved.
//
// This code is licensed under the Apache License, Version 2.0. You may
// obtain a copy of this license in the LICENSE.txt file in the root directory
// of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

//! `elim.qpu` --- an LSF External Load Information Manager for IBM Quantum
//! systems.
//!
//! Reports a QPU's attributes and queue depth to LSF as load indices, so the
//! scheduler can make placement and throttling decisions for jobs that
//! target quantum resources.
//!
//! Installed as `$LSF_SERVERDIR/elim.qpu`, started by `lim`, and expected to
//! run for as long as the host is up: one line on stdout per interval,
//! forever.
//!
//! # Operating contract
//!
//! Three properties follow from how LSF treats an ELIM, and each shapes the
//! code:
//!
//! * **stdout is the protocol.** Nothing else may be written there, so every
//!   diagnostic goes to stderr.
//! * **Exiting is a failure, not a shutdown.** When an ELIM stops, LSF loses
//!   *every* index for that host, not just the one that broke. Only a
//!   configuration error -- something that cannot be fixed by waiting -- is
//!   fatal; a transport failure degrades the line and the loop continues.
//! * **The line is fixed-width.** LSF parses by field count, so a cycle that
//!   fetched nothing still emits ten pairs, with `-` for each value.

mod poll;
mod qrmi_source;

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use lsfq_core::lsf::hostres;
use lsfq_core::{Credentials, Diagnostics, Program};

use poll::{indices_from, QpuSource};
use qrmi_source::QrmiSource;

/// How long to wait between reports.
///
/// Matches the original's `sleep_time = 10`. LSF's own sampling interval is
/// independent; reporting faster than LSF reads gains nothing and spends
/// API quota.
const INTERVAL: Duration = Duration::from_secs(10);

/// Exit status for a configuration error.
///
/// Only reached before the loop starts. Once reporting has begun the process
/// does not exit voluntarily.
const CONFIG_ERROR: u8 = 1;

fn main() -> ExitCode {
    let diagnostics = Diagnostics::from_env(Program::Elim);

    // A single-threaded runtime is enough: the two per-cycle queries are
    // joined concurrently, which is I/O concurrency, not parallelism.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            diagnostics.error(&format!("cannot start async runtime: {error}"));
            return ExitCode::from(CONFIG_ERROR);
        }
    };

    match start(&diagnostics) {
        Ok(mut source) => {
            runtime.block_on(report_forever(&diagnostics, &mut source));
            // Reached only on a shutdown signal.
            ExitCode::SUCCESS
        }
        Err(message) => {
            diagnostics.error(&message);
            ExitCode::from(CONFIG_ERROR)
        }
    }
}

/// Resolve the host's QPU, load its credentials, and build the source.
///
/// Everything here is a configuration error if it fails: waiting will not
/// fix a missing `env.qpu` or an unmapped host, so these are the only fatal
/// conditions.
fn start(diagnostics: &Diagnostics) -> Result<QrmiSource, String> {
    // 1. Which QPU does this host serve? LSF's host configuration says so.
    let device = hostres::discover_qpu().map_err(|error| error.to_string())?;
    diagnostics.debug_with("My QPU =", &device);

    // A host mapped to several QPUs is a misconfiguration that the original
    // resolves silently to whichever came first; say so instead.
    if let Ok(hostname) = hostres::hostname() {
        if let Ok(output) = std::process::Command::new("lshosts")
            .args(["-o", "RESOURCES", &hostname])
            .output()
        {
            let resources = hostres::parse_all_resources(&String::from_utf8_lossy(&output.stdout));
            if resources.len() > 1 {
                diagnostics.warn(&format!(
                    "{hostname} is mapped to {} QPU resources ({}); reporting only {device}",
                    resources.len(),
                    resources.join(", "),
                ));
            }
        }
    }

    // 2. Load $LSF_ENVDIR/env.qpu and resolve the device-scoped settings.
    let (credentials, path) = Credentials::load_env_qpu().map_err(|error| error.to_string())?;
    diagnostics.debug_with("Found:", &path.display());

    let unexpanded = credentials.has_interpolation();
    if !unexpanded.is_empty() {
        diagnostics.warn(&format!(
            "{} contains unexpanded ${{...}} reference(s) in: {}; \
             variable interpolation is not performed",
            path.display(),
            unexpanded.join(", "),
        ));
    }

    let resolved = credentials
        .resolve_for_device(&device)
        .map_err(|error| format!("{}: {error}", path.display()))?;

    // 3. Export them. QRMI reads its configuration from the process
    //    environment, under the resource-scoped names.
    for (key, value) in resolved.iter() {
        diagnostics.debug_with("Set", &key);
        // SAFETY-adjacent note: this runs before the runtime spawns any
        // task and before the reporting loop, so no other thread can be
        // reading the environment concurrently.
        std::env::set_var(key, value);
    }

    // 4. Build the resource once and reuse it; QRMI renews the token itself.
    let source = QrmiSource::new(&device)?;
    diagnostics.debug_with("Created QRMI resource for", &device);

    Ok(source)
}

/// Report one line per [`INTERVAL`], until a shutdown signal arrives.
async fn report_forever(diagnostics: &Diagnostics, source: &mut impl QpuSource) {
    let mut shutdown = shutdown_signal();

    loop {
        let (reading, poll_warnings) = source.poll().await;
        let (indices, index_warnings) = indices_from(&reading);

        for warning in poll_warnings.iter().chain(index_warnings.iter()) {
            // Transport failures are expected over a long run and are
            // already visible as `-` in the line, so they are reported at
            // debug level rather than as warnings on every cycle -- an ELIM
            // runs for months, and a warning per cycle during an outage
            // would bury the log.
            diagnostics.debug(warning);
        }

        // The protocol: one line, stdout, flushed. A failed write means the
        // pipe to `lim` is gone, which no amount of retrying fixes.
        let line = indices.render();
        if let Err(error) = write_line(&line) {
            diagnostics.error(&format!("cannot write to stdout: {error}"));
            return;
        }

        diagnostics.trace(&format!("reported: {line}"));

        tokio::select! {
            () = tokio::time::sleep(INTERVAL) => {}
            () = &mut shutdown => {
                diagnostics.debug("shutting down");
                return;
            }
        }
    }
}

/// Write one ELIM line and flush it.
fn write_line(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

/// Resolves when the process is asked to stop.
///
/// `lim` stops an ELIM with `SIGTERM`. Handling it lets the loop finish the
/// current cycle and exit cleanly instead of being killed mid-write, which
/// could leave LSF a truncated line.
fn shutdown_signal() -> std::pin::Pin<Box<dyn core::future::Future<Output = ()> + Send>> {
    Box::pin(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};

            // Without a handler the default disposition applies, which is
            // what the original relies on.
            let Ok(mut terminate) = signal(SignalKind::terminate()) else {
                return core::future::pending().await;
            };
            let Ok(mut interrupt) = signal(SignalKind::interrupt()) else {
                return core::future::pending().await;
            };

            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        }

        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_matches_the_original() {
        assert_eq!(INTERVAL, Duration::from_secs(10));
    }

    #[test]
    fn write_line_emits_exactly_one_line() {
        // The protocol is one line per cycle; render() must not already
        // contain a newline, or each cycle would emit two.
        let line = lsfq_core::lsf::elim::LoadIndices::unavailable().render();
        assert!(!line.contains('\n'), "render must not include a newline");
        assert_eq!(line.lines().count(), 1);
    }
}
