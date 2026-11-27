# Containerized LSF-QRMI test environment

Place the LSF CE archive in virtualization/. Run the build helper from the repository root with Docker or Podman. It uses virtualization/ as the build context and stages the current integration scripts for the build. The archive and runtime credentials must not be committed.

Build on amd64 with Docker:

    CONTAINER_ENGINE=docker ./virtualization/build_podman.sh amd64 10.2.0.15

Start with hostname lsfmaster. The image includes quantum_test. Supply credentials at runtime; configure resource mappings separately for ELIM.

## Compose test cluster (Docker or Podman, x86_64 or arm64, multi-host)

`compose.yaml` runs the image built from `Dockerfile` as a long-lived LSF
cluster for testing the integration: the management host `lsfmaster` (it also
runs jobs) plus up to four execution hosts, `compute01` to `compute04`. The
wrapper picks the architecture, the container engine and the number of hosts.

1. Put the LSF Community Edition archive in the **repository root** (the build
   context), `lsfsce10.2.0.15-x86_64.tar.Z` or `lsfsce10.2.0.15-armv8.tar.Z`.
   It must not be committed.
2. Build and start, from anywhere:

       ./virtualization/lsf-cluster.sh up -d --build --wait   # lsfmaster + 2 compute hosts
       ./virtualization/lsf-cluster.sh shell                  # as lsfadmin on lsfmaster
       ./virtualization/lsf-cluster.sh shell compute02        # ... or on another host
       ./virtualization/lsf-cluster.sh down                   # `down -v` also drops the home volume

   `--wait` returns once the master's health check passes, which needs every
   host to be `ok` in `bhosts`. In the shell, `bhosts`, `lshosts` and
   `bqueues` work, and the `quantum_test` queue exists. Check that jobs reach
   the compute hosts with the `normal` queue (`quantum_test` wraps every job in
   `jobstarter.qrmi`, which needs QRMI settings):

       bsub -K -q normal -n 4 -R "span[ptile=2]" hostname   # two hosts, two slots each

| Setting | Meaning | Default |
| --- | --- | --- |
| `LSF_COMPUTE_NODES` | compute hosts besides `lsfmaster`, 0 to 4 | 2 |
| `LSF_TARGET_ARCH` | `x86_64` or `arm64`; the other one builds and runs under emulation | this machine |
| `CONTAINER_ENGINE` | `podman` or `docker` | `podman` if installed |
| `LSF_VERSION` | release in the archive name | `10.2.0.15` |
| `QRMI_ENV_FILE` | host `.env` with QRMI credentials | none |

| Host | Env file | LSF archive |
| --- | --- | --- |
| x86_64 / amd64 | `lsf-cluster.x86_64.env` | `lsfsce<ver>-x86_64.tar.Z` |
| arm64 / Apple silicon | `lsf-cluster.arm64.env` | `lsfsce<ver>-armv8.tar.Z` |

Without the wrapper, name the env file, one `--profile` per compute host, and
give `LSF_NODES` the same hosts (every node must be told the full list):

    LSF_NODES="compute01 compute02" podman compose \
        --env-file virtualization/lsf-cluster.arm64.env \
        --profile compute01 --profile compute02 \
        -f virtualization/compose.yaml up -d --build

**How the hosts form one cluster.** Every node runs the same image, which
installs LSF for the single host `lsfmaster`. At start, each node's
`compose-entrypoint.sh` adds the hosts in `LSF_NODES` to its own
`lsf.cluster.<name>` (using the file's own column header, and doing nothing on a
restart). The compute hosts then start their LIM, wait for the master, and start
`res` and `sbatchd`. So the host count changes without rebuilding the image.
Only `/home/lsfadmin` is shared between hosts (the `lsf_home` volume): job
working directories and output are visible everywhere. LSF's own tree is per
node, with identical configuration.

**Credentials.** Point `QRMI_ENV_FILE` at a host `.env` (format below). The
management host copies it to `/home/lsfadmin/.env` with mode 600, where
`-a "qrmi(file=.env,...)"` looks; the esub forwards the values to the job, so the
compute hosts need no copy. It is never part of the image.

    QRMI_ENV_FILE=$HOME/qrmi.env ./virtualization/lsf-cluster.sh up -d

**Examples.** `examples/` is mounted read-only at `/opt/lsf-quantum-examples` on
every host.

**Rootless Podman.** `lsadmin`/`badmin` start the daemons through `bctrld`,
which refuses inside a user namespace. `compose-entrypoint.sh` detects that and
starts `lim`, `res` and `sbatchd` directly.

**Limits.**
- The cluster has one management host. Failover would need master candidates
  in `LSF_MASTER_LIST`, which `install.config` sets to `lsfmaster` alone.
- The image does not yet contain `schmod_qrmi`. To test it, build it
  (`scheduler-plugin/`), copy `schmod_qrmi.so` into `$LSF_LIBDIR` on
  `lsfmaster`, add it to `lsb.modules` and run `badmin mbdrestart`.
- Compute hosts may appear in `bhosts` with the network's DNS suffix (for
  example `compute01.dns.podman`); the sibling `lsf-integration` cluster sees the
  same and dispatches normally.
- Validated so far: the compose file's syntax, profiles and variable
  substitution (with `podman-compose`, arm64), the wrapper, the health check
  expression, and the host-registration step against LSF's own `lsf.cluster`
  template. Not run end to end: that needs the LSF archive. Multi-host
  authentication relies on LSF's default `eauth` behaving the same on every
  node, which is the first thing to check if `bhosts` shows a compute host
  `unavail`.

## LSF-QRMI integration verification

The LSF integration (`esub.qrmi`, `jobstarter.qrmi`, `elim.qpu`) is built
from this repository's Rust sources in the Dockerfile's first stage and
installed into the LSF server directory as three native binaries. The
runtime image contains no interpreter for them.

The image also carries a Python venv at `/opt/qrmi-venv` with
`qrmi[ibm]>=0.26.0` and `python-dotenv`, used **only by the quantum
workloads** a user submits (the Qiskit circuits in `examples/`), not by the
scheduler integration. Because the build context is the repository root,
`podman build` is invoked from `build_podman.sh` rather than directly.

All three images passed build, daemon startup, host status, ESUB help, dependency imports, and normal LSF job execution. AMD64 ran natively on RHEL x86_64; ARM64 and PPC64LE ran under QEMU.

### Quantum hardware execution

Each container submitted a 2-qubit Bell circuit with 128 shots to `ibm_fez` through LSF ESUB, `JOB_STARTER`, and QRMI SamplerV2. Every LSF job completed successfully and retrieved counts totaling 128 shots.

| Architecture | LSF job | Quantum job ID | Counts (00, 11, 10, 01) |
| --- | --- | --- | --- |
| AMD64 | 2 | dauibibg95ks73eievf0 | 57, 62, 7, 2 |
| ARM64 | 2 | dauio1ihcrkc73dvnmg0 | 57, 60, 5, 6 |
| PPC64LE | 3 | dauissbojkfs738s8sqg | 63, 55, 2, 8 |

Queues, credentials, and the circuit application were configured only in disposable test containers. Live ELIM metrics were additionally verified on AMD64; ELIM metrics were not tested on ARM64 or PPC64LE.

### PPC64LE emulation limitation

The unmodified image reported an illegal instruction when the LSF profile explicitly probed Power10-specific libc under QEMU. Standard libc, daemon startup, and normal job execution worked. For the PPC64LE quantum test, the running container profile was modified to exclude the Power10 libc from that probe. This workaround is not included in the Dockerfile. Native ARM64 and PPC64LE execution was not tested.

## Running a quantum job

These instructions use Docker and an image built by the build helper.
For Podman, replace `docker` with `podman`.

### Start the container

Run from the repository root. The examples mount is read-only; `:z` permits shared container access on SELinux hosts.

```bash
docker run --rm -d \
  --name lsf-qrmi-demo \
  --hostname lsfmaster \
  -v "$PWD/examples:/opt/lsf-quantum-examples:ro,z" \
  localhost/lsf-ce:latest sleep infinity

docker logs lsf-qrmi-demo
```

Wait until the logs show LIM, RES, and the batch daemon have started.
Check the cluster as `lsfadmin`:

```bash
docker exec -u lsfadmin lsf-qrmi-demo bash -lc \
  '. /opt/lsf/conf/profile.lsf; lsid; bhosts; bqueues'
```

### Interactive container

Start an interactive shell with Podman:

    podman run --rm -it --hostname lsfmaster localhost/lsf-ce:latest /bin/bash

The entrypoint starts LSF. Switch to the LSF administrator:

    su - lsfadmin

Then run these commands in the new shell:

    . /opt/lsf/conf/profile.lsf
    lsid
    bhosts
    bqueues
    bsub -K -q normal /bin/hostname

Verified with rootful Podman on AMD64. Rootless AMD64 testing failed during LSF startup; see the limitation below.

### Supply runtime credentials

Prepare a local `.env` file containing your IBM Quantum configuration:

```dotenv
QRMI_JOB_QPU_TYPES=ibm-quantum-compute-service
QRMI_IBM_QCS_IAM_APIKEY=<your-api-key>
QRMI_IBM_QCS_SERVICE_CRN=<your-service-crn>
QRMI_IBM_QCS_ENDPOINT=https://quantum.cloud.ibm.com
QRMI_IBM_QCS_IAM_ENDPOINT=https://iam.cloud.ibm.com
QRMI_IBM_QCS_SESSION_MODE=batch
```

Use the endpoints appropriate for your service. Do not commit this file
or include it in the image.

Copy it into the running container and restrict access:

```bash
docker cp .env lsf-qrmi-demo:/home/lsfadmin/.env

docker exec lsf-qrmi-demo bash -c '
  chown lsfadmin:lsfadmin /home/lsfadmin/.env
  chmod 600 /home/lsfadmin/.env
'
```

### Check the quantum queue and mounted example

The image includes `quantum_test`, configured with `jobstarter.qrmi`.
No runtime queue editing or reconfiguration is needed.

Check the queue:

    docker exec -u lsfadmin lsf-qrmi-demo bash -lc '. /opt/lsf/conf/profile.lsf; bqueues -l quantum_test'

Check the mounted example:

    docker exec -u lsfadmin lsf-qrmi-demo test -r /opt/lsf-quantum-examples/bell_test.py

The example submits a two-qubit Bell circuit with 128 shots using
the resource selected by the jobstarter.

### Submit and inspect the job

Use a backend accessible to your IBM Quantum instance. The validation
used `ibm_fez`. This command submits a real hardware job and consumes
quantum service usage:

```bash
docker exec -u lsfadmin -w /home/lsfadmin lsf-qrmi-demo bash -lc '
  . /opt/lsf/conf/profile.lsf
  bsub -q quantum_test \
    -a "qrmi(file=.env,device=ibm_fez)" \
    -o /home/lsfadmin/bell-test.%J.out \
    /opt/qrmi-venv/bin/python /opt/lsf-quantum-examples/bell_test.py
'
```

Record the LSF job ID printed by `bsub`. Replace `123` below with that ID:

```bash
docker exec -u lsfadmin lsf-qrmi-demo bash -lc '
  . /opt/lsf/conf/profile.lsf
  bjobs -a 123
  bpeek 123
'
```

The LSF job can remain `RUN` while the remote quantum job waits for hardware.
`bpeek` displays output while the LSF job is running. After completion,
inspect its history and saved output:

```bash
docker exec -u lsfadmin lsf-qrmi-demo bash -lc '
  . /opt/lsf/conf/profile.lsf
  bhist -l 123
  cat /home/lsfadmin/bell-test.123.out
'
```

Verify successful LSF completion and application output containing the
quantum job ID, measurement counts, and
`Quantum result verified: 128 shots`. Counts vary between executions.

Run these commands inside the container: the host may belong to a different
LSF cluster. An interactive shell is also available:

```bash
docker exec -it -u lsfadmin lsf-qrmi-demo bash -l
```

### Architecture and ELIM notes

On a host with a different architecture, specify the image platform at
launch and provide suitable emulation. ARM64 and PPC64LE validation used
QEMU; the PPC64LE profile workaround is described above.

Explicit-device submission does not require live ELIM metrics. To enable
those metrics, additionally configure the QPU resource definitions and
mappings in LSF and provide `$LSF_ENVDIR/env.qpu`.

### Cleanup

After the job finishes, stop the disposable container:

```bash
docker stop lsf-qrmi-demo
```

The `--rm` option removes the container and its runtime credentials,
configuration, and output files. Copy any results you want to retain
before stopping it.

### Podman validation

Validated with rootful Podman 5.8.2 on a RHEL x86_64 host.
AMD64 ran natively; ARM64 and PPC64LE ran under QEMU.
Each fresh image provided `quantum_test` without runtime queue editing.
The examples directory was mounted read-only.

All three platforms passed LSF startup, host status, QRMI imports,
mounted example access, normal hostname job execution, and a Bell
hardware job through ESUB, JOB_STARTER, and QRMI SamplerV2.
Every Bell job completed successfully and returned 128 shots.

| Architecture | LSF job | Quantum job ID | Counts (00, 11, 10, 01) |
| --- | --- | --- | --- |
| AMD64 | 2 | dav6pdk92g1c7398ddn0 | 55, 65, 5, 3 |
| ARM64 | 2 | dav7tgdj371s73dmg1m0 | 64, 56, 7, 1 |
| PPC64LE | 2 | davlipc92g1c73994ueg | 56, 67, 3, 2 |

PPC64LE reported the Power10 libc probe warning described above.
Its Podman quantum job succeeded without modifying the container profile.

Native ARM64/PPC64LE execution was not tested.
Live ELIM metrics were previously verified on AMD64 only.

## Apptainer

See [the Apptainer workflow](README.Apptainer.md) for native LSF image builds, isolated execution, and the mounted Bell example.

### Rootless Podman limitation

On AMD64, rootless Podman reproduced three startup permission errors. Tracing bctrld showed it reading the UID map, where container root maps to host UID 1000, and immediately returning Permission denied. Direct daemon startup succeeded, but LSF client authentication and job submission failed. Use rootful Podman for this tested full LSF environment. Rootless ARM64 and PPC64LE were not tested.
