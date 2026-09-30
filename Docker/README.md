# Containerized LSF-QRMI test environment

Place the LSF CE archive in Docker/. Run the build helper from the repository root with Docker or Podman. It uses Docker/ as the build context and stages the current integration scripts for the build. The archive and runtime credentials must not be committed.

Build on amd64 with Docker:

    CONTAINER_ENGINE=docker ./Docker/build_podman.sh amd64 10.2.0.15

Start with hostname lsfmaster. Configure the QPU queue, resource map, and credentials at runtime.

## LSF-QRMI integration verification

Build from the repository root with the appropriate LSF CE archive and build
arguments. The image installs `qrmi[ibm]>=0.25.1`, `python-dotenv`, and
`omegaconf`, and installs `esub.qrmi`, `jobstarter.qrmi`, and `elim.qpu` into
LSF's server directory. The installed Python scripts use `/opt/qrmi-venv/bin/python`.

On an x86_64 single-node test container (`lsfmaster`), the following passed:

- LSF CE 10.1.0.15 started LIM, RES, and batch daemons; a normal LSF job completed.
- A temporary `quantum_test` queue with `JOB_STARTER=jobstarter.qrmi` accepted
  `bsub -a "qrmi(file=.env,device=ibm_fez)"`; job 3 completed after the QRMI
  resource acquire/release path and ran `/bin/hostname`.
- With `ibm_fez` and the dynamic indices configured in LSF, LIM started
  `elim.qpu`. `lsload -l lsfmaster` reported live QPU metrics including
  156 qubits, CLOPS, and pending jobs.

The test queue, QPU resource mapping, and credentials were configured only in
the disposable test container. A quantum circuit was not submitted to hardware.

### Three-platform validation

The final Dockerfile built successfully for amd64, arm64, and ppc64le. Each image passed LSF startup, host status, ESUB help, QRMI dependency imports, and a synchronous normal-queue hostname job.

AMD64 ran natively on the RHEL x86_64 test host. ARM64 and PPC64LE ran under QEMU emulation. On PPC64LE, the Power10-specific libc probe reported an illegal instruction; the standard libc worked, and LSF startup and job execution succeeded.

Authenticated QRMI jobstarter and live ELIM metrics were previously verified on AMD64. These QPU checks were not repeated on ARM64 or PPC64LE. No quantum circuit was submitted to hardware.
