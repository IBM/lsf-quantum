# qpu-select

Vendor-neutral QPU selection policies for HPC schedulers.

Given the observable metrics of a set of quantum backends and a user's
request, decide which backend to run on. That is the whole of this crate's
job.

## Scope

This crate has **no dependencies** and performs **no I/O**. It does not know
about QRMI, LSF, HTTP, environment variables, or an async runtime. Collecting
metrics is the caller's problem; choosing between them is this crate's.

That separation is deliberate — it is what makes the policies reusable from a
Slurm plugin, a Kubernetes scheduler, a broker service or a notebook, and what
keeps the tests hermetic (no network, no fixtures on disk, millisecond runs).
It is `no_std` + `alloc`, so it also links into constrained environments.

**Do not add a dependency here** without a strong reason.

## Policies

| `selector=` | Type | Chooses |
|---|---|---|
| `basic` | `Basic` | the least busy backend with enough qubits |
| `health` | `Health` | best weighted composite of T1, readout error and queue depth |
| `priority` | `Priority` | by applying the request's requirements in the order written |

## Usage

```rust
use qpu_select::{Candidates, ProcessorType, QpuMetrics, Request, Selector, selector_by_name};

let mut candidates = Candidates::new();
candidates.insert(
    "ibm_marrakesh".to_owned(),
    QpuMetrics::builder()
        .qubits(156)
        .clops(195_000.0)
        .pending_jobs(3)
        .processor_type(ProcessorType::new("Heron").with_revision("2"))
        .available(true)
        .build(),
);

let request = Request::parse("qubits=128;pending_jobs=1000").unwrap();
let selector = selector_by_name("priority").expect("known policy");
let selection = selector.select(&candidates, &request);

assert_eq!(selection.name(), Some("ibm_marrakesh"));
```

## The request grammar

Requirements are separated by `;` and applied in the order written:

```
qubits=120;processor_type=Nighthawk;clops=240000
```

A bare `=` is widened by the metric's direction — `>=` for metrics where more
is better, `<=` where less is better — but only when the value is numeric;
otherwise it means `==`:

| Written | Means |
|---|---|
| `qubits=120` | `qubits >= 120` |
| `clops=240000` | `clops >= 240000` |
| `T1_median_us=100` | `T1_median_us >= 100` |
| `pending_jobs=10` | `pending_jobs <= 10` |
| `readout_error_median=0.01` | `readout_error_median <= 0.01` |
| `processor_type=Heron` | `processor_type.family == "Heron"` |

`>=`, `<=`, `>`, `<`, `==` and `!=` may also be written explicitly. Values may
be quoted (`'2'`) to force string comparison, and `none`/`null` match a metric
the backend does not report.

## Verification

```bash
cargo test                    # 86 tests: unit, integration and doc
cargo clippy --all-targets    # clean, including pedantic lints
```

The port's fidelity to the original Python is checked differentially rather
than by inspection:

```bash
cargo test -p qpu-select --test golden
```

`tests/fixtures/py_results.json` holds the decisions the **original Python**
`select_device_priority()` made over `tests/fixtures/corpus.txt` — 40
randomised fleets × 25 requests, 1000 pairs, with each metric independently
present or absent so the missing-value paths are exercised. They were
captured by running the Python itself, while `qrmi-esub-jobstarter.py` was
still in the tree, and the test asserts this crate still reproduces every
one: **1000/1000**, over 501 selections spanning 44 distinct backends and
499 correct rejections.

Originally this ran as a live differential harness that extracted the
selector with `ast` and compared the two implementations on each run. Now
that the Python has been removed, the recorded outputs take its place: the
comparison is the same, but one side is frozen.

**These fixtures are evidence, not test data to be regenerated.** If a
change makes the test fail, the change altered which backend a job lands on
— that is the finding, not the fixture's fault.

## Deliberate differences from the Python implementation

Both are documented at the point they occur in the source, and both are
behaviour changes rather than refactors:

1. **`Basic` joins by name, not by list position.** The Python
   `select_device_default()` merges its status and configuration lists by
   index, with the assumption written in a source comment ("Assuming that
   devices come in the same order"). If the two enumerations ever disagree,
   qubit counts are attributed to the wrong device. Keying by name makes that
   unrepresentable.

2. **`Health` excludes unscoreable backends instead of scoring them zero.**
   The Python version defaults a missing T1 to `0.0`, which forces the whole
   product to zero, so every backend lacking calibration data scores exactly
   `0.0000` and the policy degenerates into "first backend examined" — visible
   in the worked example in the top-level `README.md`, where all six QPUs
   score `0.0000`. Here such a backend is rejected with
   `Rejection::MissingMetric`, so the caller can see why.

One upstream quirk is preserved rather than fixed, because it is part of the
compatibility surface: `processor_type.revision=2` matches nothing, since the
revision is text and an unquoted `2` parses as a number. Write
`processor_type.revision='2'`. This is covered by a named test.

## Licence

Apache-2.0. See `LICENSE` in the repository root.
