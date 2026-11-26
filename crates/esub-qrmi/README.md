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

## Verification

```bash
cargo test -p esub-qrmi                  # 38 tests
cargo clippy -p esub-qrmi --all-targets  # clean under pedantic
```

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

## Licence

Apache-2.0. See `LICENSE` in the repository root.
