#!/bin/bash

LSF_TOP="${LSF_TOP:-/opt/lsf}"
LSF_PROFILE="${LSF_TOP}/conf/profile.lsf"

if [[ ! -r "${LSF_PROFILE}" ]]; then
    echo "ERROR: LSF profile is missing: ${LSF_PROFILE}" >&2
    exit 1
fi

source "${LSF_PROFILE}"

lsadmin limstartup
lsadmin resstartup
badmin hstartup

exec "$@"
