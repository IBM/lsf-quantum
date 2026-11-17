# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.

# ---------------------------------------------------------------------------
# Stage 1: build the three Rust binaries.
#
# Kept separate so the runtime image carries neither the Rust toolchain nor
# QRMI's build dependencies (openssl-devel, a C compiler, Python headers for
# pyo3). The previous Python integration needed python3.11, a venv and three
# pip packages *at runtime*; this needs none of that in the final image.
#
# Note the build context is the REPOSITORY ROOT, not virtualization/ --
# build_podman.sh and build_apptainer.sh pass it accordingly.
# ---------------------------------------------------------------------------
FROM docker.io/library/rockylinux:9 AS lsfq-builder

# QRMI is a git dependency and links against OpenSSL; its transitive AWS SDK
# requires a recent toolchain, so rustup is used rather than the distro's
# older rust package.
#
# `--allowerasing` is required, not cosmetic. Without it this install fails
# on Rocky 9 with:
#
#   nothing provides openssl-libs(aarch-64) = 1:3.5.8-2.el9_8
#   needed by openssl-devel-1:3.5.8-2.el9_8
#
# A -devel package requires its runtime library at exactly the same
# version, and the base image ships an older openssl-libs. dnf will not
# *upgrade* an already-installed package to satisfy a new install request
# unless permitted to replace it, so the pair is unsatisfiable by default
# even though both versions are present in the repositories (openssl-libs
# in baseos, openssl-devel in appstream). `--allowerasing` lets dnf
# upgrade openssl-libs in the same transaction.
#
# Naming `openssl-libs` explicitly in the install list also works, but
# `--allowerasing` is kept because it covers any future -devel package
# with the same base-image skew rather than hardcoding one name.
#
# Verified by building this layer on linux/arm64 with podman, 2026-10-09.
RUN dnf -y --enablerepo=crb --setopt=install_weak_deps=False \
      --allowerasing install \
      gcc gcc-c++ make cmake git openssl-devel zlib-devel \
      python3.11 python3.11-devel \
 && dnf clean all \
 && rm -rf /var/cache/dnf

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
ENV PATH="/opt/cargo/bin:${PATH}"
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates

# --locked so the image is reproducible: the pinned QRMI tag and the
# MSRV-compatible dependency set in Cargo.lock are used as committed.
RUN cargo build --release --locked \
      -p esub-qrmi -p jobstarter-qrmi -p elim-qpu

# LSF expects these exact filenames. A Rust crate name cannot contain a dot,
# so the artifacts are renamed here rather than in the runtime stage.
RUN mkdir -p /out \
 && install -m 755 target/release/esub-qrmi       /out/esub.qrmi \
 && install -m 755 target/release/jobstarter-qrmi /out/jobstarter.qrmi \
 && install -m 755 target/release/elim-qpu        /out/elim.qpu

# ---------------------------------------------------------------------------
# Stage 2: the LSF image.
# ---------------------------------------------------------------------------
FROM docker.io/library/rockylinux:9

ARG LSFTARFILE=""
ARG LSFINSTALLER=""
ARG LSFDISTRO=""

LABEL org.opencontainers.image.title="IBM Spectrum LSF Community Edition"
LABEL org.opencontainers.image.description="LSF CE running on Rocky Linux"

ENV LSF_TOP=/opt/lsf

RUN dnf -y update && \
    dnf -y install \
        java-17-openjdk-headless \
        ed \
        hostname \
        which \
        procps-ng \
        net-tools \
        iproute \
        tar \
        gzip \
        unzip \
        sudo \
        passwd \
        openssh-clients \
        openssh-server \
        perl \
        ksh \
        tcsh \
        rsync \
        libnsl \
        libtirpc \
        findutils \
        libxcrypt-compat \
        wget && \
    dnf clean all

RUN mkdir -p ${LSF_TOP}

RUN useradd lsfadmin

COPY ${LSFTARFILE} /tmp/
COPY virtualization/install.config /tmp/

RUN cd /tmp/ && \
    tar zxf ${LSFTARFILE} 

WORKDIR /tmp/${LSFDISTRO}/lsf
RUN tar zxf ${LSFINSTALLER}

ENV LSF_TOP=/opt/lsf
ENV JAVA_HOME=/usr/lib/jvm/jre-17-openjdk
ENV PATH="${JAVA_HOME}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

WORKDIR /tmp/${LSFDISTRO}/lsf/lsf10.1_lsfinstall
RUN sed -i "s@^[[:space:]]*LSF_TARDIR=.*@LSF_TARDIR=\"/tmp/${LSFDISTRO}/lsf/\"@" /tmp/install.config \
 && ./lsfinstall -f /tmp/install.config

# A Python runtime for the *workloads*, not for the integration.
#
# The scheduler integration (esub, jobstarter, ELIM) is now Rust and needs
# no interpreter. But the quantum jobs a user submits through it -- the
# Qiskit circuits in examples/, and anything they write themselves -- are
# Python, because Qiskit is. The venv below exists only for those; the LSF
# binaries do not use it.
#
# Drop this layer if the image is only ever used to schedule externally
# supplied containers.
RUN dnf -y --enablerepo=crb install python3.11 python3.11-pip \
 && dnf clean all \
 && python3.11 -m venv /opt/qrmi-venv \
 && /opt/qrmi-venv/bin/pip install --no-cache-dir "qrmi[ibm]>=0.26.0" python-dotenv \
 && /opt/qrmi-venv/bin/python -c "import qrmi, dotenv"

ENV PATH="/opt/qrmi-venv/bin:${PATH}"

# LSF-QRMI integration: the Rust binaries from stage 1.
#
# No interpreter, no venv and no pip packages. The symlink pair the Python
# needed (esub.qrmi and jobstarter.qrmi both pointing at one script that
# branched on argv[0]) is gone: these are three independent binaries.
COPY --from=lsfq-builder /out/ /tmp/lsf-quantum/

RUN bash -c 'set -eux; \
    LSF_SERVERDIR="$(find /opt/lsf/10.1 -mindepth 2 -maxdepth 2 -type d -name etc -print -quit)"; \
    test -n "$LSF_SERVERDIR"; \
    install -m 755 /tmp/lsf-quantum/esub.qrmi       "$LSF_SERVERDIR/esub.qrmi"; \
    install -m 755 /tmp/lsf-quantum/jobstarter.qrmi "$LSF_SERVERDIR/jobstarter.qrmi"; \
    install -m 755 /tmp/lsf-quantum/elim.qpu        "$LSF_SERVERDIR/elim.qpu"' \
 && rm -rf /tmp/lsf-quantum

# Fail the build rather than ship an image whose binaries cannot start --
# a missing shared library would otherwise only surface when LSF ran them.
RUN bash -c 'set -eux; \
    LSF_SERVERDIR="$(find /opt/lsf/10.1 -mindepth 2 -maxdepth 2 -type d -name etc -print -quit)"; \
    "$LSF_SERVERDIR/esub.qrmi" --help > /dev/null'

# Configure the quantum queue in the image.
RUN bash -c 'set -eu; \
    serverdir="$(find /opt/lsf/10.1 -mindepth 2 -maxdepth 2 -type d -name etc -print -quit)"; \
    test -x "$serverdir/jobstarter.qrmi"; \
    queues=/opt/lsf/conf/lsbatch/lsfce/configdir/lsb.queues; \
    test -f "$queues"; \
    printf "\nBegin Queue\nQUEUE_NAME = quantum_test\nPRIORITY = 30\nJOB_STARTER = %s/jobstarter.qrmi\nDESCRIPTION = QRMI quantum queue\nEnd Queue\n" "$serverdir" >> "$queues"'

WORKDIR /home/lsfadmin

RUN printf '%s\n' '. /opt/lsf/conf/profile.lsf' >> /etc/profile

RUN rm -rf /tmp/install.config \
           /tmp/${LSFDISTRO} \
           /tmp/${LSFTARFILE}

COPY virtualization/lsf-entrypoint.sh /usr/local/bin/lsf-entrypoint.sh

RUN chmod 755 /usr/local/bin/lsf-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/lsf-entrypoint.sh"]
CMD ["bash"]
