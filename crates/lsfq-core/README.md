# lsfq-core

Shared runtime routines for `esub.qrmi`, `jobstarter.qrmi` and `elim.qpu`.

## Scope

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

## Verification

```bash
cargo test -p lsfq-core                  # 126 tests
cargo test -p lsfq-core --features qrmi  # plus the QRMI adapter
cargo clippy -p lsfq-core --all-targets  # clean under pedantic
```


## Licence

Apache-2.0. See `LICENSE` in the repository root.
