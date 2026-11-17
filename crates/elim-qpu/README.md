# elim-qpu

An LSF External Load Information Manager (ELIM) for IBM Quantum systems.

Reports a QPU's attributes and queue depth to LSF as load indices, so the
scheduler can make placement and throttling decisions for jobs targeting
quantum resources.

## Install

The binary is built as `elim-qpu` and installed as `elim.qpu`: a Rust crate
name cannot contain a dot, so the rename happens at install time.

```bash
cargo build --release -p elim-qpu
cp target/release/elim-qpu "$LSF_SERVERDIR/elim.qpu"
chmod 755 "$LSF_SERVERDIR/elim.qpu"
```

Then reconfigure LSF, exactly as for the Python version:

```bash
lsadmin reconfig
badmin reconfig
```

Nothing else changes. The load-index names, the field count, the value
formats and `$LSF_ENVDIR/env.qpu` are all unchanged, so no `lsf.shared` or
`lsf.cluster` edit is needed.

## Configuration

Unchanged from the Python implementation:

- **`$LSF_ENVDIR/env.qpu`** — credentials, mode `0400`, owned by the LSF
  administrator. Accepts the `QRMI_IBM_QCS_*` names, the device-prefixed
  form, or the legacy `APIKEY`/`CRN` pair; the two endpoints default to the
  public IBM Quantum Platform URLs.
- **Host → QPU mapping** — taken from the host's `RESOURCES` list via
  `lshosts`, matching the first `ibm_*` resource.
- **`LSF_ELIM_DEBUG`** — `level1` or `level2` for diagnostics on stderr.
  (Note this is a *different* variable from the esub/jobstarter's
  `LSF_QRMI_DEBUG`; both are preserved as they are.)

## Operating contract

Three properties follow from how LSF treats an ELIM, and each shapes the
code:

1. **stdout is the protocol.** Every diagnostic goes to stderr.
2. **Exiting is a failure, not a shutdown.** When an ELIM stops, LSF loses
   *every* index for that host. Only a configuration error — something
   waiting cannot fix — is fatal; a transport failure degrades the line and
   the loop continues.
3. **The line is fixed-width.** LSF parses by field count, so a cycle that
   fetched nothing still emits ten pairs with `-` for each value.

Fatal (exit 1, before reporting starts): no `ibm_*` resource for this host,
`LSF_ENVDIR` unset, `env.qpu` missing or unreadable, credentials
unresolvable, or the QRMI resource cannot be constructed.

Non-fatal (reported at debug level, line degrades): any `status()` or
`target()` failure, an unparseable target payload, missing calibration data.

`SIGTERM` and `SIGINT` finish the current cycle and exit cleanly, rather than
risking a truncated line mid-write.

## One deliberate difference

On a cycle whose `target()` fetch fails, `elim.qpu` does
`print_debug(...); time.sleep(...); continue` — it emits **nothing**. This
implementation emits an all-unavailable line instead.

An ELIM that goes quiet is indistinguishable, from LSF's side, from one that
is merely slow: LSF keeps serving the last values it received, so during an
outage it schedules against calibration and queue figures that may be hours
stale. Reporting `-` tells LSF the values are unavailable, which is what `-`
is for. The field count is unchanged, so no LSF configuration is affected.

Everything else is reproduced exactly, including `processor_type Unknown` for
an unparseable `sample_name` — verified against the original.

## Verification

```bash
cargo test -p elim-qpu                   # 11 tests
cargo clippy -p elim-qpu --all-targets   # clean under pedantic
cargo test -p lsfq-core --test golden    # ELIM lines vs. the original
```

The line formatting is verified **byte-for-byte** against the output
recorded from the original `elim.qpu` (11/11 identical), by the golden-file
test in `lsfq-core`. The tests here cover the reporting
cycle's robustness: a `QpuSource` trait lets them substitute a source that
fails on demand, so the two invariants above are checked without a QPU —
including an exhaustive sweep of every failure combination asserting the line
stays parseable.

This binary has also been run end-to-end against the live IBM Quantum
Platform API with deliberately invalid credentials: it completed startup,
received a real authentication rejection, and **kept emitting well-formed
ten-field lines** across cycles rather than exiting, then shut down cleanly
on `SIGTERM`.

### Toolchain note

QRMI is a git dependency and pulls in the AWS SDK transitively, whose MSRV
currently exceeds this project's declared `1.76`, so building this crate
needs a recent stable toolchain (1.94.1 or newer at the time of writing).
`qpu-select` and the default feature set of `lsfq-core` still build on 1.76.

### macOS: `libiconv` at runtime

QRMI depends unconditionally on `pythonize`/`pyo3`, so the binary links
against the Python discovered at build time. If that is a Conda or miniforge
Python, the resulting binary references `@rpath/libiconv.2.dylib` with no
matching `LC_RPATH` and fails to start:

```
dyld: Library not loaded: @rpath/libiconv.2.dylib
```

Either build against the system Python, or set
`DYLD_FALLBACK_LIBRARY_PATH` to that Python's `lib` directory. This affects
macOS development only; the deployment targets are Linux, where the Python
runtime libraries are on the default loader path.

## Licence

Apache-2.0. See `LICENSE` in the repository root.
