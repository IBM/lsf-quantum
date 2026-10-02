#!/bin/bash

# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.


if [ "$#" -ne 2 ]; then
    echo "Usage: $0 <architecture> <lsf_version>"
    echo "       Example: $0 arm64 10.2.0.15"
    echo " "
    exit 1
fi

arch=$1
lsf_version=$2

case "$arch" in 
        amd64|x86_64)           lsf_arch="x86_64"
                                podman_arch="amd64"
                                lsf_installer="lsf10.1_lsfinstall_linux_x86_64.tar.Z"
                                ;; 
        arm64|aarch64|armv8)    lsf_arch="armv8"
                                podman_arch="arm64"
                                lsf_installer="lsf10.1_no_jre_lsfinstall.tar.Z"
                                ;; 
        ppc64le)                lsf_arch="ppc64le"; 
                                podman_arch="ppc64le"
                                lsf_installer="lsf10.1_lsfinstall_linux_ppc64le.tar.Z"
                                ;; 
        *) 
                                echo "Unsupported architecture: $arch" >&2; 
                                exit 1; 
                                ;; 
esac; 

set -x
lsf_tarfile="lsfsce$lsf_version-$lsf_arch.tar.Z"
lsf_distro="${lsf_tarfile%.*.*}"

podman build \
  --arch $podman_arch \
  --build-arg LSFTARFILE=$lsf_tarfile \
  --build-arg LSFDISTRO=$lsf_distro \
  --build-arg LSFINSTALLER=$lsf_installer \
  --os linux \
  -t localhost/lsf-ce:latest \
  .
