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
