#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "Usage: $0 IMAGE.sif RESULTS_DIRECTORY [COMMAND ...]" >&2
    exit 1
fi
test "$EUID" -eq 0 || { echo "Run as root" >&2; exit 1; }

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(dirname -- "$script_dir")
image=$(realpath -e -- "$1")
results=$(realpath -e -- "$2")
shift 2
test -f "$image"
test -d "$results"

run_stage=$(mktemp -d /var/tmp/lsf-apptainer-run.XXXXXX)
trap 'rm -rf -- "$run_stage"' EXIT
printf '127.0.0.1 localhost\n::1 localhost\n' > "$run_stage/hosts"

binds=(
    --bind "$run_stage/hosts:/etc/hosts:rw"
    --bind /etc/resolv.conf:/etc/resolv.conf:ro
    --bind "$repo_root/examples:/examples:ro"
    --bind "$results:/results:rw"
)

for path in "$repo_root/examples" "$results" "$run_stage"; do
    case "$path" in
        *:*|*,*|*$'\n'*)
            echo "Bind paths must not contain colons, commas, or newlines" >&2
            exit 1
            ;;
    esac
done

if [ -n "${QRMI_CREDENTIALS_FILE:-}" ]; then
    credentials=$(realpath -e -- "$QRMI_CREDENTIALS_FILE")
    test -f "$credentials"
    install -d -o 1000 -g 1000 -m 700 "$run_stage/credentials"
    install -o 1000 -g 1000 -m 600 \
        "$credentials" "$run_stage/credentials/.env"
    binds+=(--bind "$run_stage/credentials:/credentials:ro")
fi

if [ "$#" -eq 0 ]; then
    set -- bash
fi

env -i \
    HOME=/root \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    apptainer run \
    --writable-tmpfs --cleanenv --containall --no-eval \
    --no-mount hostfs,bind-paths,cwd \
    --net --network bridge \
    --uts --hostname lsfmaster \
    "${binds[@]}" \
    --pwd / \
    "$image" "$@"
