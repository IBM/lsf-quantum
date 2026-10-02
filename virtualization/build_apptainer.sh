#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "Usage: $0 LSF_X86_64_TARBALL OUTPUT.sif" >&2
    exit 1
fi
test "$EUID" -eq 0 || { echo "Run as root" >&2; exit 1; }
test "$(uname -m)" = x86_64 || {
    echo "This build wrapper currently supports native x86_64 only" >&2
    exit 1
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(dirname -- "$script_dir")
tarball=$(realpath -e -- "$1")
output=$(realpath -m -- "$2")
filename=$(basename -- "$tarball")

case "$filename" in
    lsfsce*-x86_64.tar.Z) ;;
    *) echo "Expected an LSF CE x86_64 distribution tarball" >&2; exit 1 ;;
esac

test ! -e "$output" || {
    echo "Output already exists: $output" >&2
    exit 1
}
test -d "$(dirname -- "$output")"

build_stage=$(mktemp -d /var/tmp/lsf-apptainer-build.XXXXXX)
trap 'rm -rf -- "$build_stage"' EXIT
mkdir -p "$build_stage/cache" "$build_stage/tmp"

env -i \
    HOME=/root \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    APPTAINER_CACHEDIR="$build_stage/cache" \
    APPTAINER_TMPDIR="$build_stage/tmp" \
    apptainer build \
    --build-arg "REPO_ROOT=$repo_root" \
    --build-arg "LSFTARFILE=$tarball" \
    --build-arg "LSFDISTRO=${filename%.tar.Z}" \
    --build-arg LSFINSTALLER=lsf10.1_lsfinstall_linux_x86_64.tar.Z \
    "$output" "$script_dir/lsfce.def"
