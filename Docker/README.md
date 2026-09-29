# Containerized LSF-QRMI test environment

Place the LSF CE archive in Docker/. Build from the repository root with Docker, or run ./Docker/build_podman.sh on a host with Podman. The archive and runtime credentials must not be committed.

Docker build on amd64:

    docker build --platform linux/amd64 -f Docker/Dockerfile --build-arg LSFTARFILE=lsfsce10.2.0.15-x86_64.tar.Z --build-arg LSFDISTRO=lsfsce10.2.0.15-x86_64 --build-arg LSFINSTALLER=lsf10.1_lsfinstall_linux_x86_64.tar.Z -t lsf-quantum:issue7 .

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
