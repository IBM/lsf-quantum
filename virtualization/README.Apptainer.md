# Apptainer LSF test environment

The native image contains LSF Community Edition, QRMI 0.25.1,
Qiskit, integration scripts, and a quantum_test queue configured
during the build.

Validated: x86_64, Apptainer 1.5.4, root-launched container,
batch jobs submitted as lsfadmin (UID/GID 1000).
Rootless startup and other architectures are untested.

## Build

From the repository root:

    ./virtualization/build_apptainer.sh \
        /absolute/path/lsfsce10.2.0.15-x86_64.tar.Z \
        /absolute/path/lsfce-amd64.sif

The output directory must exist. Existing images are not overwritten.

## Run

Create a fresh results directory:

    results=$(mktemp -d /var/tmp/lsf-apptainer-results.XXXXXX)
    chown 1000:1000 "$results"
    chmod 700 "$results"

    ./virtualization/run_apptainer.sh \
        /absolute/path/lsfce-amd64.sif "$results"

The wrapper isolates network, hostname, PID, and IPC namespaces.
Examples are mounted read-only; results are mounted read-write.
The host resolver configuration is mounted read-only.

The entrypoint configures lsfmaster and private temporary-directory
permissions, then starts LSF. The SIF remains unchanged.
Runtime LSF logs and spool changes are temporary; /results persists.

Inside the container:

    su -s /bin/bash lsfadmin
    source /opt/lsf/conf/profile.lsf
    lsid
    bhosts
    bqueues -l quantum_test

Wait until bhosts reports lsfmaster as ok.

## Mounted Bell example

Use a fresh results directory. Supply credentials at runtime:

    QRMI_CREDENTIALS_FILE=/absolute/path/.env \
        ./virtualization/run_apptainer.sh \
        /absolute/path/lsfce-amd64.sif "$results"

The credentials file must contain QRMI_JOB_QPU_TYPES set to
ibm-quantum-compute-service and QRMI_IBM_QCS_* settings.
The wrapper mounts a temporary copy at /credentials/.env,
readable by lsfadmin, and removes that copy when the run ends.
Credentials are not built into the image.

Inside the container, after LSF is ready:

    su -s /bin/bash lsfadmin
    source /opt/lsf/conf/profile.lsf
    export PATH="/opt/qrmi-venv/bin:$PATH"
    cd /credentials

    bsub -K -q quantum_test -J apptainer-bell \
        -a "qrmi(file=.env,device=ibm_fez)" \
        -oo /results/lsf.out -eo /results/lsf.err \
        /opt/qrmi-venv/bin/python /examples/apptainer_bell.py \
        --expected-uid "$(id -u)" --expected-gid "$(id -g)"

This submits a real 128-shot quantum job. The example uses the
resource acquired by the queue jobstarter without acquiring
or releasing a second resource.

Check lsf.out, lsf.err, and bell-result.json under /results.
Verify the quantum job ID, UID/GID 1000, and 128 total counts.

Known limitation: the existing explicit-device jobstarter masks
application failures by exiting zero. Verify the result JSON;
LSF DONE alone does not establish quantum success.

## Docker-image import

    apptainer build --sandbox lsfce-import \
        docker-daemon:lsf-ce:issue7-amd64

Imported images retain their configuration. The native run wrapper
requires apptainer-entrypoint.sh supplied by lsfce.def.

## Validation

- Imported Docker sandbox: isolated LSF startup and batch job passed.
- Native sandbox: baked quantum queue and batch execution passed.
- Native sandbox: real Bell job passed as UID/GID 1000.
- Native SIF: entrypoint, DNS, queue, and batch execution passed.
- Native SIF quantum job: davm1rg4oijs73e7qkd0.
- Counts: 00=63, 11=59, 10=4, 01=2; total 128.
- Reusable run wrapper: SIF batch, DNS, and queue checks passed.
- Build wrapper: shell syntax passed; equivalent direct build tested.
