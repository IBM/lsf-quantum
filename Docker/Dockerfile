# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.

FROM rockylinux:9

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
COPY install.config /tmp/

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

# LSF-QRMI integration (current repository scripts, including PR #16).
RUN dnf -y --enablerepo=crb install python3.11 python3.11-pip python3.11-devel gcc-gfortran openblas-devel lapack-devel \
 && dnf clean all \
 && python3.11 -m venv /opt/qrmi-venv \
 && /opt/qrmi-venv/bin/pip install --no-cache-dir \
      "qrmi[ibm]>=0.25.1" python-dotenv omegaconf

ENV PATH="/opt/qrmi-venv/bin:${PATH}"

COPY qrmi-esub-jobstarter.py elim.qpu /tmp/lsf-quantum/

RUN bash -c 'set -eux; \
    LSF_SERVERDIR="$(find /opt/lsf/10.1 -mindepth 2 -maxdepth 2 -type d -name etc -print -quit)"; \
    test -n "$LSF_SERVERDIR"; \
    install -m 755 /tmp/lsf-quantum/qrmi-esub-jobstarter.py "$LSF_SERVERDIR/qrmi-esub-jobstarter.py"; \
    install -m 755 /tmp/lsf-quantum/elim.qpu "$LSF_SERVERDIR/elim.qpu"; \
    sed -i "1c#!/opt/qrmi-venv/bin/python" "$LSF_SERVERDIR/qrmi-esub-jobstarter.py"; \
    sed -i "1c#!/opt/qrmi-venv/bin/python" "$LSF_SERVERDIR/elim.qpu"; \
    ln -s qrmi-esub-jobstarter.py "$LSF_SERVERDIR/esub.qrmi"; \
    ln -s qrmi-esub-jobstarter.py "$LSF_SERVERDIR/jobstarter.qrmi"' \
 && rm -rf /tmp/lsf-quantum \
 && /opt/qrmi-venv/bin/python -c "import qrmi, dotenv, omegaconf"

RUN printf '%s\n' '. /opt/lsf/conf/profile.lsf' >> /etc/profile

RUN rm -rf /tmp/install.config \
           /tmp/${LSFDISTRO} \
           /tmp/${LSFTARFILE}

COPY lsf-entrypoint.sh /usr/local/bin/lsf-entrypoint.sh

RUN chmod 755 /usr/local/bin/lsf-entrypoint.sh

ENTRYPOINT ["/usr/local/bin/lsf-entrypoint.sh"]
CMD ["bash"]
