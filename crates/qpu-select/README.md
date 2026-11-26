# qpu-select

Vendor-neutral QPU selection policies for HPC schedulers.

Given the observable metrics of a set of quantum backends and a user's
request, decide which backend to run on. That is the whole of this crate's
job.

## Scope

This crate has **no dependencies** and performs **no I/O**. It does not know
about QRMI, LSF, HTTP, environment variables, or an async runtime. Collecting
metrics is the caller's problem; choosing between them is this crate's.

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

## Licence

Apache-2.0. See `LICENSE` in the repository root.
