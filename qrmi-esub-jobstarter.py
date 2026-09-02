#!/usr/bin/env python3
# -*- coding: utf-8 -*-
# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.

import sys
import os
import time
import json
import requests
import subprocess
from dotenv import dotenv_values
from operator import itemgetter

from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from omegaconf import MISSING, OmegaConf

# Helpers

def print_debug(message, var=None):
    """
    Print message to stderr
    stdout is not available to esub
    """
    if debug:
        if message == None:
            return
        message = "[DEBUG] " + message
        if var:
            message = message + " {}"
            print(message.format(var), file=sys.stderr)
        else:
            print(message, file=sys.stderr)

def print_error(message):
    """
    Print to stderr and exit
    """
    if message != None:
        message = "[ERROR] " + message
        print(message, file=sys.stderr)

    if esub_abort_val:
        sys.exit(int(esub_abort_val))
    else:    
        sys.exit(1)

# Arguments parser

@dataclass
class QPUAttributes:
    """Attributes used to describe and select a quantum processing unit."""

    # Number of qubits on the QPU
    qubits: Optional[int] = None

    # QPU version
    qpu_version: Optional[str] = None

    # QPU processor type
    processor_type: Optional[str] = None

    # Hardware-aware circuit layer operations per second
    clops: Optional[float] = None

    # Number of jobs currently pending on the QPU
    pending_jobs: Optional[int] = None

    # Median QPU readout error
    readout_error_median: Optional[float] = None

    # Median QPU SX-gate error
    sx_error_median: Optional[float] = None

    # Median QPU CZ-gate error
    cz_error_median: Optional[float] = None

    # Median T1 coherence time in microseconds
    T1_median_us: Optional[float] = None

    # Median T2 coherence time in microseconds
    T2_median_us: Optional[float] = None

@dataclass
class Config:
    """Application command-line configuration."""

    # File containing user REST API credentials
    file: Path = MISSING

    # QPU selection policy
    selector: str = "basic"

    # Explicit QPU device name, when required
    device: Optional[str] = None

    # Requested or reported QPU attributes
    qpu: Optional[QPUAttributes] = None

def validate_qpu_attributes(qpu: QPUAttributes) -> None:
    """Validate only the QPU attributes that were supplied."""

    if qpu.qubits is not None and qpu.qubits <= 0:
        print_error("qpu.qubits must be greater than zero")

    if qpu.pending_jobs is not None and qpu.pending_jobs < 0:
        print_error("qpu.pending_jobs must not be negative")

    nonnegative_metrics = {
        "clops": qpu.clops,
        "readout_error_median": qpu.readout_error_median,
        "sx_error_median": qpu.sx_error_median,
        "cz_error_median": qpu.cz_error_median,
        "T1_median_us": qpu.T1_median_us,
        "T2_median_us": qpu.T2_median_us,
    }

    for name, value in nonnegative_metrics.items():
        if value is not None:
            if value < 0:
              print_error(f"qpu.{name} must not be negative")


def parse_config() -> Config:
    """Parse and validate key=value command-line arguments."""

    defaults = OmegaConf.structured(Config)
    cli = OmegaConf.from_cli()

    # Prevent command-line arguments from introducing unknown fields.
    OmegaConf.set_struct(defaults, True)
    cfg = OmegaConf.merge(defaults, cli)

    # Detect mandatory values before conversion to dataclasses.
    missing = sorted(OmegaConf.missing_keys(cfg))

    if missing:
        formatted = ", ".join(missing)
        print_error("Missing mandatory argument(s)") 
        sys.exit(esub_abort_val)

    OmegaConf.resolve(cfg)
    config: Config = OmegaConf.to_object(cfg)

    device_present = config.device is not None
    qpu_present = config.qpu is not None

    if device_present == qpu_present:
        print_error(
                "Specify exactly one of device=<name> or ""qpu.<attribute>=<value>"
                )  

    # Validate QPU attributes
    if config.qpu is not None:
        validate_qpu_attributes(config.qpu)

    # Validate selectors
    allowed_selectors = {"basic", "health", "priority"}
    if config.selector not in allowed_selectors:
        print_error(
            f"Invalid selector {config.selector!r}; "
            f"expected one of {sorted(allowed_selectors)}"
        )

    print_debug(f"Selection policy: {config.selector}")
    print_debug(f"Credentials file: {config.file}")
    print_debug(f"Requested device: {config.device}")
    if config.qpu != None:
        print_debug(f"Qubits: {config.qpu.qubits}")
        print_debug(f"QPU version: {config.qpu.qpu_version}")
        print_debug(f"Processor type: {config.qpu.processor_type}")
        print_debug(f"CLOPS: {config.qpu.clops}")
        print_debug(f"Pending jobs: {config.qpu.pending_jobs}")
        print_debug(f"Readout error median: {config.qpu.readout_error_median}")
        print_debug(f"SX error median: {config.qpu.sx_error_median}")
        print_debug(f"CZ error median: {config.qpu.cz_error_median}")
        print_debug(f"T1 median: {config.qpu.T1_median_us} us")
        print_debug(f"T2 median: {config.qpu.T2_median_us} us")

    return config



# Functions

def read_config_file(cfile):
    """
    Read config file
    """
    env_file = os.getcwd() + "/" + cfile.name
    if not os.path.isfile(env_file):
        message = "File %s does not exist." % env_file
        print(message, file=sys.stderr)
        return None
    config = dotenv_values(env_file)
    return config

def gen_bearer_token(api_key):
    """
    Generate an IAM bearer token (valid for 3600 sec.)
    """
    url = "https://iam.cloud.ibm.com/identity/token"
    payload = {
        "grant_type": "urn:ibm:params:oauth:grant-type:apikey",
        "apikey": api_key
    }
    headers = {
        "Content-Type": "application/x-www-form-urlencoded",
        "Accept": "application/json"
    }
    try:
        response = requests.post(url, data=payload, headers=headers)
        response.raise_for_status()
        data = response.json()
        bearer_token = data["access_token"]
        return bearer_token
    except requests.exceptions.RequestException as e:
        print(f"An error occurred: {e}", file=sys.stderr)
        if hasattr(e, 'response') and e.response:
            print(f"Response status code: {e.response.status_code}", file=sys.stderr)
            print(f"Response content: {e.response.text}", file=sys.stderr)
        return None

def get_avail_devices(token, crn):
    """
    Get a list of available devices
    """
    url = "https://quantum.cloud.ibm.com/api/v1/backends"
    headers = {
        "Service-CRN": crn,
        "accept": "application/json",
        "Authorization": "Bearer " + token
    }
    try:
        response = requests.get(url, headers=headers)
        response.raise_for_status()
        devices = response.json()
        return devices
    except requests.exceptions.RequestException as e:
        print(f"An error occurred: {e}", file=sys.stderr)
        if hasattr(e, 'response') and e.response:
            print(f"Response status code: {e.response.status_code}", file=sys.stderr)
            print(f"Response content: {e.response.text}", file=sys.stderr)
        return None

def get_device_topology(token, devices, request_type):
    """
    Get status of available devices
    Returns a list of dictionaries with each list element corresponding to a device.
    """
    devices_topo = []
    for key, device in devices.items():
        for name in device:
            url = "https://quantum.cloud.ibm.com/api/v1/backends/" + name + "/" + request_type
            headers = {
                "Service-CRN": crn,
                "accept": "application/json",
                "Authorization": "Bearer " + token
            }
            try:
                response = requests.get(url, headers=headers)
                response.raise_for_status()
                topo = response.json()
                topo['device'] = name
                devices_topo.append(topo)
            except requests.exceptions.RequestException as e:
                print(f"An error occurred: {e}", file=sys.stderr)
                if hasattr(e, 'response') and e.response:
                    print(f"Response status code: {e.response.status_code}", file=sys.stderr)
                    print(f"Response content: {e.response.text}", file=sys.stderr)
                return None
    return devices_topo

def read_env_vars_from_config(config):
    """
    Read QRMI related variables from $CWD/envfile
    """
    api_key = config["QRMI_IBM_QCS_IAM_APIKEY"]
    if not api_key:
        print_error("No QRMI_IBM_QCS_IAM_APIKEY provided")

    token = gen_bearer_token(api_key)
    if not token:
        print_error("No IAM bearer token provided.")

    crn = config["QRMI_IBM_QCS_SERVICE_CRN"]
    if not crn:
        print_error("No QRMI_IBM_QCS_SERVICE_CRN provided")

    qrs_endpoint = config["QRMI_IBM_QCS_ENDPOINT"]
    if not qrs_endpoint:
        print_error("No QRMI_IBM_QCS_ENDPOINT provided")

    iam_endpoint = config["QRMI_IBM_QCS_IAM_ENDPOINT"]
    if not iam_endpoint:
        print_error("No QRMI_IBM_QCS_IAM_ENDPOINT provided")

    # Optional, so can be null
    mode = config["QRMI_IBM_QCS_SESSION_MODE"]

    return token, crn


def select_device_default(req_qubits, devices_status, devices_config):
    """
    Device selection based on the number of qubits and queue length.
    """

    best_device = None

    # Get 'length_queue' and 'message' for each device.
    tmp_stat = []
    for element in devices_status:
        pair = []
        for k, v in element.items():
            if k == 'device':
                pair.append(v)
            if k == 'length_queue':
                pair.append(v)
            if k == 'message':
                pair.append(v)
        tmp_stat.append(pair)

    tmp_conf = []
    for element in devices_config:
        pair = []
        for k, v in element.items():
            if k == 'device':
                pair.append(v)
            if k == 'n_qubits':
                pair.append(v)
        tmp_conf.append(pair)

    # Assuming that devices come in the same order merge tmp_stat and tmp_conf.
    for i in range(len(tmp_stat)):
        tmp_stat[i].append(tmp_conf[i][0])
    #print(tmp_stat)

    # Device selection algorithm:
    # Use the least busy device with enough qubits
    devices_sorted = sorted(tmp_stat, key = itemgetter(1))
    for device in devices_sorted:
        dev_qbits = device[3]
        status = device[0]
        if dev_qbits >= req_qubits and status == 'available':
            best_device = device[2]
            break

    #print(f"Selecting <{best_device}> as the least busy device with enough qbits.")

    return best_device



# ─────────────────────────────────────────────────────────────────────────────
# Weighted QPU health scoring
# Author: Randy Yang (IBM Spectrum Computing Intern, 2026)
#
# Compute a composite health score for each candidate device and
#                select the highest-scoring one.
#
# Score formula:
#   score = (T1_us / T1_NORM) * (1 / (readout_error + EPSILON)) * (1 / (queue + 1))
#
# Where:
#   T1_NORM  = 200.0  — normalise T1 so a 200µs coherence time contributes ~1.0
#   EPSILON  = 0.001  — floor to avoid division by zero when error = 0
#   queue+1          — +1 so a queue of 0 doesn't blow up the score
#
# Higher score = healthier QPU overall.
# All three factors matter: a QPU with great coherence but 500 queued jobs
# will lose to a slightly worse QPU with 5 jobs.
# ─────────────────────────────────────────────────────────────────────────────

T1_NORM = 200.0   # µs — reference coherence time for normalisation
EPSILON = 0.001   # floor for readout error to avoid division by zero


def compute_health_score(t1_us, readout_error, queue_depth):
    """
    Compute a composite QPU health score. Higher is better.

    Parameters
    ----------
    t1_us        : float  — T1 median coherence time in microseconds
    readout_error: float  — median readout error (0.0 – 1.0)
    queue_depth  : int    — number of jobs currently pending on this QPU

    Returns
    -------
    float — composite health score
    """
    t1_component    = t1_us / T1_NORM
    error_component = 1.0 / (readout_error + EPSILON)
    queue_component = 1.0 / (queue_depth + 1)
    return t1_component * error_component * queue_component


def select_device_health(req_qubits, devices_status, devices_config):
    """
    Device selection based on job requirements and devices availability
    and topology.

    Original: picked the least-busy device with enough qubits.
    Modified: picks the highest composite-health-score device with enough qubits,
              weighting T1 coherence, readout error rate, and queue depth together.
    """
    best_device = None

    # Build a lookup: device_name -> queue_depth, message (availability status)
    status_map = {}
    for element in devices_status:
        name   = element.get('device')
        queue  = element.get('length_queue', 999)
        status = element.get('message', 'unavailable')
        if name:
            status_map[name] = {'queue': queue, 'status': status}

    # Build a lookup: device_name -> n_qubits, T1, readout_error
    config_map = {}
    for element in devices_config:
        name          = element.get('device')
        n_qubits      = element.get('n_qubits', 0)
        t1_us         = element.get('T1_median_µs', 0.0)   # from ELIM metrics
        readout_error = element.get('readout_error_median', 1.0)
        if name:
            config_map[name] = {
                'n_qubits':      n_qubits,
                't1_us':         t1_us,
                'readout_error': readout_error,
            }

    # Score every candidate device and pick the best
    best_score = -1.0

    for name, cfg in config_map.items():
        st = status_map.get(name, {})

        # Skip unavailable or under-provisioned devices
        if st.get('status') != 'available':
            print_debug(f"Skipping {name}: status={st.get('status')}")
            continue
        if cfg['n_qubits'] < req_qubits:
            print_debug(f"Skipping {name}: only {cfg['n_qubits']} qubits < {req_qubits} required")
            continue

        score = compute_health_score(
            t1_us         = cfg['t1_us'],
            readout_error = cfg['readout_error'],
            queue_depth   = st.get('queue', 999),
        )

        print_debug(
            f"Device {name}: qubits={cfg['n_qubits']} "
            f"T1={cfg['t1_us']:.1f}µs "
            f"err={cfg['readout_error']:.4f} "
            f"queue={st.get('queue', '?')} "
            f"→ score={score:.4f}"
        )

        if score > best_score:
            best_score  = score
            best_device = name

    if best_device:
        print_debug(f"Selected {best_device} with health score {best_score:.4f}")
    else:
        print_debug("No suitable device found after health scoring")

    return best_device

#-------------------------------------------------------------------------
# To be implemented for mor complex topology decisions.
#devices_properties = get_device_topology(token, devices, "properties")

# Needs a different implementation
#devices_defaults = get_device_topology(token, devices, "defaults")
#-------------------------------------------------------------------------

def build_qrmi_vars_job(config, device):
    """
    Build QRMI environment variables with device for a job
    """
    for key, val in config.items():
        if 'QRMI' in key:
            tmp = device + '_' + key
            os.environ[tmp] = str(val)
            os.environ.pop(key)
    os.environ['QRMI_IBM_QCS_BEST_DEVICE'] = device

def build_qrmi_vars_lsf(config, device):
    """
    Build QRMI environment variables with device to pass on to LSF
    """
    lsf_var='LSF_SUB4_SUB_ENV_VARS="'
    for key, val in config.items():
        lsf_var = lsf_var + device + '_' + key + '=' + val + ','
    if debug:
        lsf_var = lsf_var + 'LSF_QRMI_DEBUG=' + debug + ','
    # Add IBM_QCS_BEST_DEVICE variable
    lsf_var = lsf_var + 'QRMI_IBM_QCS_BEST_DEVICE=' + device + '"'

    print_debug("QRMI variables to write:", lsf_var)

    mod_file = os.environ.get('LSB_SUB_MODIFY_FILE')
    if not mod_file:
        print_error("Cannot get LSB_SUB_MODIFY_FILE variable")
    try:
        with open(mod_file, "a") as esub_file:
            esub_file.write(lsf_var)
    except IOError:
        print_error("Cannot write to the LSB_SUB_MODIFY_FILE.")

def transfer_vars_lsf(config, requests):
    """
    Transfer QRMI creds and user requests
    """
    lsf_var='LSF_SUB4_SUB_ENV_VARS="'
    # Configs from the environment file
    for key, val in config.items():
        lsf_var = lsf_var + key + '=' + val + ','
    # User requests
    for key, val in requests.items():
        if key == 'file':
            continue
        lsf_var = lsf_var + 'ESUB_USER_REQ_' + key.upper() + '=' + str(val) + ','
    if debug:
        lsf_var = lsf_var + 'LSF_QRMI_DEBUG=' + debug + ','
    lsf_var = lsf_var + '"'
    print_debug("lsf_var: ", lsf_var)

    mod_file = os.environ.get('LSB_SUB_MODIFY_FILE')
    if not mod_file:
        print_error("Cannot get LSB_SUB_MODIFY_FILE variable")
    try:
        with open(mod_file, "a") as esub_file:
            esub_file.write(lsf_var)
    except IOError:
        print_error("Cannot write to the LSB_SUB_MODIFY_FILE.")

def read_config_from_env():
    """
    Build QRMI environment variables for jobs. 
    Note that "QCS" variables are for QPUs on IBM Quantum Platform.
    See QRMI documentation for other platforms.
    """
    config = {}

    api_key = os.getenv("QRMI_IBM_QCS_IAM_APIKEY")
    if not api_key:
        print_error("No QRMI_IBM_QCS_IAM_APIKEY provided")
    config.update({"QRMI_IBM_QCS_IAM_APIKEY": api_key})

    crn = os.getenv("QRMI_IBM_QCS_SERVICE_CRN")
    if not crn:
        print_error("No QRMI_IBM_QCS_SERVICE_CRN provided")
    config.update({"QRMI_IBM_QCS_SERVICE_CRN": crn})

    qrs_endpoint = os.getenv("QRMI_IBM_QCS_ENDPOINT")
    if not qrs_endpoint:
        print_error("No QRMI_IBM_QCS_ENDPOINT provided")
    config.update({"QRMI_IBM_QCS_ENDPOINT": qrs_endpoint})

    iam_endpoint = os.getenv("QRMI_IBM_QCS_IAM_ENDPOINT")
    if not iam_endpoint:
        print_error("No QRMI_IBM_QCS_IAM_ENDPOINT provided")
    config.update({"QRMI_IBM_QCS_IAM_ENDPOINT": iam_endpoint})

    # Optional, so can be null
    mode = os.getenv("QRMI_IBM_QCS_SESSION_MODE")
    if mode:
        config.update({"QRMI_IBM_QCS_SESSION_MODE": mode})

    #user_qubits = os.getenv("ESUB_USER_REQ_QUBITS")
    #if not user_qubits:
    #    print_error("No ESUB_USER_REQ_QUBITS provided")
    #config.update({"ESUB_USER_REQ_QUBITS": user_qubits})

    device_selector = os.getenv("ESUB_USER_REQ_SELECTOR")
    config.update({"ESUB_USER_REQ_SELECTOR": device_selector})

    req_device = os.getenv("ESUB_USER_REQ_DEVICE")
    config.update({"ESUB_USER_REQ_DEVICE": req_device})

    return config

#-----------------------------------------------------
# Main starts here
#-----------------------------------------------------

debug = os.getenv('LSF_QRMI_DEBUG')
if debug != 'level1' and debug != 'level2':
    debug = None

# Check who I am
identity = os.path.basename(sys.argv[0]).removesuffix('.qrmi')
if identity != 'esub' and identity != 'jobstarter':
    print_error("Unknown identity")

esub_abort_val = os.environ.get("LSB_SUB_ABORT_VALUE")

# Do what esub is supposed to do.
if identity == "esub":

    # Parse the command line
    config = parse_config()

    creds = read_config_file(config.file)
    if not creds:
        print_error("Cannot read credentials file {credentials_file.name}")

    transfer_vars_lsf(creds, vars(config))
    print_debug("Passed creds and requests to a jobstarter.")

    exit(0)
else:
    # Arguments to pass on to the actual job
    job_args = sys.argv[1:]

#=============================================================
# Acting as a jobstarter from here onwards
#=============================================================

# Build QRMI environment vars for LSF
config = read_config_from_env()

#print(config, file=sys.stderr)

device=config["ESUB_USER_REQ_DEVICE"]
if device != None:
    # Set {device}_QRMI variables for a job
    build_qrmi_vars_job(config, device)
    # Launch the job
    subprocess.run(job_args)
    sys.exit(0)

# Get QRMI environment variables templates
token, crn = read_env_vars_from_config(config)
print_debug("Obtained authentication token and CRN")

# Get available devices.
devices = get_avail_devices(token, crn)
if not devices:
    print_error("No quantum devices found.")
print_debug("Devices: ", devices['devices'])

# Get status of each available device.
devices_status = get_device_topology(token, devices, "status")
if not devices_status:
    print_error("No status of quantum devices.")
print_debug("Status: ", devices_status)

# Get number of qubits for each device.
devices_config = get_device_topology(token, devices, "configuration")
if not devices_config:
    print_error("No configuration of quantum devices.")
if debug == 'level2':
    print_debug("Configuration: ", devices_config)


# Select best device for a job 
qubits = int(config['ESUB_USER_REQ_QUBITS'])
policy = config['ESUB_USER_REQ_SELECTOR']
print_debug("Device selection policy:", policy)

selectors = {
    "basic": select_device_default,
    "health": select_device_health,
}

try:
    select_device = selectors[policy]
except KeyError:
    print_error(f"Unknown policy: {policy}")
    raise SystemExit(2)


best_device = select_device(
    qubits,
    devices_status,
    devices_config,
)

if not best_device:
    print_error("No suitable quantum device available.")
print_debug("Best device: ", best_device)

# Set {device}_QRMI variables for a job
build_qrmi_vars_job(config, best_device)

# Launch the job
subprocess.run(job_args)
exit(0)
