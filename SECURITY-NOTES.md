# Security notes

Dependency advisories that are suppressed rather than fixed, and why. For
how to report a vulnerability in this project, see [SECURITY.md](SECURITY.md).

Everything below reaches this workspace only transitively, through the
pinned QRMI release (0.26.3), and has no patched release that can be adopted
from here. Advisories with a patched release are fixed by updating
`Cargo.lock`, not suppressed.

Suppressions live in two files because `cargo-audit` and `cargo-deny` do not
share a format, and an ignore in one has no effect on the other:

- `.cargo/audit.toml` for `cargo-audit`
- `deny.toml` for `cargo-deny` (advisories, licences, bans, sources)

Keep the advisory lists in step.

## Summary

| Advisory | Crate | Why it is suppressed |
|---|---|---|
| RUSTSEC-2026-0098, -0099, -0104 | `rustls-webpki` 0.101.7 | Legacy TLS stack, linked but not used for traffic |
| RUSTSEC-2026-0346, -0349 | `rustls` 0.21.12 | Same legacy stack |
| RUSTSEC-2026-0382 | `rustls-webpki` 0.101.7 | Same legacy stack |
| RUSTSEC-2026-0258 | `h2` 0.3.27 | Same legacy stack; low severity |
| RUSTSEC-2026-0385, -0387 | `ctutils` 0.4.3, `crypto-bigint` 0.5.5 | Only on the S3 request-signing path, which is never called |
| RUSTSEC-2026-0386 | `tokio` 1.53.2 | `AsyncFd` is never constructed here |
| RUSTSEC-2021-0141, RUSTSEC-2024-0384, RUSTSEC-2026-0441, RUSTSEC-2026-0543 | `dotenv`, `instant`, `pin-utils`, `chrono` | Unmaintained notices, no vulnerability |
| RUSTSEC-2026-0253 | `lru` 0.16.4 | Unsound notice; only via `aws-sdk-s3`, which is never called (suppressed in `.cargo/audit.toml` only) |

The modern TLS stack that carries live traffic to IBM Quantum Platform is
current and unaffected: `rustls` 0.23.46, `rustls-webpki` 0.103.17,
`webpki-roots` 1.0.12.

## The legacy TLS stack

Two copies of `rustls-webpki` are in the tree, which matters when reading
scanner output:

| Version | Status | Used by |
|---|---|---|
| 0.103.17 | current, unaffected | `rustls` 0.23.46, the TLS these binaries use |
| 0.101.7 | vulnerable | `rustls` 0.21.12, a legacy stack pulled in transitively |

A scanner that reports `rustls-webpki` 0.101.7, `rustls` 0.21.12 or
`h2` 0.3.27 has found the legacy copy.

### Where it comes from

```
rustls 0.21.12 / rustls-webpki 0.101.7 / h2 0.3.27
  └── hyper-rustls 0.24.2
        └── aws-smithy-http-client 1.5.0
              └── aws-sdk-s3 1.137.0
                    └── quantum-system-api
                          └── QRMI 0.26.3
                                └── lsfq-core → {esub, jobstarter, elim}
```

The cause is feature unification in `aws-smithy-http-client`: its modern
`rustls-aws-lc` feature activates the optional `legacy-hyper-rustls`
dependency, so the whole `rustls` 0.21 stack is compiled although nothing in
this workspace asks for legacy TLS. The fix belongs upstream, in
`awslabs/smithy-rs` or in a QRMI dependency bump that follows it.

### Why it cannot be fixed here

- **Bumping `rustls-webpki`:** `rustls` 0.21 requires `^0.101`, so cargo
  refuses to move it alone.
- **Bumping `hyper-rustls`:** every 0.24.x release is pinned to `rustls`
  0.21. Modern rustls arrives only in 0.25.0, outside the range
  `aws-smithy-http-client` accepts.
- **`[patch.crates-io]`:** cargo rejects a patch that points at the same
  source.
- **Bumping `aws-smithy-http-client`:** 1.5.0 is the newest release.
- **Bumping `aws-sdk-s3`:** QRMI constrains it to `>=1.57.0, <1.138.0`, and
  it is at 1.137.0. Version 1.152.0 was tested in a scratch project and
  resolves the same legacy stack, so this is not a stale pin.
- **Feature flags:** neither QRMI nor `quantum-system-api` exposes a feature
  that drops the S3 path.

### What is linked

The QRMI maintainers' view is that the legacy stack can be ignored because
`quantum-system-api` requests `aws-sdk-s3` with `behavior-version-latest`.
The conclusion holds but that reason does not: `behavior-version-latest` is
an empty feature (`behavior-version-latest = []`) that gates no dependency.
The legacy stack is selected by the default `default-https-client` feature.

Linking was measured on a release build against QRMI 0.26.0:

| Binary | `rustls-webpki` 0.101.7 | `rustls-webpki` 0.103.x |
|---|---|---|
| `esub.qrmi` | absent | absent |
| `jobstarter.qrmi` | present | present |
| `elim.qpu` | absent | present |

The legacy code is compiled for all three and survives linking in
`jobstarter.qrmi`, the only binary that enumerates backends through
`ResourceProvider`. The retained modules are the ones the advisories concern
(`verify_cert.rs`, `subject_name/dns_name.rs`, `subject_name/ip_address.rs`).
Re-measure after a QRMI upgrade rather than relying on this table.

### Why the exposure is low

- These binaries make TLS connections only to IBM Quantum Platform, through
  QRMI, on the modern `rustls` 0.23 / `rustls-webpki` 0.103 path.
- Nothing in `crates/` calls the AWS SDK or S3, so the legacy stack is linked
  but not exercised.
- RUSTSEC-2026-0104 (panic in CRL parsing) states that applications that do
  not use CRLs are not affected. No code here parses a CRL.
- RUSTSEC-2026-0098 and -0099 require a misissued certificate, because name
  constraints restrict otherwise properly issued certificates.
- RUSTSEC-2026-0346 (session values exposed through `Debug`) requires
  something to format a `rustls` session value with `{:?}`. No crate in this
  workspace references `rustls`, and the diagnostics call sites pass only
  device names, selectors, paths, exit statuses and QPU metrics, even at the
  most verbose level (`LSF_QRMI_DEBUG=level2`).
- RUSTSEC-2026-0258 (`h2` empty DATA frames) is rated low severity. The
  0.3 line has no fix, and the modern `h2` 0.4.x is also in the tree.

This is a static assessment. It shows that no code in this repository
reaches the vulnerable functions. It does not prove that the code linked into
`jobstarter.qrmi` is unreachable under every possible QRMI configuration.

RUSTSEC-2026-0049 (CRL distribution points) is not listed in either config
file: it is marked unaffected below 0.102.0-alpha.0, so 0.101.7 is out of
scope and no tool raises it.

## The S3 signing path

`ctutils` 0.4.3 (RUSTSEC-2026-0385) and `crypto-bigint` 0.5.5
(RUSTSEC-2026-0387) are timing-variability issues in crypto primitives. They
are reachable only through S3 request signing:

    ctutils       <- digest <- {hmac, sha1, sha2, md-5} <- aws-smithy-checksums <- aws-sdk-s3
    crypto-bigint <- elliptic-curve <- p256 <- ecdsa <- aws-sigv4 <- aws-runtime

Nothing in `crates/` issues an S3 request, and a timing side channel also
needs an attacker who can measure repeated operations. `lru` 0.16.4
(RUSTSEC-2026-0253) is likewise only reached through `aws-sdk-s3`.

`tokio` 1.53.2 (RUSTSEC-2026-0386) concerns `AsyncFd` constructors violating
I/O safety. `grep -rn AsyncFd crates/` is empty. The only runtime this
workspace builds is `elim-qpu`'s current-thread runtime, which joins two
QRMI queries.

## Unmaintained crates

None of these has a known vulnerability. They are suppressed so that a real
advisory is not lost in the noise, not because they are dismissed.

| Crate | Note |
|---|---|
| `dotenv` 0.15.0 | A direct dependency of QRMI. Nothing in `crates/` uses it: `lsfq-core` parses `.env` and `env.qpu` itself (see `creds.rs`). |
| `instant` 0.1.13 | Transitive via `parking_lot` 0.11 / `wasm-timer`, under `reqwest-retry` and the QRMI sub-crates. |
| `pin-utils` 0.1.1 | Transitive via `aws-smithy-http`. |
| `chrono` 0.4.45 | Transitive via the QRMI sub-crates. |

## Licences

`GPL-3.0-or-later` is deliberately not in the `deny.toml` allow-list, so
`cargo deny check licenses` fails if a dependency introduces it. This
project is Apache-2.0, and GPL-3.0 is generally held to be incompatible with
redistributing a combined work under it.

## Checking and maintaining

```bash
cargo install cargo-audit cargo-deny   # neither is vendored

cargo audit                            # reads .cargo/audit.toml
cargo deny check                       # advisories, licences, bans, sources
```

All of these currently pass.

- **After a QRMI upgrade:** run `cargo tree -i rustls-webpki@0.101.7`. If it
  returns nothing, delete the legacy-stack advisory IDs from both config
  files and the "legacy TLS stack" section above.
- **Testing whether an ignore is still needed:** comment it out and re-run
  the tools, or check the tree for the crate directly. Do not infer it from
  the tool's output, because a suppressed advisory is exactly the one that
  does not appear.
- **Stale entries:** `cargo deny` warns (`advisory-not-detected`) when an
  ignored ID matches nothing, so a stale entry reports itself.
- **Duplicate versions:** multiple `rustls`, `rustls-webpki`, `hyper-rustls`
  and `tokio-rustls` versions are expected while the upstream fix is
  outstanding. They are listed under `[bans].skip` so `multiple-versions`
  stays useful for everything else.
