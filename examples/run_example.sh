#!/bin/bash 

# -*- coding: utf-8 -*-

# This code is part of Qiskit.
#
# Copyright (C) 2025 UKRI-STFC (Hartree Centre), IBM
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.


# Fail loudly rather than passing empty arguments to the primitives below.
set -u

# Unset the unprefixed QRMI template variables. esub.qrmi re-keys the
# provider settings to the selected device (see scope_provider_vars in
# lsfq-core), so what arrives here is "<device>_QRMI_IBM_QCS_*" and the
# unprefixed names are already removed -- these unsets are belt-and-braces
# in case the job was launched by hand.
unset QRMI_IBM_QCS_SESSION_MODE
unset QRMI_IBM_QCS_IAM_APIKEY
unset QRMI_IBM_QCS_SERVICE_CRN
unset QRMI_IBM_QCS_IAM_ENDPOINT
unset QRMI_IBM_QCS_ENDPOINT

# The device esub.qrmi selected. Everything below is scoped to its name.
# esub.qrmi always sets this; a bare "unbound variable" from set -u would not
# say which variable or why, so report it directly.
if [ -z "${QRMI_IBM_QCS_BEST_DEVICE:-}" ]; then
    echo "run_example.sh: QRMI_IBM_QCS_BEST_DEVICE is not set;" \
         "this script expects to run under a job submitted via esub.qrmi" >&2
    exit 1
fi
device=$QRMI_IBM_QCS_BEST_DEVICE

# Read the device-scoped QRMI environment variables. The names carry the
# device prefix, so match on that to avoid picking up another backend's
# values if any are still in the environment.
endpoint=$(env | grep "^${device}_QRMI_IBM_QCS_ENDPOINT=" | cut -d= -f2-)
endpoint=${endpoint%/v1}
apikey=$(env | grep "^${device}_QRMI_IBM_QCS_IAM_APIKEY=" | cut -d= -f2-)
crn=$(env | grep "^${device}_QRMI_IBM_QCS_SERVICE_CRN=" | cut -d= -f2-)

for name in endpoint apikey crn; do
    if [ -z "${!name}" ]; then
        echo "run_example.sh: ${device}_QRMI_IBM_QCS_* is not set for $name" >&2
        exit 1
    fi
done

# Run Sampler and Estimator QRMI-enabled primitives
source ~/pyenv/bin/activate
for app in sampler estimator
do
  python3 "gen_${app}_inputs.py" "$device" "$endpoint" "$apikey" "$crn"
  python3 example.py "$device" "${app}_input_${device}_params_only.json" "$app"
done
