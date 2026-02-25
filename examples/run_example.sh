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


# Unset QRMi tempalte variables passed over by esub.qrmi
unset QRMI_IBM_QRS_SESSION_MODE
unset  QRMI_IBM_QRS_IAM_APIKEY
unset QRMI_IBM_QRS_SERVICE_CRN
unset QRMI_IBM_QRS_SESSION_MODE
unset QRMI_IBM_QRS_IAM_ENDPOINT
unset QRMI_IBM_QRS_ENDPOINT

# Read device specific QRMI environment variables
endpoint=$(env |grep QRMI_IBM_QRS_ENDPOINT |awk -F= '{print $2}')
endpoint=${endpoint%/v1}
apikey=$(env |grep QRMI_IBM_QRS_IAM_APIKEY |awk -F= '{print $2}')
crn=$(env |grep QRMI_IBM_QRS_SERVICE_CRN |awk -F= '{print $2}')

# Run Sampler and Estimator QRMI-enabled primitives
source ~/pyenv/bin/activate
for app in sampler estimator
do	
  python3 "gen_"$app"_inputs.py" $QRMI_IBM_QRS_BEST_DEVICE $endpoint $apikey $crn
  python3 example.py $QRMI_IBM_QRS_BEST_DEVICE $app"_input_"$QRMI_IBM_QRS_BEST_DEVICE"_params_only.json" $app
done
