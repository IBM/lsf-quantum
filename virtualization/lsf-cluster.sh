#!/bin/bash
# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

# Run the LSF test cluster (compose.yaml) with the right architecture,
# container engine and number of hosts.
#
#   lsf-cluster.sh up -d --build     build the image if needed and start
#   lsf-cluster.sh up -d --wait      ... and wait until every host is ok
#   lsf-cluster.sh shell [HOST]      login shell as lsfadmin (default lsfmaster)
#   lsf-cluster.sh logs -f | ps | down | down -v ...   any compose command
#
# Environment:
#   LSF_COMPUTE_NODES  compute hosts besides lsfmaster, 0-4  (default: 2)
#   CONTAINER_ENGINE   podman or docker      (default: podman if installed)
#   LSF_TARGET_ARCH    x86_64 or arm64       (default: this machine's; set it
#                      to the other one to build and run under emulation)
#   LSF_VERSION        LSF CE release        (default: 10.2.0.15)
#   QRMI_ENV_FILE      host .env with QRMI credentials, optional

set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname -- "${here}")"
max_compute=4

if [[ $# -eq 0 || "$1" == "-h" || "$1" == "--help" ]]; then
    sed -n '/^# Run the LSF/,/^# *QRMI_ENV_FILE/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
fi

engine="${CONTAINER_ENGINE:-}"
if [[ -z "${engine}" ]]; then
    if command -v podman >/dev/null 2>&1; then engine=podman
    elif command -v docker >/dev/null 2>&1; then engine=docker
    else echo "ERROR: neither podman nor docker found" >&2; exit 1
    fi
fi

case "${LSF_TARGET_ARCH:-$(uname -m)}" in
    x86_64|amd64)         arch=x86_64 ;;
    arm64|aarch64|armv8)  arch=arm64 ;;
    *) echo "ERROR: unsupported architecture: ${LSF_TARGET_ARCH:-$(uname -m)}" >&2; exit 1 ;;
esac
envfile="${here}/lsf-cluster.${arch}.env"

# Read LSF_ARCH from the env file without leaking the rest into compose.
lsf_arch="$(sed -n 's/^LSF_ARCH=//p' "${envfile}")"
version="${LSF_VERSION:-10.2.0.15}"

n="${LSF_COMPUTE_NODES:-2}"
if ! [[ "${n}" =~ ^[0-9]+$ ]] || (( n > max_compute )); then
    echo "ERROR: LSF_COMPUTE_NODES must be 0-${max_compute}, got: ${n}" >&2
    exit 1
fi

# Every node is told the full host list (it registers the hosts in its own
# lsf.cluster file), so the same list goes to all of them.
nodes=()
for ((i = 1; i <= n; i++)); do nodes+=("$(printf 'compute%02d' "${i}")"); done
export LSF_NODES="${nodes[*]:-}"

# Starting selects the profiles of the hosts wanted. Everything else (down, ps,
# logs, ...) addresses all four, so `down` also removes hosts left over from a
# run with more nodes.
profiles=()
case "$1" in
    up|create|run|start|restart|build|config)
        for h in "${nodes[@]:-}"; do [[ -n "${h}" ]] && profiles+=(--profile "${h}"); done ;;
    *)
        for ((i = 1; i <= max_compute; i++)); do
            profiles+=(--profile "$(printf 'compute%02d' "${i}")")
        done ;;
esac

case "$1" in
    build|up|create|run)
        archive="${root}/lsfsce${version}-${lsf_arch}.tar.Z"
        # An up on an existing image does not need the archive; a build does.
        if [[ ! -f "${archive}" ]] && ! "${engine}" image inspect \
                "localhost/lsf-ce:${version}-${lsf_arch}" >/dev/null 2>&1; then
            echo "ERROR: LSF archive not found: ${archive}" >&2
            echo "       Download it first; see virtualization/README.md." >&2
            exit 1
        fi
        ;;
esac

if [[ "$1" == "shell" ]]; then
    exec "${engine}" exec -it -u lsfadmin -w /home/lsfadmin "${2:-lsfmaster}" bash -l
fi

# ${profiles[@]+...}: an empty array is "unbound" under `set -u` in bash 3.2.
exec "${engine}" compose --env-file "${envfile}" ${profiles[@]+"${profiles[@]}"} \
    -f "${here}/compose.yaml" "$@"
