# lsfq-core

Shared runtime routines for `esub.qrmi`, `jobstarter.qrmi` and `elim.qpu`.

## Scope

Everything the three binaries genuinely share, and nothing else. Selection
*policy* is not here — that is [`qpu-select`](../qpu-select), which this crate
depends on and re-exports. The split is deliberate: `qpu-select` is pure and
reusable by other schedulers, while everything here is specific to LSF and
QRMI.

| Module | Covers |
|---|---|
| `diag` | debug levels, warnings, `LSB_SUB_ABORT_VALUE` |
| `creds` | `.env` and `$LSF_ENVDIR/env.qpu`, legacy aliases, endpoint defaults |
| `lsf::esub` | the `LSF_SUB4_SUB_ENV_VARS` directive |
| `lsf::bpost` | posting the acquisition ID to the job record |
| `lsf::elim` | the load-index line format |
| `lsf::hostres` | mapping an ELIM host to the QPU it serves |
| `qrmi::target` | parsing the `target()` payload into metrics |
| `qrmi::provider` | enumerating backends, concurrently *(feature `qrmi`)* |
| `qrmi::lease` | RAII acquire/release *(feature `qrmi`)* |
| `jobenv` | the job's environment, built explicitly |
| `exit` | child outcome → LSF exit status |

## The `qrmi` feature

Calling QRMI pulls in the QRMI crate and a tokio runtime. Most of this crate
is parsers and formatters that need neither, so those live behind the `qrmi`
feature, **off by default**:

```bash
cargo test                    # parsers, formatters, env logic — hermetic
cargo test --features qrmi    # the above plus the QRMI adapter
```

This keeps the bulk of the crate testable without the QRMI toolchain or a
network, and confines every use of QRMI's pre-1.0 API to two modules.

### Toolchain requirement

QRMI is not published to crates.io, so it is a **git dependency pinned to a
tag**. It pulls in the AWS SDK transitively, and some `aws-smithy-*` releases
have an MSRV above the crate's declared `1.76`. The default feature set builds
on 1.76; `--features qrmi` needs a toolchain at least as new as the AWS SDK's
MSRV (1.94.1 at the time of writing). `.cargo/config.toml` sets
`resolver.incompatible-rust-versions = "fallback"`, which helps only when the
lockfile is not already pinned — so in practice the QRMI feature wants a
current stable toolchain.

## Verification

```bash
cargo test -p lsfq-core                  # 126 tests
cargo test -p lsfq-core --features qrmi  # plus the QRMI adapter
cargo clippy -p lsfq-core --all-targets  # clean under pedantic
```

Fidelity to the original Python is established differentially:

```bash
cargo test -p lsfq-core --test golden
```

`tests/fixtures/py_lines.txt` holds the lines the **original** `elim.qpu`
produced for the target documents in `tests/fixtures/targets.json`. They
were captured by running the Python itself, while `elim.qpu` was still in
the tree, and the test asserts this crate still reproduces each one
**byte-for-byte** — the right standard here, because LSF parses these lines
itself, so a one-character difference is a deployment regression.

The corpus deliberately covers the cases where implementations diverge: odd
and even sample counts (median tie-breaking), absent properties, a missing
`pending_job_count`, an unparseable `sample_name`, an empty configuration,
an uncalibrated CZ gate, and error values at the extremes of the exponent
formatting.

Current status: **11/11 byte-identical**.

Originally this ran as a live differential harness extracting the formatters
with `ast`. Now that the Python has been removed, the recorded lines take
its place. **They are evidence, not test data to be regenerated** — a
failure means what LSF receives has changed.

## Notes on fidelity

A few upstream behaviours are reproduced deliberately, with a test naming each
one, because they are part of the external contract rather than defects:

- **The trailing comma** in `LSF_SUB4_SUB_ENV_VARS="A=1,B=2,"`. LSF accepts
  it, and a deployed cluster is parsing this string today.
- **`µs` in ELIM index names** (`T1_median_µs`) versus `us` in the request
  grammar (`T1_median_us`). These are two separate contracts; unifying them
  would silently stop LSF matching the indices.
- **`"None"` as a device name.** An unset Python `None` serialises into the
  environment as the literal text, so `is_unset_device` treats `none`, `null`
  and `""` as absent.
- **Python's `float` formatting.** `format_scientific` reproduces CPython's
  `format(v, '.1e')` with its `e-0`/`e+0` fix-up, including the `+` that
  Rust's native `{:.1e}` omits for non-negative exponents.

Two differences are deliberate improvements, each documented at the point it
occurs:

1. **Unsafe values in the esub directive are rejected, not concatenated.** A
   comma or quote in a value would silently corrupt every variable after it;
   failing at submission time is safer and does not change what a well-formed
   request produces.
2. **Release is structural.** `qrmi::lease::Lease` releases on `Drop` if it
   was not released explicitly, so the two code paths that each had to
   remember a `try`/`finally` cannot diverge.

## Licence

Apache-2.0. See `LICENSE` in the repository root.
