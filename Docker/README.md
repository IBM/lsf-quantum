# Containerized LSF-QRMI test environment

Place the LSF CE archive in Docker/. Run the build helper from the repository root with Docker or Podman. It uses Docker/ as the build context and stages the current integration scripts for the build. The archive and runtime credentials must not be committed.

Build on amd64 with Docker:

    CONTAINER_ENGINE=docker ./Docker/build_podman.sh amd64 10.2.0.15

Start with hostname lsfmaster. Configure the QPU queue, resource map, and credentials at runtime.

## LSF-QRMI integration verification

The image installs `qrmi[ibm]>=0.25.1`, `python-dotenv`, and `omegaconf`. Integration scripts are installed in the LSF server directory and use `/opt/qrmi-venv/bin/python`.

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

```bash
docker run --rm -d \
  --name lsf-qrmi-demo \
  --hostname lsfmaster \
  localhost/lsf-ce:latest sleep infinity

docker logs lsf-qrmi-demo
```

Wait until the logs show LIM, RES, and the batch daemon have started.
Check the cluster as `lsfadmin`:

```bash
docker exec -u lsfadmin lsf-qrmi-demo bash -lc \
  '. /opt/lsf/conf/profile.lsf; lsid; bhosts; bqueues'
```

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

### Configure a quantum queue

Create a queue whose jobstarter is the installed QRMI integration script:

```bash
docker exec lsf-qrmi-demo bash -c '
  set -e
  serverdir=$(find /opt/lsf/10.1 -mindepth 2 -maxdepth 2 \
    -type d -name etc -print -quit)
  test -x "$serverdir/jobstarter.qrmi"
  queues=/opt/lsf/conf/lsbatch/lsfce/configdir/lsb.queues

  if ! grep -qE "^QUEUE_NAME[[:space:]]*=[[:space:]]*quantum_test[[:space:]]*$" "$queues"; then
    cp -p "$queues" "$queues.before-quantum-test"
    printf "\nBegin Queue\nQUEUE_NAME = quantum_test\nPRIORITY = 30\nJOB_STARTER = %s/jobstarter.qrmi\nDESCRIPTION = QRMI quantum test queue\nEnd Queue\n" \
      "$serverdir" >> "$queues"
  fi
'

docker exec -u lsfadmin lsf-qrmi-demo bash -lc '
  . /opt/lsf/conf/profile.lsf
  badmin ckconfig && badmin reconfig
'
```

Reconfiguration is asynchronous. Check that the queue is available before
submitting:

```bash
docker exec -u lsfadmin lsf-qrmi-demo bash -lc \
  '. /opt/lsf/conf/profile.lsf; bqueues quantum_test'
```

### Create the quantum application

The following application submits a two-qubit Bell circuit with 128 shots.
The jobstarter supplies the selected resource and its authenticated
environment; the application uses that resource through QRMI.

```bash
docker exec -i -u lsfadmin lsf-qrmi-demo \
  /opt/qrmi-venv/bin/python - <<'PY'
from pathlib import Path

Path("/home/lsfadmin/bell_test.py").write_text("""
import os
from qiskit import QuantumCircuit
from qiskit.transpiler.preset_passmanagers import generate_preset_pass_manager
from qrmi import QuantumResource, ResourceType
from qrmi.primitives.ibm import SamplerV2, get_target

device = os.environ["QRMI_JOB_QPU_RESOURCES"]
assert os.environ["QRMI_JOB_QPU_TYPES"] == "ibm-quantum-compute-service"
resource = QuantumResource(device, ResourceType.IBMQuantumComputeService)

circuit = QuantumCircuit(2)
circuit.h(0)
circuit.cx(0, 1)
circuit.measure_all()

target = get_target(resource)
isa = generate_preset_pass_manager(
    optimization_level=1, target=target
).run(circuit)

job = SamplerV2(resource).run([isa], shots=128)
print("Quantum job ID:", job.job_id(), flush=True)
result = job.result()
counts = result[0].data.meas.get_counts()
print("Counts:", counts, flush=True)
assert sum(counts.values()) == 128
print("Quantum result verified: 128 shots", flush=True)
""")
PY
```

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
    /opt/qrmi-venv/bin/python /home/lsfadmin/bell_test.py
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
