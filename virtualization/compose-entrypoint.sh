#!/bin/bash
# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.

# Entrypoint for the compose test cluster. Every node, management or compute,
# runs the same image; this script is what makes them one cluster.
#
#   NODE_ROLE   master (default) or compute
#   LSF_NODES   space-separated compute host names in the cluster, the same on
#               every node (empty for a single-host cluster)
#
# It
#   1. installs optional QRMI credentials (management host only);
#   2. adds the compute hosts to this node's copy of lsf.cluster.<name>. The
#      image installs LSF for the one host `lsfmaster`, and each container has
#      its own /opt/lsf, so every node must be given the same host list;
#   3. starts the LSF daemons, in a way that also works inside the user
#      namespace of rootless Podman.

set -euo pipefail

LSF_TOP="${LSF_TOP:-/opt/lsf}"
profile="${LSF_TOP}/conf/profile.lsf"
role="${NODE_ROLE:-master}"
nodes="${LSF_NODES:-}"
master="lsfmaster"

log() { echo "[$(hostname)] $*"; }

if [[ ! -r "${profile}" ]]; then
    echo "ERROR: LSF profile is missing: ${profile}" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Credentials. compose.yaml mounts the host file read-only at /run/qrmi/env, or
# /dev/null when none was given. Copying it (rather than mounting it where the
# job looks) makes the owner and mode right on every engine. Only the
# management host needs it: the esub runs at submission and forwards the
# values to the job's environment.
# ---------------------------------------------------------------------------
src=/run/qrmi/env
dst=/home/lsfadmin/.env
if [[ "${role}" == master && -f "${src}" && -s "${src}" ]]; then
    install -m 600 -o lsfadmin -g lsfadmin "${src}" "${dst}"
    log "Installed QRMI credentials at ${dst}"
fi

# ---------------------------------------------------------------------------
# Cluster membership.
#
# Add each host in $2 to the Host section of cluster file $1 unless it is
# already there. The row is built from the file's own header, so it follows
# whatever columns this LSF release installs: HOSTNAME takes the host, model
# and type take "!" (detect), server takes 1, and every other column "()".
# Idempotent, so a restarted container does nothing.
# ---------------------------------------------------------------------------
register_hosts() {
    local file="$1" hosts="$2" tmp
    [[ -n "${hosts}" ]] || return 0

    tmp="$(mktemp)"
    awk -v hosts="${hosts}" '
        BEGIN { want_n = split(hosts, want, " ") }
        /^Begin[ \t]+Host/ { in_host = 1 }
        in_host && $1 == "HOSTNAME" && !ncols {
            for (i = 1; i <= NF && $i !~ /^#/; i++) col[++ncols] = $i
        }
        in_host && NF && $1 !~ /^#/ && $1 != "HOSTNAME" { have[$1] = 1 }
        in_host && /^End[ \t]+Host/ {
            if (!ncols) { print "no HOSTNAME header in Host section" > "/dev/stderr"; exit 2 }
            for (w = 1; w <= want_n; w++) {
                if (want[w] in have) continue
                row = ""
                for (i = 1; i <= ncols; i++) {
                    c = col[i]
                    v = (c == "HOSTNAME") ? want[w] : (c == "model" || c == "type") ? "!" : (c == "server") ? "1" : "()"
                    row = row (i > 1 ? "  " : "") v
                }
                print row
            }
            in_host = 0
        }
        { print }
    ' "${file}" > "${tmp}" || { rm -f "${tmp}"; return 1; }

    if cmp -s "${file}" "${tmp}"; then
        rm -f "${tmp}"
    else
        cat "${tmp}" > "${file}"   # keep the file's owner and mode
        rm -f "${tmp}"
        log "Registered compute hosts in ${file##*/}: ${hosts}"
    fi
}

cluster_file="$(find "${LSF_TOP}/conf" -maxdepth 1 -name 'lsf.cluster.*' -print -quit)"
if [[ -z "${cluster_file}" ]]; then
    echo "ERROR: no lsf.cluster.* under ${LSF_TOP}/conf" >&2
    exit 1
fi
register_hosts "${cluster_file}" "${nodes}"

# Names come from the compose network's DNS, which can trail the containers
# starting. LSF cannot resolve a host that is not there yet.
for host in "${master}" ${nodes}; do
    for _ in $(seq 1 120); do
        getent hosts "${host}" >/dev/null && continue 2
        sleep 1
    done
    echo "WARNING: ${host} does not resolve; continuing" >&2
done

# ---------------------------------------------------------------------------
# Daemons.
#
# lsadmin/badmin start the daemons through bctrld, which reads
# /proc/self/uid_map and refuses ("Permission denied") when root is inside a
# user namespace, as in rootless Podman. There the daemons are started
# directly. In the initial namespace the map is "0 0 4294967295".
# ---------------------------------------------------------------------------
in_user_namespace() {
    local inside outside count
    read -r inside outside count < /proc/self/uid_map
    [[ "${inside}" != 0 || "${outside}" != 0 || "${count}" != 4294967295 ]]
}

set +u
# shellcheck disable=SC1090
source "${profile}"
set -u

if in_user_namespace; then
    log "User namespace detected; starting the LSF daemons directly"
    start_lim()  { "${LSF_SERVERDIR}/lim"; }
    start_res()  { "${LSF_SERVERDIR}/res"; }
    start_sbd()  { "${LSF_SERVERDIR}/sbatchd"; }   # starts mbatchd on the master
else
    start_lim()  { lsadmin limstartup; }
    start_res()  { lsadmin resstartup; }
    start_sbd()  { badmin hstartup; }
fi

start_lim
if [[ "${role}" == compute ]]; then
    # A compute host's LIM reports to the master's; sbatchd needs it.
    log "Waiting for the management host"
    for _ in $(seq 1 180); do
        lsid >/dev/null 2>&1 && break
        sleep 2
    done
fi
start_res
start_sbd

log "LSF ${role} node started"
exec "$@"
