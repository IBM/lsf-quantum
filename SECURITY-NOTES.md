# Security notes

Dependency advisories that are suppressed rather than fixed, and why. For
how to report a vulnerability in this project, see [SECURITY.md](SECURITY.md).

Everything below reaches this workspace only transitively, through the
pinned QRMI release (0.27.1), and has no patched release that can be adopted
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
| RUSTSEC-2026-0387 | `crypto-bigint` 0.5.5 | Only on the S3 request-signing path, which is never called |
| RUSTSEC-2021-0141, RUSTSEC-2024-0384, RUSTSEC-2026-0441, RUSTSEC-2026-0543 | `dotenv`, `instant`, `pin-utils`, `chrono` | Unmaintained notices, no vulnerability |
| RUSTSEC-2026-0253 | `lru` 0.16.4 | Unsound notice; only via `aws-sdk-s3`, which is never called (suppressed in `.cargo/audit.toml` only) |

The TLS stack that carries live traffic to IBM Quantum Platform is current
and unaffected, and it is the only one in the tree: `rustls` 0.23.46,
`rustls-webpki` 0.103.17, `h2` 0.4.20, `hyper-rustls` 0.27.10.

## History: the legacy TLS stack is gone

Up to QRMI 0.26.x, `aws-smithy-http-client` pulled in a second, legacy TLS
stack (`rustls` 0.21.12, `rustls-webpki` 0.101.7, `hyper-rustls` 0.24,
`h2` 0.3.27) through feature unification, and it could not be upgraded from
this workspace. Those advisories were suppressed (RUSTSEC-2026-0098, -0099,
-0104, -0258, -0346, -0349, -0382).

QRMI 0.27.1 resolves `aws-smithy-http-client` 1.5.3, which no longer pulls
the legacy stack. After the upgrade `Cargo.lock` has a single `rustls`, a
single `rustls-webpki` and `h2` 0.4.x only. The suppressions and the
`[bans].skip` entries for the duplicate rustls crates were removed.

Two further advisories were cleared by the lock file rather than suppressed:
`ctutils` is now 0.4.8 (RUSTSEC-2026-0385) and `tokio` is 1.54.1
(RUSTSEC-2026-0386).

A scanner that reports `rustls` 0.21, `rustls-webpki` 0.101 or `h2` 0.3 is
looking at an old lock file, not at this tree.

## The S3 signing path

QRMI still depends on `aws-sdk-s3` 1.137.0 through `quantum-system-api`, so
the AWS signing code is compiled in. Nothing in `crates/` issues an S3
request.

`crypto-bigint` 0.5.5 (RUSTSEC-2026-0387) is a timing-variability issue in a
constant-time primitive. It is reachable only through S3 request signing:

    crypto-bigint <- elliptic-curve <- p256 <- ecdsa <- aws-sigv4 <- aws-runtime <- aws-sdk-s3

No S3 request is ever signed, and a timing side channel also needs an
attacker who can measure repeated operations. `lru` 0.16.4
(RUSTSEC-2026-0253) is likewise only reached through `aws-sdk-s3`.

This is a static assessment. It shows that no code in this repository
reaches these functions. It does not prove that the code linked into
`jobstarter.qrmi` is unreachable under every possible QRMI configuration.
Linking was last measured against QRMI 0.26.0 and has not been re-measured
for 0.27.1.

## Unmaintained crates

None of these has a known vulnerability. They are suppressed so that a real
advisory is not lost in the noise, not because they are dismissed.

| Crate | Note |
|---|---|
| `dotenv` 0.15.0 | A direct dependency of QRMI. Nothing in `crates/` uses it: `lsfq-core` parses `.env` and `env.qpu` itself (see `creds.rs`). |
| `instant` 0.1.13 | Transitive via `parking_lot` 0.11, under `reqwest-retry` and the QRMI sub-crates. Only appears with `cargo tree --target all`. |
| `pin-utils` 0.1.1 | Transitive via `aws-smithy-http`. |
| `chrono` 0.4.45 | Transitive via the QRMI sub-crates. |

## Licences

`GPL-3.0-or-later` is deliberately not in the `deny.toml` allow-list, so
`cargo deny check licenses` fails if a dependency introduces it. This
project is Apache-2.0, and GPL-3.0 is generally held to be incompatible with
redistributing a combined work under it. (`rhexdump`, which was GPL-3.0, was
removed from QRMI in 0.26.3.)

## Checking and maintaining

```bash
cargo install cargo-audit cargo-deny   # neither is vendored

cargo audit                            # reads .cargo/audit.toml
cargo deny check                       # advisories, licences, bans, sources
```

All of these currently pass.

- **After a QRMI upgrade:** run both tools, then
  `cargo tree -i aws-sdk-s3`. If `aws-sdk-s3` is gone, delete the S3-path
  advisory IDs from both config files and the "S3 signing path" section
  above.
- **Testing whether an ignore is still needed:** comment it out and re-run
  the tools, or check the tree for the crate directly. Do not infer it from
  the tool's output, because a suppressed advisory is exactly the one that
  does not appear.
- **Stale entries:** `cargo deny` warns (`advisory-not-detected`) when an
  ignored ID matches nothing, so a stale entry reports itself. It currently
  reports none.
- **Duplicate versions:** `multiple-versions` is `warn`, so duplicate crates
  appear as warnings. They come from QRMI's own dependencies and are not
  actionable here.
