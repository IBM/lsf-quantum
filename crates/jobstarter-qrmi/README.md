# jobstarter-qrmi

An LSF jobstarter that acquires a QPU and launches the job on it.

Runs on the execution host, wrapping the user's command.

## Install

Built as `jobstarter-qrmi` and installed as `jobstarter.qrmi`: a Rust crate
name cannot contain a dot, so the rename happens at install time.

```bash
cargo build --release -p jobstarter-qrmi
cp target/release/jobstarter-qrmi "$LSF_SERVERDIR/jobstarter.qrmi"
chmod 755 "$LSF_SERVERDIR/jobstarter.qrmi"
```

## Exit status

| Status | Meaning |
|---|---|
| the job's own code | the job ran and exited normally |
| `128 + n` | the job was killed by signal `n` |
| `125` | the jobstarter itself failed; the job never ran |

125 is the convention `env` and `timeout` use, chosen so an operator can tell
"the wrapper failed" from "the job failed".

## Resource types

The jobstarter accepts **six** types for acquisition and **three** for
metric-based selection, mirroring the two separate dictionaries in the Python
(`build_quantum_resource` and `build_provider`):

| Type | Acquire | Enumerate |
|---|:-:|:-:|
| `ibm-quantum-system` | ✓ | ✓ |
| `ibm-quantum-compute-service` | ✓ | ✓ |
| `qiskit-runtime-service` | ✓ | ✓ |
| `pasqal-cloud` | ✓ | — |
| `iqm-server` | ✓ | — |
| `alice-bob-felis` | ✓ | — |

Selection ranks on qubit counts, CLOPS and calibration medians, which the
non-IBM providers do not publish in that shape — so those must be named
explicitly with `device=`. Asking to *select* among them produces a message
saying so and suggesting `device=`, rather than ranking on absent metrics.

## Verification

```bash
cargo test -p jobstarter-qrmi                  # 20 tests
cargo clippy -p jobstarter-qrmi --all-targets  # clean under pedantic
```

## Licence

Apache-2.0. See `LICENSE` in the repository root.
