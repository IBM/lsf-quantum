# Migrating from IBM Quantum Platform REST to the QRMI API

Analysis and record of the change that replaces the direct IBM Quantum
Platform REST calls in `elim.qpu` and `qrmi-esub-jobstarter.py` with calls to
the [QRMI](https://github.com/qiskit-community/qrmi) API.

## Summary

Every IBM Quantum Platform REST call in both scripts had an exact QRMI
equivalent, and both have been migrated. QRMI calls the same service endpoints
internally, so the data is identical rather than merely comparable.

The result removes roughly 320 lines of hand-rolled HTTP, authentication and
token-cache code, drops the `requests` dependency from the job starter, and
leaves the LSF-facing behaviour of both scripts unchanged.

| | before | after |
|---|---|---|
| HTTP client | `requests`, hand-rolled | QRMI (Rust core) |
| IAM token | generated and cached per script | handled inside QRMI |
| Endpoint URLs | hardcoded in both scripts | from `QRMI_IBM_QCS_ENDPOINT` |
| Vendor coupling | IBM-only | vendor-agnostic via `ResourceType` |

## Why this was worth doing

The job starter already used QRMI for the parts of its work that acquire and
release a resource (`QuantumResource`, `ResourceType`), while doing its own REST
calls for everything else. `elim.qpu` used REST exclusively. That split meant
two authentication paths, two token lifetimes and two sets of hardcoded URLs in
a single repository. The migration makes both scripts use one client for all
IBM Quantum access.

## Call-by-call mapping

### `elim.qpu`

| before | after |
|---|---|
| `gen_bearer_token()` → `POST iam.cloud.ibm.com/identity/token` | implicit; QRMI renews the token on each call |
| `GET /backends/{id}/configuration` | `resource.target()` → `["configuration"]` |
| `GET /backends/{id}/properties` | `resource.target()` → `["properties"]` |
| `GET /backends/{id}/status` → `length_queue` | `resource.status()` → `pending_job_count` |

Three HTTP round-trips per poll became two QRMI calls, because `target()`
returns configuration and properties in one document.

### `qrmi-esub-jobstarter.py`

| before | after |
|---|---|
| `gen_bearer_token()` | implicit |
| `read_env_vars_from_config()` (validated *and* fetched a token) | `validate_env_vars_from_config()` (validates only) |
| `get_avail_devices()` → `GET /backends` | `ResourceProvider.resources()` |
| `get_device_topology(..., "status")` | `resource.status()` per resource |
| `get_device_topology(..., "configuration")` | `resource.target()` per resource |
| `get_all_backend_metrics_rest()` | `get_all_backend_metrics_qrmi()` |
| `get_json()`, `extract_rest_backend_metrics()` | removed; QRMI equivalents reuse the shared extraction helpers |

## The three findings that decided the approach

### 1. Vendor-specific configuration fields survive QRMI

This was the main risk. Both scripts read `clops_h` and `sample_name`, neither
of which appears in QRMI's typed `BackendConfiguration` struct
(`qrmi/src/ibm/models/backend_configuration.rs`). Had the IBM Quantum Compute
Service path deserialised into that struct, CLOPS and the processor type would
have been silently dropped.

It does not. The generated client returns
`HashMap<String, serde_json::Value>`
(`qrmi/dependencies/quantum_compute_client/src/apis/backends_api.rs`), which is
a verbatim passthrough of the service payload. Every field is preserved,
including `clops_h` and `sample_name`. The typed struct is used only by the
Qiskit Runtime Service and Quantum System providers.

Consequence: the existing `get_system_config()` and `get_system_medians()`
parsers in `elim.qpu` work unchanged on the QRMI payload, and were left
byte-for-byte identical apart from trailing-whitespace cleanup.

### 2. `target()` and `status()` do not require an acquired resource

The upstream QRMI example calls `acquire()` before `target()`, which would be
alarming for an ELIM that polls every 10 seconds, since acquiring a resource
consumes a session. Tracing the implementation
(`qrmi/src/ibm/quantum_compute_service.rs`), both `target()` and `status()`
perform only authenticated GETs and never touch the session; the example's
`acquire()` is there for the subsequent `task_start()` call.

**`elim.qpu` therefore does not call `acquire()`.** Doing so would be a
correctness bug, not just an inefficiency.

### 3. Token renewal improves

`elim.qpu` previously maintained a 3500-second token cache by hand. QRMI's
`check_token` refreshes at `max(360, lifetime/10)` seconds before expiry, using
the lifetime the IAM service actually reports rather than a hardcoded constant.
The `token_cache_time` variable and its timer arithmetic are gone.

## Behavioural changes

### `pending_jobs` has a new source

Previously `length_queue` came from the per-backend status endpoint. QRMI's
`status()` reads `queue_length` from the backend list and additionally pages
`list_jobs` and `get_session` to compute its `busy` flag. The queue number is
the same, but the request is heavier. At the ELIM's 10-second interval this is
worth watching; raise `sleep_time` if it proves too chatty.

`pending_job_count` is optional in QRMI, so it may be absent.

### Backward compatibility: on QRMI 0.24.5 the queue length defaults to 0

Not a limitation of the current release — 0.25.1 supplies the queue length — but
the fallback is retained so a host still on 0.24.5 keeps working.

`QuantumResource.status()` is the only route to `pending_job_count`, and it
landed after 0.24.5. Both scripts detect its absence and fall back to
`is_accessible()` (`get_device_status()` in `elim.qpu`,
`get_resource_status()` in the job starter), which reports a bool and no queue
length.

For the ELIM that surfaces as `pending_jobs -`. For the job starter the
consequence would have been worse: both query sites wrap their calls in
`try/except`, so an unguarded `status()` call raised `AttributeError` there and
**every backend was dropped**, leaving the selectors with nothing to choose
from. The shared helper prevents that.

The selectors sort and do arithmetic on `length_queue`, which the REST endpoint
always supplied as an integer, so `get_devices_topology_qrmi()` substitutes `0`
when it is `None` -- otherwise `select_device_default` raises `TypeError: '<'
not supported between instances of 'NoneType' and 'NoneType'` and
`select_device_health` raises on `None + int`. Treating an unknown queue as
empty keeps every backend eligible and lets the other criteria rank them. A
real queue length is never overwritten.

### Enumerating backends injects environment variables, which are pruned

QRMI's `ResourceProvider` is not side-effect free. `inject_backend_env()` in
`qrmi/src/ibm/quantum_compute_service_provider/mod.rs:96` calls `setenv()` for
every backend it enumerates:

```rust
fn inject_backend_env(&self, backend_name: &str) {
    for (key, value) in &self.provider_env {
        env::set_var(format!("{backend_name}_{key}"), value);
    }
}
```

It has to: `IBMQuantumComputeService::new()` resolves its configuration only
through the resource-scoped `<backend>_<KEY>` name, with no unprefixed
fallback. So enumerating N backends leaves N sets of prefixed variables in the
job starter's process, and `subprocess.run()` passes all of them to the job --
visible as a `QRMI_*` block per device in `env` inside an interactive job.

The REST implementation had no such effect: `GET /backends` touched no
environment. This was therefore a regression introduced by the migration, and
not a pre-existing behaviour -- `build_qrmi_vars_job()` is single-device and
unchanged, but it is no longer the only writer.

**The injected names are invisible to `os.environ`.** Rust's `env::set_var()`
calls libc `setenv()`, which mutates the C-level `environ` that `execve()`
passes to children. Python populated `os.environ` once at interpreter start and
never re-reads it, so those names are absent from `os.environ` while still being
inherited by every subprocess -- and `os.environ.pop()` on them is a no-op. A
prune written against `os.environ` therefore finds nothing to remove, reports
nothing, and the job still sees every backend. This is what made the first
attempt at this fix appear to work while changing nothing.

`read_process_environ()` reads the C `environ` symbol through `ctypes`, so it
sees the native writes. `/proc/self/environ` is deliberately not used: on Linux
it reports the environment as of `exec()` time and would miss exactly these
writes.

`snapshot_backend_env()` records that environment before enumeration, and
`job_environment()` builds the job's environment explicitly -- undoing QRMI's
writes for the unselected backends -- which is passed to
`subprocess.run(job_args, env=...)`. Mutating the inherited environment is not
an option, so the job's environment is constructed rather than inherited.

Two details matter:

- **Values are snapshotted, not just names.** `inject_backend_env()` overwrites
  an existing `<backend>_<KEY>` as readily as it creates one, so a site-set
  value for an unselected device (a regional endpoint in `env.qpu`, say) would
  otherwise be silently replaced by the provider's own. A name QRMI created is
  removed; one it overwrote is restored to the site value rather than deleted.
- **Debugging only adds information.** Pruning is unconditional, so the job's
  QRMI variables always describe just the selected device. `LSF_QRMI_DEBUG=level1`
  or `level2` additionally logs the backends QRMI enumerated -- a record that
  exists nowhere else once the injected names are removed -- and the selected
  device the job will use. What the job receives is identical at every level.

The explicit `device=` path never enumerates, so it has nothing to prune, but
it builds the job environment the same way for consistency.

### A failed poll no longer kills the ELIM

The original loop called `print_error()` (which calls `exit(1)`) or `break` when
a fetch returned nothing, so one transient failure terminated the ELIM and LSF
lost *every* index for that host until LIM restarted it. Each failure is now
handled in place: a missing target skips the cycle and retries, while a missing
status or missing properties emit `-` for the affected fields. The field count
stays at 10 in all cases, so LSF always parses the line.

### Missing values are now reported as `-` instead of crashing

`elim.qpu` previously interpolated medians directly into its output. When a
device returned no calibration data, `format(None, '.1e')` raised `TypeError`
and `device_status['length_queue']` raised `KeyError`, killing the ELIM. The
migrated script emits `-`, which LSF reads as "value not currently available",
and keeps the field count constant as LSF requires.

The emitted resource names and their order are unchanged:

```
10 qubits <n> clops <n> qpu_version <s> processor_type <s> pending_jobs <n> \
   readout_error_median <sci> sx_error_median <sci> cz_error_median <sci> \
   T1_median_µs <f> T2_median_µs <f>
```

### CLOPS extraction now covers `clops_h` and `clops_v`

`extract_clops()` only looked at a `clops` key, which is how the backend *list*
endpoint publishes the figure. A backend *configuration* publishes it as a
scalar `clops_h`. Because the QRMI path reads the configuration rather than the
list entry, CLOPS came back as `None` and any `clops=` requirement in a
priority selector silently matched nothing. All three keys are now checked in
order.

This was a live bug in the priority selector, surfaced by the migration.

### The "health" selector now receives the data it scores on

`select_device_health()` reads `T1_median_µs` and `readout_error_median` from
its `devices_config` entries, but the REST implementation never populated
either — the health score always ran on its fallback defaults. Since QRMI
returns the calibration properties in the same `target()` document as the
configuration, these fields are now filled in at no extra request cost, and
the selector scores on real data.

### `select_device_default` depends on dict insertion order

`select_device_default()` flattens each status and configuration dict into a
positional list by iterating its keys, then indexes the result. It requires:

- status entries ordered `message`, `length_queue`, `device`
- configuration entries ordered `n_qubits`, `device`

`get_devices_topology_qrmi()` constructs its dicts in exactly that order, with
a comment recording the constraint. Any new key must be appended *after*
`n_qubits`. This coupling is pre-existing and fragile; rewriting that selector
to use key lookups (as `select_device_health` already does) would be a
worthwhile follow-up but is outside the scope of this change.

### QRMI status codes are mapped back to REST spellings

The selectors compare against the string `'available'`. QRMI reports an
`online` / `offline` / `paused` status code instead, so
`get_devices_topology_qrmi()` maps `online` to `available` and otherwise
reports the status reason. This keeps the selectors unchanged and, as a bonus,
surfaces `paused` (for example during calibration) which the old code could not
distinguish.

## Configuration changes

### `elim.qpu` — new variable names in `$LSF_ENVDIR/env.qpu`

QRMI resolves its settings **per resource**, from environment variables named
`<resource_id>_<SETTING>`. For `ibm_marrakesh` it reads
`ibm_marrakesh_QRMI_IBM_QCS_ENDPOINT` and **never** the bare
`QRMI_IBM_QCS_ENDPOINT` — `resolve_opt` in `qrmi/src/common/config.rs` does a
single exact-name lookup with no fallback to the unprefixed name. This is the
same convention `esub.qrmi`/`jobstarter.qrmi` already use when they export
`<device>_<key>` for the resources a job acquires.

`elim.qpu` therefore exports the prefixed names itself, after it has discovered
its device from `lshosts` and before it constructs the resource. `env.qpu` may
spell each setting in any of three ways, tried in order:

1. `<device>_QRMI_IBM_QCS_*` — already prefixed, used as-is.
2. `QRMI_IBM_QCS_*` — unprefixed, promoted to the prefixed name.
3. `APIKEY` / `CRN` — legacy names, mapped then promoted.

So **existing `env.qpu` files keep working** in either of the simpler forms.
Preferred:

```
QRMI_IBM_QCS_IAM_APIKEY=<api_key>
QRMI_IBM_QCS_SERVICE_CRN=<crn>
QRMI_IBM_QCS_ENDPOINT=https://quantum.cloud.ibm.com/api/v1
QRMI_IBM_QCS_IAM_ENDPOINT=https://iam.cloud.ibm.com
```

Still accepted, with the endpoints defaulted:

```
APIKEY=<api_key>
CRN=<crn>
```

Use form 1 when one `env.qpu` must carry settings for several devices, or when
a device needs a different endpoint from the rest — a prefixed value is never
overwritten by the public default, so a regional endpoint survives.

A missing setting is reported before the first QRMI call, naming the device and
both accepted spellings, rather than surfacing as QRMI's bare
`ibm_marrakesh_QRMI_IBM_QCS_ENDPOINT environment variable is not set`.

Because `env.qpu` is owned by the LSF administrator with octal 400 permissions,
adopting the preferred form is an operational step, not only a code change.
Setting the endpoints explicitly is recommended for any non-public or regional
deployment, where the defaults would be wrong.

`env.qpu` must be readable by the user the ELIM runs as, and that host needs
network access to both the QCS and IAM endpoints — unchanged from before.

### `qrmi-esub-jobstarter.py` — no change to the variables it reads

It already required the four `QRMI_IBM_QCS_*` variables, read from the
`file=` argument of `bsub -a "qrmi(file=.env,...)"` relative to `$CWD` --
unchanged by the migration, and distinct from the `$LSF_ENVDIR/env.qpu` that
`elim.qpu` reads. It no longer generates a bearer token from them.

What did change is the environment it *writes*: see "Enumerating backends
injects environment variables, which are pruned" above.

### New runtime dependency

Both scripts now import `qrmi`, which must be installed for the user that runs
them. **QRMI 0.25.1 or later is recommended**, since that is the first published
release containing `QuantumResource.status()` and therefore the `pending_jobs`
load index:

```bash
pip install -U "qrmi[ibm]"
```

Both scripts still run on 0.24.5, the previous release, with one index degraded.
`status()` is the only route to `pending_job_count`, and it landed after the
0.24.5 tag, so on that version they fall back to `is_accessible()` and report
`pending_jobs` as `-`. The other nine indices are unaffected. 0.24.5 has no
alternative source for the figure: `metadata()` returns only `backend_name` and
`session_id`, and while `ResourceProvider` does sort candidates by
`queue_length`, it discards the value after sorting rather than exposing it to
Python. No code change is needed when upgrading -- both scripts detect `status()`
and use it automatically.

Check the installed version with:

```bash
python3 -c "from importlib.metadata import version; print(version('qrmi'))"
```

(The `qrmi` module exposes no `__version__` attribute, so read it from the
package metadata.)

`elim.qpu` runs on the LSF master host, which may differ from the submission
hosts where the job starter runs; both need the package. The job starter no
longer needs `requests`.

## Verification performed

Both scripts were exercised against a mock `qrmi` module reproducing the QCS
payload shape, including `clops_h` and `sample_name`:

- `elim.qpu` emits the expected ELIM line, with `clops` and `processor_type`
  resolved from the vendor-specific fields (`Heron_r2`) and `pending_jobs` from
  `status()`.
- The legacy `APIKEY`/`CRN` mapping and the endpoint defaults apply as intended.
- With empty properties and an absent `pending_job_count`, `elim.qpu` emits `-`
  for each unavailable value and keeps the field count constant, where the
  previous implementation raised `TypeError`.
- `get_all_backend_metrics_qrmi()` returns exactly the key set that
  `get_all_backend_metrics_rest()` did — no missing or extra keys — so
  `select_device_priority()` needed no changes.
- All three selectors (`basic`, `health`, `priority`) choose a backend from
  QRMI-sourced data.
- A backend that raises during `target()` is recorded with an `error` key
  without aborting the selection, and is skipped by the topology builder.
- An unsupported `qpu_type` is rejected with a clear `ValueError`.
- `pyflakes` reports no new warnings against either original; two pre-existing
  ones are resolved.

Both scripts were then re-run against the **real `qrmi` 0.25.1 wheel from
PyPI**, rather than only the mock, to confirm the API surface the migration
depends on:

- `QuantumResource.status()` exists and `ResourceStatus` is exported, with
  fields `status`, `status_reason`, `healthy`, `busy`, `capacity`,
  `pending_job_count` and `to_dict()`.
- `elim.qpu` takes the `status()` path rather than the `is_accessible()`
  fallback, and emits all ten indices with a field count of exactly 10.
- The job starter preserves distinct per-backend queue lengths, and both
  `select_device_default()` and `select_device_health()` return a backend.

Environment pruning was verified against a mock that reproduces
`inject_backend_env()`'s `setenv()` behaviour over eight backends:

- The job environment retains only the selected device's six variables; the
  seven other devices' injected sets are gone.
- A site-set value for an unselected device that QRMI overwrote is restored to
  the site value, rather than removed or left as QRMI wrote it.
- The selected device keeps the provider's values, and unrelated variables are
  untouched.
- The job environment is byte-for-byte identical with debugging off, at `level1`
  and at `level2`; the debug levels only add the enumeration listing to the log.
- With no enumeration (the explicit `device=` path) the prune is a no-op.

The mock writes its variables through `libc.setenv()` rather than `os.environ`,
reproducing how Rust's `env::set_var()` behaves -- without that, the test passes
against a prune that does nothing in production.

`0.25.0..0.25.1` contains only a version bump and a rustc bump for a macOS
import error; there is no API difference in `pyext.rs`, `models.rs` or the IBM
Quantum Compute Service provider, so either release behaves identically here.

Not verified: no call was made against the live IBM Quantum Platform, which
would need real credentials and an LSF host — the payloads above came from the
mock, driven through the real wheel's types. The conclusion that
vendor-specific fields survive follows from the client's
`HashMap<String, Value>` signature, but **confirm that `clops_h` and
`sample_name` are present in a real `target()` payload before relying on the
CLOPS and processor-type resources**, since their presence depends on what the
service returns rather than on QRMI.

The pre-migration versions of both scripts remain available from git for
comparison:

```bash
git show HEAD:elim.qpu
git show HEAD:qrmi-esub-jobstarter.py
```

## Possible follow-ups

- Use `ResourceProvider.resources(filters)` with a filter string such as
  `num_qubits=127&status=online` to push selection into QRMI, replacing part of
  the Python-side filtering. QRMI already returns results sorted least-busy
  first, which is what the `basic` selector computes by hand.
- Expose the additional fields QRMI reports — `status`, `busy`, `status_reason`
  — as further ELIM resources, letting LSF avoid dispatching to a paused or
  busy device.
- Now that both scripts go through `ResourceType`, extend them beyond IBM.
  `get_all_backend_metrics_qrmi()` deliberately rejects the non-IBM types,
  because the metrics it extracts are IBM-shaped; the other vendors would need
  their own extraction.
- Rewrite `select_device_default()` to look up keys instead of relying on dict
  insertion order.
