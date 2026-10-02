#!/bin/bash
# SPDX-License-Identifier: Apache-2.0
set -e

if [ "$(hostname)" != lsfmaster ]; then
    echo "Run with --uts --hostname lsfmaster" >&2
    exit 1
fi

container_ip=$(ip -4 -o address show scope global |
    awk 'NR == 1 {split($4, a, "/"); print a[1]}')
if [ -z "$container_ip" ]; then
    echo "Run with --net --network bridge" >&2
    exit 1
fi

printf '127.0.0.1 localhost\n::1 localhost\n%s lsfmaster\n' \
    "$container_ip" > /etc/hosts
chmod 1777 /tmp /var/tmp

if [ "$#" -eq 0 ]; then
    set -- bash
fi
exec /usr/local/bin/lsf-entrypoint.sh "$@"
