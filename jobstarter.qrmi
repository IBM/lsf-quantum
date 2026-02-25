#!/usr/bin/env python3  

# -*- coding: utf-8 -*-

# (C) Copyright 2025 IBM. All Rights Reserved.
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
import argparse
import requests
import subprocess
from dotenv import dotenv_values
from operator import itemgetter

# Functions 
def print_debug(message, var=None):
    """
    Print message to stderr
    stdout is not availablke to esub
    """
    if debug:
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
    message = "[ERROR] " + message
    print(message, file=sys.stderr)
    exit(1)

def read_config_file(cfile):
    """
    Read config file
    """
    env_file = os.getcwd() + "/" + cfile.name
    if not os.path.isfile(env_file):
        message = "File %s does not exist." % env_file 
        print(message, file=sys.stderr)
        return None
    config =  dotenv_values(env_file) 
    return config

def gen_bearer_token(api_key):
    """
    Generate aan IAM bearer token (valid for 3600 sec.)
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
        response.raise_for_status()  # Raise HTTPError for bad responses (4xx or 5xx)
        data = response.json()
        bearer_token = data["access_token"]
        #print(f"Bearer token: {bearer_token}")
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
        response.raise_for_status()  # Raise HTTPError for bad responses (4xx or 5xx)
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
                    response.raise_for_status()  # Raise HTTPError for bad responses (4xx or 5xx)
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

    api_key = config["QRMI_IBM_QRS_IAM_APIKEY"]
    if not api_key:
        print_error("No QRMI_IBM_QRS_IAM_APIKEY provided")     

    token = gen_bearer_token(api_key)
    if not token:
       print_error("No IAM bearer token provided.")

    crn = config["QRMI_IBM_QRS_SERVICE_CRN"]
    if not crn:
       print_error("No QRMI_IBM_QRS_SERVICE_CRN provided")     

    qrs_endpoint = config["QRMI_IBM_QRS_ENDPOINT"] 
    if not qrs_endpoint:
       print_error("No QRMI_IBM_QRS_ENDPOINT provided")     

    iam_endpoint = config["QRMI_IBM_QRS_IAM_ENDPOINT"] 
    if not iam_endpoint:
       print_error("No QRMI_IBM_QRS_IAM_ENDPOINT provided")     
     
    # Optional, so can be null 
    mode = config["QRMI_IBM_QRS_SESSION_MODE"] 

    return token, crn 

def select_device(req_qubits, devices_status, devices_config):
    """
    Device selection based on job requirements and
    devices availability and topology.
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

#-------------------------------------------------------------------------
# To be implemented for mor complex topology decisions.
#devices_properties = get_device_topology(token, devices, "properties")

# Needs a different implementation
#devices_defaults = get_device_topology(token, devices, "defaults")
#-------------------------------------------------------------------------

def build_qrmi_vars_job(config, device):
    """
    Build QRMI environment varibales with device for a job
    """
    for key, val in config.items():
        if 'QRMI' in key: 
          tmp = device + '_' + key 
          os.environ[tmp] = str(val)
          os.environ.pop(key)
    os.environ['QRMI_IBM_QRS_BEST_DEVICE'] = device

def build_qrmi_vars_lsf(config, device):
    """
    Build QRMI environment variables with device to pass on to LSF
    """
    lsf_var='LSF_SUB4_SUB_ENV_VARS="'
    for key, val in config.items():
        lsf_var = lsf_var + device + '_' + key + '=' + val + ','
    if debug:
        lsf_var = lsf_var + 'LSF_QRMI_DEBUG=' + debug + ','
    # Add IBM_QRS_BEST_DEVICE variable    
    lsf_var = lsf_var + 'QRMI_IBM_QRS_BEST_DEVICE=' + device + '"'    
    # Add debugging if set
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
    Build QRMI environment variables for jobs
    """
    config = {}
    api_key = os.getenv("QRMI_IBM_QRS_IAM_APIKEY")
    if not api_key:
        print_error("No QRMI_IBM_QRS_IAM_APIKEY provided")
    config.update({"QRMI_IBM_QRS_IAM_APIKEY":api_key})

    crn = os.getenv("QRMI_IBM_QRS_SERVICE_CRN")
    if not crn:
       print_error("No QRMI_IBM_QRS_SERVICE_CRN provided")
    config.update({"QRMI_IBM_QRS_SERVICE_CRN":crn})

    qrs_endpoint = os.getenv("QRMI_IBM_QRS_ENDPOINT")
    if not qrs_endpoint:
       print_error("No QRMI_IBM_QRS_ENDPOINT provided")
    config.update({"QRMI_IBM_QRS_ENDPOINT":qrs_endpoint})

    iam_endpoint = os.getenv("QRMI_IBM_QRS_IAM_ENDPOINT")
    if not iam_endpoint:
       print_error("No QRMI_IBM_QRS_IAM_ENDPOINT provided")
    config.update({"QRMI_IBM_QRS_IAM_ENDPOINT":iam_endpoint})

    # Optional, so can be null
    mode = os.getenv("QRMI_IBM_QRS_SESSION_MODE")
    if mode:
        config.update({"QRMI_IBM_QRS_SESSION_MODE":mode})

    user_qubits = os.getenv("ESUB_USER_REQ_QUBITS")
    if not user_qubits:
       print_error("No ESUB_USER_REQ_QUBITS provided")
    config.update({"ESUB_USER_REQ_QUBITS":user_qubits})

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

# Parse the command line
if identity == "esub":
    parser = argparse.ArgumentParser(
        description="esub.qrmi for IBM Spectrum LSF",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
        Number of qubits is ignored when quantum device name is provided, 
examples: 
        bsub -a "qrmi(.env, 128)" my_quantum_app
        bsub -a "qrmi(.env, 128, ibm_blue)" my_quantum_app

notes: export LSF_QRMI_DEBUG=level1 enables debugging messages
       export LSF_QRMI_DEBUG=level2 enables level1 and more 
"""
    )
    parser.add_argument("file", type=argparse.FileType('r'), help="File with user REST API creds")
    parser.add_argument("qubits", type=int, help="Number of qubits")
    parser.add_argument("device", type=str, nargs='?', help="Quantum device")
    args = parser.parse_args()
else:    
    # Arguments to pass on to the actual job 
    job_args = sys.argv[1:]

# Do what esub is supposed to do.
if identity == "esub":
    # Read config file
    config = read_config_file(args.file)
    if not config:
        print_error("Cannot read configuration file {args.file.name}")

    # If device name is provided by a user, just us it verbatim.
    if args.device:
        # Build QRMI environment vars for LSF
        build_qrmi_vars_lsf(config, args.device)
        print_debug("Created QRMI variables using device", args.device)
    else:
        # Pass creds and resources request to a jobstarter
        transfer_vars_lsf(config, vars(args))
        print_debug("Passed creds and requests on")
    exit(0)

#=============================================================
# Acting as a jobstarter from here onwards
#=============================================================

# Build QRMI environment vars for LSF
config = read_config_from_env()

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
best_device = select_device(qubits, devices_status, devices_config)
if not best_device:
   print_error("No suitable quantum device available.")
print_debug("Best device: ", best_device)

# Set {device}_QRMI varibales for a job
build_qrmi_vars_job(config, best_device)

# Launch the job
subprocess.run(job_args) 

exit(0)
