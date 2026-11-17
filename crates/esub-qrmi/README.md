# esub-qrmi

An LSF submission filter (`esub`) for quantum jobs.

Runs on the submission host inside `bsub`. Parses the `-a "qrmi(...)"`
argument list, reads the user's QRMI credentials, and appends one
`LSF_SUB4_SUB_ENV_VARS` directive to `$LSB_SUB_MODIFY_FILE` — which is how
LSF injects those values into the job's environment, where
`jobstarter.qrmi` reads them back.

## Install

Built as `esub-qrmi` and installed as `esub.qrmi`: a Rust crate name cannot
contain a dot, so the rename happens at install time.

```bash
cargo build --release -p esub-qrmi
cp target/release/esub-qrmi "$LSF_SERVERDIR/esub.qrmi"
chmod 755 "$LSF_SERVERDIR/esub.qrmi"
```

No `lsadmin`/`badmin` reconfiguration is needed — LSF resolves an esub by
filename at submission time.

## Usage

Unchanged from the Python:

```bash
bsub -a "qrmi(file=.env,qpu.qubits=120,qpu.processor_type=Nighthawk)" my_app
bsub -a "qrmi(file=.env,device=ibm_marrakesh)" my_app
bsub -a "qrmi(file=.env,qpu.qubits=120,selector=priority)" my_app
```

`file=` names a credentials file relative to `$CWD`. `device=` and `qpu.*`
are mutually exclusive. `selector` is `basic` (default), `health` or
`priority`, and may not be combined with `device=`: naming a device leaves
nothing to select, so a selector given alongside one would be ignored and is
rejected instead. `LSF_QRMI_DEBUG=level1|level2` enables diagnostics on stderr,
and is forwarded so the jobstarter logs at the same level.

## Why this one is deliberately minimal

An esub runs in the submission path. A defect here does not degrade a metric
or mis-place one job: it rejects the submission outright, or admits it with a
malformed environment that fails later on the execution host. So this
program does as little as it can — it makes no network calls, builds no QRMI
resource, and needs no async runtime.

That has a practical consequence: **this crate does not enable the `qrmi`
feature**, so it builds free of the QRMI crate, the AWS SDK and tokio — and
on the workspace's declared MSRV (1.76), unlike `elim-qpu` and
`jobstarter-qrmi`.

On any failure it exits with `$LSB_SUB_ABORT_VALUE` (or 1 if LSF did not set
one) and **writes nothing** to the modify file, so a rejected submission
cannot leave a partial directive behind.

## Verification

```bash
cargo test -p esub-qrmi                  # 38 tests
cargo clippy -p esub-qrmi --all-targets  # clean under pedantic
```

`tests/roundtrip.rs` checks the esub/jobstarter contract directly: every
request string the esub can emit is parsed back with the jobstarter's own
parser, and every selector it accepts is resolved with the jobstarter's own
lookup. A mismatch there would admit a job and then fail it on the execution
host — the failure mode the phase ordering exists to avoid.

The binary has been run end-to-end:

- **Full pipeline.** The directive it wrote was converted into an
  environment the way LSF does and handed to the real `jobstarter.qrmi`
  binary, which resolved the resource type, selected the `priority` policy,
  parsed the request string, and reached the live IBM Quantum Platform API —
  failing only on the deliberately fake credentials.
- **Every error path** (no `file=`, both `device=` and `qpu.*`, neither,
  unknown attribute, unknown selector, zero qubits, missing credentials
  file, absent `$LSB_SUB_MODIFY_FILE`) exits with the abort value and leaves
  no directive behind.
- **`--help` and no arguments** print usage and exit 0.

## Two behaviours worth knowing

### CLI attribute order is normalised (preserved from the Python)

`to_lsf_string()` emits attributes in a fixed order, not the order typed.
The Python inherits this from `dataclasses.fields()`, and it has a visible
consequence: the `priority` selector ranks requirements *in the order of the
request string*, so

```text
qpu.clops=240000,qpu.qubits=120
qpu.qubits=120,qpu.clops=240000
```

both arrive as `qubits=120;clops=240000` and select identically. A user
cannot express "rank on CLOPS first" through the esub.

This is reproduced rather than fixed. Changing it would silently change
which backend a job lands on at every site already using
`selector=priority` — a scheduling change disguised as a refactor. It
belongs in a separate, announced change; the test
`cli_order_does_not_affect_the_request_string` pins the current behaviour so
that change is deliberate when it comes.

### The credentials path is no longer truncated

The README says "esub.qrmi expects a short file name for QRMI templates and
assumes that it is in $CWD". The Python implements that as
`os.getcwd() + "/" + cfile.name` — note `.name`, which **discards any
directory given**, so `file=../secrets/.env` silently reads `$CWD/.env`.

That truncation is not reproduced. Silently reading a different file than
the one named is worse than an error, especially for credentials: a user who
writes a path and gets `$CWD/.env` may ship the wrong API key without
noticing. A relative path is resolved against `$CWD` instead — identical for
the documented bare-filename case, and honouring anything else.

### Attribute spellings

The help text and README advertise `qpu.t1_median_us` in lower case, while
the Python dataclass field — and so the name OmegaConf accepts — is
`T1_median_us`, meaning a user following the documentation is rejected.
Attribute names are matched case-insensitively here so both spellings work;
the canonical form is still what reaches the jobstarter.

## Licence

Apache-2.0. See `LICENSE` in the repository root.
