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

The symlink pair the Python used (`esub.qrmi` and `jobstarter.qrmi` both
pointing at one script, which branched on `argv[0]`) is gone: these are two
real programs.

## The two paths

**explicit** — `device=<name>` was given. The device is acquired directly and
nothing is enumerated. The README is explicit that the name "is taken at face
value and no checks for the device availability are made", so this path
deliberately does not validate it.

**selection** — backends are enumerated through QRMI, their metrics collected
concurrently, and a policy from `qpu-select` picks one.

## What holds on both paths

- **An acquired resource is always released.** A QPU acquisition is a
  chargeable reservation; leaking one holds it against other jobs until the
  service times it out. The Python repeats a `try`/`finally` on each path;
  here `lsfq_core::qrmi::lease::Lease` releases on drop, so the two cannot
  diverge. The acquisition token is removed from the environment whether or
  not the release succeeded, matching the Python's `finally` — which
  `tests/test_jobstarter_exit_status.py` asserts on.
- **The job's exit status reaches LSF faithfully**, including `128 + n` for a
  signal death (the case commit `8a3d1ce` fixed).
- **The job's environment is built, not inherited.** `env_clear()` then the
  computed map, so a variable QRMI injected for an unselected backend cannot
  leak through.

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

### A bug this fixes

The Python maps `qiskit-runtime-service` to
`ResourceType.IBMQiskitRuntimeService`, in both `build_provider()` (line
1260) and `build_quantum_resource()` (line 2122). **That attribute does not
exist** — QRMI's variant is `QiskitRuntimeService` — so every job submitted
with `qpu_type=qiskit-runtime-service` raises `AttributeError` today, on both
code paths. The Rust port uses the real name, verified against QRMI 0.26.0's
`ResourceType`, and the mapping is written out exhaustively so a rename
upstream becomes a compile error rather than a runtime one.

## Verification

```bash
cargo test -p jobstarter-qrmi                  # 20 tests
cargo clippy -p jobstarter-qrmi --all-targets  # clean under pedantic
```

Exit-status translation and environment isolation are tested against **real
child processes**, covering the same cases as
`tests/test_jobstarter_exit_status.py`: exit codes 0, 1, 42, 143 and 255, and
death by `SIGTERM`, `SIGINT` and `SIGKILL` (→ 143, 130, 137). One test
asserts that a sentinel in the jobstarter's own environment does *not* reach
the child, which is the property the `ctypes` workaround existed to achieve.

The binary has been run end-to-end against the live IBM Quantum Platform API:
with an explicit device and deliberately invalid credentials it built the
QRMI resource, attempted acquisition, received a genuine `code 1219`
authentication rejection, and reported it cleanly. All four startup error
paths (no command, no resource type, no request, unknown selector) were
checked to exit 125 with a specific message.

### Toolchain note

Needs a recent stable toolchain (1.94.1 or newer at the time of writing),
because QRMI pulls in the AWS SDK transitively. See
[`lsfq-core`](../lsfq-core/README.md#toolchain-requirement). The macOS
`libiconv` caveat described in [`elim-qpu`](../elim-qpu/README.md) applies
here too.

## Licence

Apache-2.0. See `LICENSE` in the repository root.
