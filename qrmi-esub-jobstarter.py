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

from __future__ import annotations
import sys
import os
import time
import json
import requests
import subprocess
from dotenv import dotenv_values
from operator import itemgetter
from dataclasses import dataclass, fields
from pathlib import Path
from omegaconf import MISSING, OmegaConf
from statistics import median
from typing import Any
from pprint import pprint
import math
import operator
import re
from typing import Any, Callable, Mapping, Optional


# Helpers for printing errors and debug messages
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



# Priority-based QPU selection algorithm

# Metrics where a larger value is better.
MAXIMIZE_METRICS = {
    "qubits",
    "clops",
    "T1_median_us",
    "T2_median_us",
}

# Metrics where a smaller value is better.
MINIMIZE_METRICS = {
    "pending_jobs",
    "readout_error_median",
    "sx_error_median",
    "cz_error_median",
}


@dataclass(frozen=True)
class Requirement:
    attribute: str
    comparison: str
    value: Any
    priority: int


REQUIREMENT_PATTERN = re.compile(
    r"""
    ^\s*
    (?P<attribute>[A-Za-z_][A-Za-z0-9_.]*)
    \s*
    (?P<operator>>=|<=|==|!=|>|<|=)
    \s*
    (?P<value>.+?)
    \s*$
    """,
    re.VERBOSE,
)


COMPARISON_OPERATORS: dict[
    str,
    Callable[[Any, Any], bool],
] = {
    "==": operator.eq,
    "!=": operator.ne,
    ">": operator.gt,
    ">=": operator.ge,
    "<": operator.lt,
    "<=": operator.le,
}


def _parse_requirement_value(value: str) -> Any:
    """Parse a requirement value into an appropriate Python type."""
    value = value.strip()

    if (
        len(value) >= 2
        and value[0] == value[-1]
        and value[0] in {"'", '"'}
    ):
        return value[1:-1]

    lowered = value.lower()

    if lowered in {"none", "null"}:
        return None

    if lowered == "true":
        return True

    if lowered == "false":
        return False

    try:
        return int(value)
    except ValueError:
        pass

    try:
        return float(value)
    except ValueError:
        return value


def _metric_name(attribute: str) -> str:
    """
    Return the top-level metric name.

    Example:
        processor_type.family -> processor_type
    """
    return attribute.split(".", maxsplit=1)[0]


def _comparison_for_equals(
    attribute: str,
    value: Any,
) -> str:
    """
    Interpret a single '=' according to the metric direction.

    Examples:
        qubits=156 means qubits >= 156
        clops=100 means clops >= 100
        pending_jobs=10 means pending_jobs <= 10
        processor_type.family=Heron means exact equality
    """
    metric = _metric_name(attribute)

    is_numeric = (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
    )

    if is_numeric and metric in MAXIMIZE_METRICS:
        return ">="

    if is_numeric and metric in MINIMIZE_METRICS:
        return "<="

    return "=="


def _parse_requirements(
    requirements: str,
    ) -> list:
    """Parse semicolon-separated requirements in priority order."""
    parsed: list[Requirement] = []

    for priority, expression in enumerate(
        requirements.split(";")
    ):
        expression = expression.strip()

        if not expression:
            continue

        match = REQUIREMENT_PATTERN.fullmatch(expression)

        if match is None:
            raise ValueError(
                f"Invalid requirement: {expression!r}. "
                "Expected forms such as 'qubits=156', "
                "'clops>=100', or "
                "'processor_type.family=Heron'."
            )

        attribute = match.group("attribute")
        comparison = match.group("operator")
        value = _parse_requirement_value(
            match.group("value")
        )

        if comparison == "=":
            comparison = _comparison_for_equals(
                attribute,
                value,
            )

        parsed.append(
            Requirement(
                attribute=attribute,
                comparison=comparison,
                value=value,
                priority=priority,
            )
        )

    if not parsed:
        raise ValueError(
            "The requirements string contains no requirements"
        )

    return parsed


def _get_metric_value(
    metrics: Mapping[str, Any],
    attribute: str,
) -> Any:
    """
    Retrieve a value using a dotted attribute path.

    Examples:
        qubits
        processor_type.family
        processor_type.revision
    """
    value: Any = metrics

    for part in attribute.split("."):
        if not isinstance(value, Mapping):
            return None

        if part not in value:
            return None

        value = value[part]

    if (
        isinstance(value, str)
        and value.strip().lower()
        in {"", "none", "null", "n/a", "unavailable"}
    ):
        return None

    return value


def _is_numeric(value: Any) -> bool:
    """Return True if value is a finite numeric value."""
    if isinstance(value, bool):
        return False

    if not isinstance(value, (int, float)):
        return False

    return math.isfinite(float(value))


def _satisfies_requirement(
    actual_value: Any,
    requirement: Requirement,
) -> bool:
    """Return whether a metric satisfies a requirement."""
    required_value = requirement.value

    if actual_value is None or required_value is None:
        if requirement.comparison == "==":
            return actual_value is required_value

        if requirement.comparison == "!=":
            return actual_value is not required_value

        return False

    if (
        _is_numeric(actual_value)
        and _is_numeric(required_value)
    ):
        actual_value = float(actual_value)
        required_value = float(required_value)

    elif (
        isinstance(actual_value, str)
        and isinstance(required_value, str)
    ):
        actual_value = actual_value.casefold()
        required_value = required_value.casefold()

    comparison_function = COMPARISON_OPERATORS[
        requirement.comparison
    ]

    try:
        return bool(
            comparison_function(
                actual_value,
                required_value,
            )
        )
    except TypeError:
        return False


def _keep_best_candidates(
    candidates: dict[str, dict[str, Any]],
    requirement: Requirement,
) -> dict[str, dict[str, Any]]:
    """
    Sort candidates by the current requested attribute and keep
    all candidates tied at the best value.
    """
    metric = _metric_name(requirement.attribute)

    if metric in MAXIMIZE_METRICS:
        maximize = True
    elif metric in MINIMIZE_METRICS:
        maximize = False
    else:
        # Categorical attributes do not have a ranking.
        # Keep all matching candidates for the next requirement.
        return candidates

    candidate_values: dict[str, float] = {}

    for device_name, metrics in candidates.items():
        value = _get_metric_value(
            metrics,
            requirement.attribute,
        )

        if _is_numeric(value):
            candidate_values[device_name] = float(value)

    if not candidate_values:
        return {}

    if maximize:
        best_value = max(candidate_values.values())
    else:
        best_value = min(candidate_values.values())

    return {
        device_name: candidates[device_name]
        for device_name, value in candidate_values.items()
        if math.isclose(
            value,
            best_value,
            rel_tol=1e-12,
            abs_tol=1e-15,
        )
    }


def select_device_priority(
    backend_metrics: Mapping[
        str,
        Mapping[str, Any],
    ],
    requirements: str,
) -> tuple[str, dict[str, Any]] | None:
    """
    Select a backend according to prioritized user requirements.

    backend_metrics must have the structure returned by
    get_all_backend_metrics_rest():

        {
            "ibm_backend_name": {
                "qubits": 156,
                "clops": 100.0,
                "pending_jobs": 3,
                ...
            },
            ...
        }

    Requirements are processed in appearance order:

        "qubits=156;clops=100.0;pending_jobs=10"

    Single '=' semantics:

        qubits=156
            means qubits >= 156

        clops=100.0
            means clops >= 100.0

        pending_jobs=10
            means pending_jobs <= 10

        readout_error_median=0.02
            means readout_error_median <= 0.02

        processor_type.family=Heron
            means exact equality

    Use '==' for exact numeric equality:

        "qubits==156"

    Algorithm:

        1. Start with every available backend.
        2. Process requirements in their string order.
        3. Remove backends that do not satisfy the current requirement.
        4. Among satisfying backends, retain those tied at the best value.
        5. Continue with the next requirement.
        6. If multiple backends remain, select by backend name.

    Returns:

        (backend_name, backend_metrics)

    Returns None when no backend satisfies the requirements.
    """
    parsed_requirements = _parse_requirements(
        requirements
    )
    print(f"Parsed requirements: {parsed_requirements}", file=sys.stderr)

    candidates: dict[str, dict[str, Any]] = {
        str(backend_name): dict(metrics)
        for backend_name, metrics
        in backend_metrics.items()
        if isinstance(metrics, Mapping)
        and "error" not in metrics
    }

    if not candidates:
        return None

    for requirement in parsed_requirements:
        satisfying_candidates: dict[
            str,
            dict[str, Any],
        ] = {}

        for backend_name, metrics in candidates.items():
            actual_value = _get_metric_value(
                metrics,
                requirement.attribute,
            )

            if _satisfies_requirement(
                actual_value,
                requirement,
            ):
                satisfying_candidates[backend_name] = metrics

        if not satisfying_candidates:
            return None

        candidates = _keep_best_candidates(
            satisfying_candidates,
            requirement,
        )

        if not candidates:
            return None

        if len(candidates) == 1:
            break

    # Deterministic final tie-break.
    selected_backend_name = min(candidates)

    return (
        selected_backend_name,
        candidates[selected_backend_name],
    )

# QPU metrics extraction
NULL_STRINGS = {
    "",
    "none",
    "null",
    "n/a",
    "unavailable",
}


def normalize_value(value: Any) -> Any | None:
    """Convert textual null values to Python None."""
    if value is None:
        return None

    if (
        isinstance(value, str)
        and value.strip().lower() in NULL_STRINGS
    ):
        return None

    return value


def safe_median(values: list[float]) -> float | None:
    """Return the median, or None if the list is empty."""
    if not values:
        return None

    return float(median(values))


def extract_clops(backend: Any) -> int | float | str | None:
    """
    Extract CLOPS from the backend.

    Checks:
      1. backend.configuration().clops
      2. backend.configuration().clops_h
      3. backend.configuration().clops_v
      4. backend.clops
    """
    try:
        configuration = backend.configuration()
    except Exception:
        configuration = None

    candidates: list[Any] = []

    if configuration is not None:
        candidates.extend(
            [
                getattr(configuration, "clops", None),
                getattr(configuration, "clops_h", None),
                getattr(configuration, "clops_v", None),
            ]
        )

    candidates.append(getattr(backend, "clops", None))

    for candidate in candidates:
        candidate = normalize_value(candidate)

        if candidate is None:
            continue

        # Example:
        # {"type": "hardware", "value": 12345}
        if isinstance(candidate, dict):
            candidate = normalize_value(
                candidate.get("value")
            )

        # Handle an object with a .value attribute.
        elif hasattr(candidate, "value"):
            candidate = normalize_value(
                getattr(candidate, "value")
            )

        if candidate is not None:
            return candidate

    return None


def extract_gate_errors(
    properties: Any,
    gate_name: str,
    ) -> list:
    """
    Return errors for every calibrated instance of gate_name.

    For example:
      - sx on individual qubits
      - cz on connected qubit pairs
    """
    errors: list[float] = []

    if properties is None:
        return errors

    for gate in getattr(properties, "gates", []):
        calibrated_gate_name = getattr(gate, "gate", None)

        if calibrated_gate_name != gate_name:
            continue

        qubits = getattr(gate, "qubits", None)

        if qubits is None:
            continue

        try:
            value = properties.gate_error(
                gate_name,
                tuple(qubits),
            )
        except Exception:
            continue

        value = normalize_value(value)

        if value is not None:
            errors.append(float(value))

    return errors


def get_backend_metrics(
    backend: Any,
) -> dict[str, Any]:
    """
    Collect IBM Qiskit backend metrics.

    Returned keys:
      qubits
      qpu_version
      processor_type
      clops
      pending_jobs
      readout_error_median
      sx_error_median
      cz_error_median
      T1_median_us
      T2_median_us

    CZ handling:
      If CZ is not a calibrated native gate, cz_error_median
      is returned as Python None.

    CLOPS handling:
      Supports dictionary, object, scalar, clops_h, and
      clops_v representations.
    """
    num_qubits = normalize_value(
        getattr(backend, "num_qubits", None)
    )

    qpu_version = normalize_value(
        getattr(backend, "backend_version", None)
    )

    processor_type = normalize_value(
        getattr(backend, "processor_type", None)
    )

    clops = extract_clops(backend)

    try:
        status = backend.status()
        pending_jobs = normalize_value(
            getattr(status, "pending_jobs", None)
        )
    except Exception:
        pending_jobs = None

    try:
        properties = backend.properties()
    except Exception:
        properties = None

    readout_errors: list[float] = []
    t1_values_us: list[float] = []
    t2_values_us: list[float] = []

    if properties is not None and num_qubits is not None:
        for qubit in range(int(num_qubits)):
            try:
                value = normalize_value(
                    properties.readout_error(qubit)
                )

                if value is not None:
                    readout_errors.append(float(value))

            except Exception:
                pass

            try:
                value = normalize_value(
                    properties.t1(qubit)
                )

                if value is not None:
                    # Qiskit returns T1 in seconds.
                    t1_values_us.append(
                        float(value) * 1_000_000
                    )

            except Exception:
                pass

            try:
                value = normalize_value(
                    properties.t2(qubit)
                )

                if value is not None:
                    # Qiskit returns T2 in seconds.
                    t2_values_us.append(
                        float(value) * 1_000_000
                    )

            except Exception:
                pass

    sx_errors = extract_gate_errors(
        properties,
        "sx",
    )

    # If CZ is not present in backend calibration properties,
    # this produces an empty list and the median becomes None.
    cz_errors = extract_gate_errors(
        properties,
        "cz",
    )

    return {
        "qubits": num_qubits,
        "qpu_version": qpu_version,
        "processor_type": processor_type,
        "clops": clops,
        "pending_jobs": pending_jobs,
        "readout_error_median": safe_median(
            readout_errors
        ),
        "sx_error_median": safe_median(
            sx_errors
        ),
        "cz_error_median": safe_median(
            cz_errors
        ),
        "T1_median_us": safe_median(
            t1_values_us
        ),
        "T2_median_us": safe_median(
            t2_values_us
        ),
    }

# Arguments parser

@dataclass
class QPUAttributes:
    """Attributes used to describe and select quantum processing units."""

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

    def to_lsf_string(self) -> str:
        """Convert populated QPU attributes to an LSF-compatible string."""
        return ";".join(
            f"{item.name}={getattr(self, item.name)}"
            for item in fields(self)
            if getattr(self, item.name) is not None
        )


@dataclass
class Config:
    """Application command-line configuration."""

    # File containing user REST API credentials
    file: Path = MISSING

    # QPU selection
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

    print_debug(f"Selector : {config.selector}")
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
    if debug == 'level2':        
        print_debug("Devices topology:, devices_topo")        
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


NULL_STRINGS = {
    "",
    "none",
    "null",
    "n/a",
    "unavailable",
}


def normalize_value(value: Any) -> Any | None:
    """Convert textual null values to Python None."""
    if value is None:
        return None

    if (
        isinstance(value, str)
        and value.strip().lower() in NULL_STRINGS
    ):
        return None

    return value


def safe_median(values: list[float]) -> float | None:
    """Return the median, or None when no values are available."""
    return float(median(values)) if values else None


def get_json(
    session: requests.Session,
    url: str,
    timeout: float = 30.0,
) -> dict[str, Any]:
    """Perform a REST GET request and return its JSON object."""
    response = session.get(url, timeout=timeout)
    response.raise_for_status()

    data = response.json()

    if not isinstance(data, dict):
        raise TypeError(
            f"Expected a JSON object from {url}, "
            f"received {type(data).__name__}"
        )

    return data


def extract_clops(device: dict[str, Any]) -> int | float | str | None:
    """
    Extract CLOPS from a backend-list entry.

    The normal REST representation is:

        {
            "clops": {
                "type": "hardware",
                "value": 12345
            }
        }

    Scalar CLOPS values are also accepted.
    """
    clops = normalize_value(device.get("clops"))

    if isinstance(clops, dict):
        return normalize_value(clops.get("value"))

    return clops


def extract_parameter_value(
    parameters: list[dict[str, Any]],
    names: set[str],
) -> float | None:
    """
    Extract a numeric value from a list of REST calibration parameters.

    Parameter matching is case-insensitive.
    """
    normalized_names = {
        name.lower()
        for name in names
    }

    for parameter in parameters:
        name = str(
            parameter.get("name", "")
        ).strip().lower()

        if name not in normalized_names:
            continue

        value = normalize_value(
            parameter.get("value")
        )

        if value is None:
            continue

        try:
            return float(value)
        except (TypeError, ValueError):
            continue

    return None


def convert_time_to_us(
    value: float,
    unit: str | None,
) -> float | None:
    """
    Convert a calibration time value to microseconds.

    Known units:
      s, ms, us, µs, μs, ns

    IBM backend properties normally provide a unit alongside the value.
    Unknown units return None rather than assuming a conversion.
    """
    if unit is None:
        return None

    normalized_unit = (
        unit.strip()
        .lower()
        .replace("μ", "u")
        .replace("µ", "u")
    )

    conversion_to_us = {
        "s": 1_000_000.0,
        "ms": 1_000.0,
        "us": 1.0,
        "ns": 0.001,
    }

    multiplier = conversion_to_us.get(
        normalized_unit
    )

    if multiplier is None:
        return None

    return value * multiplier


def extract_qubit_metrics(
    properties: dict[str, Any],
) -> tuple[
    list[float],
    list[float],
    list[float],
]:
    """
    Extract readout error, T1, and T2 values from REST properties.

    The REST properties schema represents qubits as:

        "qubits": [
            [
                {
                    "name": "T1",
                    "value": ...,
                    "unit": "us"
                },
                ...
            ],
            ...
        ]
    """
    readout_errors: list[float] = []
    t1_values_us: list[float] = []
    t2_values_us: list[float] = []

    for qubit_parameters in properties.get(
        "qubits",
        [],
    ):
        if not isinstance(qubit_parameters, list):
            continue

        readout_error = extract_parameter_value(
            qubit_parameters,
            {
                "readout_error",
                "readout error",
            },
        )

        if readout_error is not None:
            readout_errors.append(readout_error)

        for parameter in qubit_parameters:
            name = str(
                parameter.get("name", "")
            ).strip().lower()

            if name not in {"t1", "t2"}:
                continue

            value = normalize_value(
                parameter.get("value")
            )
            unit = normalize_value(
                parameter.get("unit")
            )

            if value is None:
                continue

            try:
                numeric_value = float(value)
            except (TypeError, ValueError):
                continue

            value_us = convert_time_to_us(
                numeric_value,
                str(unit) if unit is not None else None,
            )

            if value_us is None:
                continue

            if name == "t1":
                t1_values_us.append(value_us)
            else:
                t2_values_us.append(value_us)

    return (
        readout_errors,
        t1_values_us,
        t2_values_us,
    )


def extract_gate_errors(
    properties: dict[str, Any],
    gate_name: str,
    ) -> list:
    """
    Extract all calibrated errors for a specific gate.

    For example:
      - gate_name="sx" collects all calibrated SX errors.
      - gate_name="cz" collects all calibrated CZ errors.

    If the backend has no CZ gate, an empty list is returned.
    """
    errors: list[float] = []

    for gate in properties.get("gates", []):
        if not isinstance(gate, dict):
            continue

        current_gate_name = normalize_value(
            gate.get("gate")
        )

        # Some responses may use "name" for the gate name.
        if current_gate_name is None:
            current_gate_name = normalize_value(
                gate.get("name")
            )

        if (
            str(current_gate_name).lower()
            != gate_name.lower()
        ):
            continue

        parameters = gate.get("parameters", [])

        if not isinstance(parameters, list):
            continue

        gate_error = extract_parameter_value(
            parameters,
            {
                "gate_error",
                "gate error",
            },
        )

        if gate_error is not None:
            errors.append(gate_error)

    return errors


def extract_rest_backend_metrics(
    device: dict[str, Any],
    configuration: dict[str, Any],
    properties: dict[str, Any],
) -> dict[str, Any]:
    """
    Construct metrics for one REST backend.
    """
    readout_errors, t1_values_us, t2_values_us = (
        extract_qubit_metrics(properties)
    )

    sx_errors = extract_gate_errors(
        properties,
        "sx",
    )

    # Remains empty if CZ is not a calibrated gate.
    cz_errors = extract_gate_errors(
        properties,
        "cz",
    )

    performance_metrics = device.get(
        "performance_metrics",
        {},
    )

    if not isinstance(performance_metrics, dict):
        performance_metrics = {}

    # Prefer calculation from raw calibration data.
    readout_error_median = safe_median(
        readout_errors
    )

    # Fall back to the aggregate from the list endpoint.
    if readout_error_median is None:
        aggregate = performance_metrics.get(
            "readout_error_median"
        )

        if isinstance(aggregate, dict):
            aggregate = aggregate.get("value")

        aggregate = normalize_value(aggregate)

        if aggregate is not None:
            try:
                readout_error_median = float(
                    aggregate
                )
            except (TypeError, ValueError):
                pass

    qubits = normalize_value(
        device.get("qubits")
    )

    if qubits is None:
        qubits = normalize_value(
            configuration.get("n_qubits")
        )

    qpu_version = normalize_value(
        configuration.get("backend_version")
    )

    processor_type = normalize_value(
        device.get("processor_type")
    )

    if processor_type is None:
        processor_type = normalize_value(
            configuration.get("processor_type")
        )

    return {
        "qubits": qubits,
        "qpu_version": qpu_version,
        "processor_type": processor_type,
        "clops": extract_clops(device),
        "pending_jobs": normalize_value(
            device.get("queue_length")
        ),
        "readout_error_median": (
            readout_error_median
        ),
        "sx_error_median": safe_median(
            sx_errors
        ),
        "cz_error_median": safe_median(
            cz_errors
        ),
        "T1_median_us": safe_median(
            t1_values_us
        ),
        "T2_median_us": safe_median(
            t2_values_us
        ),
    }


def get_all_backend_metrics_rest(
    access_token: str,
    service_crn: str,
    *,
    base_url: str = (
        "https://quantum.cloud.ibm.com/api"
    ),
    api_version: str = "2026-04-15",
    timeout: float = 30.0,
) -> dict[str, dict[str, Any]]:
    """
    Retrieve all accessible IBM Quantum backends via REST and
    return a nested metrics dictionary.

    Returns:

        {
            "backend_name": {
                "qubits": ...,
                "qpu_version": ...,
                "processor_type": ...,
                "clops": ...,
                "pending_jobs": ...,
                "readout_error_median": ...,
                "sx_error_median": ...,
                "cz_error_median": ...,
                "T1_median_us": ...,
                "T2_median_us": ...
            }
        }
    """
    session = requests.Session()
    session.headers.update(
        {
            "Accept": "application/json",
            "Authorization": (
                f"Bearer {access_token}"
            ),
            "Service-CRN": service_crn,
            "IBM-API-Version": api_version,
        }
    )

    list_url = f"{base_url.rstrip('/')}/v1/backends"

    backend_response = get_json(
        session,
        list_url,
        timeout,
    )

    devices = backend_response.get("devices", [])

    if not isinstance(devices, list):
        raise TypeError(
            "REST response field 'devices' "
            "is not a list"
        )

    result: dict[str, dict[str, Any]] = {}

    for device in devices:
        if not isinstance(device, dict):
            continue

        backend_name = normalize_value(
            device.get("name")
        )

        if backend_name is None:
            continue

        backend_name = str(backend_name)

        backend_url = (
            f"{base_url.rstrip('/')}"
            f"/v1/backends/{backend_name}"
        )

        try:
            configuration = get_json(
                session,
                f"{backend_url}/configuration",
                timeout,
            )

            properties = get_json(
                session,
                f"{backend_url}/properties",
                timeout,
            )

            result[backend_name] = (
                extract_rest_backend_metrics(
                    device,
                    configuration,
                    properties,
                )
            )

        except requests.RequestException as error:
            # Keep fields already available from the list endpoint
            # even when configuration or properties cannot be read.
            result[backend_name] = {
                "qubits": normalize_value(
                    device.get("qubits")
                ),
                "qpu_version": None,
                "processor_type": normalize_value(
                    device.get("processor_type")
                ),
                "clops": extract_clops(device),
                "pending_jobs": normalize_value(
                    device.get("queue_length")
                ),
                "readout_error_median": None,
                "sx_error_median": None,
                "cz_error_median": None,
                "T1_median_us": None,
                "T2_median_us": None,
                "error": (
                    f"{type(error).__name__}: {error}"
                ),
            }

    return result


def select_device_priority(backend_metrics, user_request):
    """
    Select a backend according to ordered user requirements.

    Example:
        qubits=120;processor_type=Nighthawk;clops=240000.0

    A single "=" means:
        qubits=120              -> qubits >= 120
        clops=240000            -> clops >= 240000
        T1_median_us=100        -> T1_median_us >= 100
        T2_median_us=100        -> T2_median_us >= 100
        pending_jobs=10         -> pending_jobs <= 10
        readout_error_median=x  -> readout_error_median <= x
        sx_error_median=x       -> sx_error_median <= x
        cz_error_median=x       -> cz_error_median <= x
        processor_type=value    -> processor_type["family"] == value

    Algorithm:

    1. Start with every available backend.
    2. Extract requirements while preserving their string order.
    3. Apply each requirement as a constraint.
    4. Retain every backend satisfying the current constraint.
    5. Stop and return no selection if no candidates remain.
    6. Continue until all requirements have been processed.
    7. If multiple candidates remain, rank them lexicographically using
       the requested metrics in priority order.
    8. If candidates remain tied, select deterministically by backend name.

    Returns:
        (backend_name, backend_metrics)

    Returns:
        None if no backend satisfies all requirements.
    """

    maximize_metrics = {
        "qubits",
        "clops",
        "T1_median_us",
        "T2_median_us",
    }

    minimize_metrics = {
        "pending_jobs",
        "readout_error_median",
        "sx_error_median",
        "cz_error_median",
    }

    operators = (
        ">=",
        "<=",
        "==",
        "!=",
        ">",
        "<",
        "=",
    )

    def normalize(value):
        if isinstance(value, str):
            value = value.strip()

            if value.lower() in {
                "",
                "none",
                "null",
                "n/a",
                "unavailable",
            }:
                return None

        return value

    def parse_value(text):
        text = text.strip()

        if (
            len(text) >= 2
            and text[0] == text[-1]
            and text[0] in {"'", '"'}
        ):
            return text[1:-1]

        lowered = text.lower()

        if lowered in {"none", "null"}:
            return None

        if lowered == "true":
            return True

        if lowered == "false":
            return False

        try:
            return int(text)
        except ValueError:
            pass

        try:
            return float(text)
        except ValueError:
            return text

    def is_number(value):
        return (
            isinstance(value, (int, float))
            and not isinstance(value, bool)
        )

    def get_metric(metrics, attribute):
        current = metrics

        for part in attribute.split("."):
            if not isinstance(current, dict):
                return None

            if part not in current:
                return None

            current = current[part]

        if (
            attribute == "processor_type"
            and isinstance(current, dict)
        ):
            current = current.get("family")

        return normalize(current)

    def parse_requirement(expression):
        expression = expression.strip()

        for requested_operator in operators:
            if requested_operator not in expression:
                continue

            attribute, raw_value = expression.split(
                requested_operator,
                1,
            )

            attribute = attribute.strip()
            raw_value = raw_value.strip()

            if not attribute:
                raise ValueError(
                    "Missing attribute in requirement: "
                    + repr(expression)
                )

            if not raw_value:
                raise ValueError(
                    "Missing value in requirement: "
                    + repr(expression)
                )

            value = parse_value(raw_value)
            metric_name = attribute.split(".", 1)[0]
            comparison = requested_operator

            if comparison == "=":
                if (
                    metric_name in maximize_metrics
                    and is_number(value)
                ):
                    comparison = ">="

                elif (
                    metric_name in minimize_metrics
                    and is_number(value)
                ):
                    comparison = "<="

                else:
                    comparison = "=="

            return {
                "attribute": attribute,
                "operator": comparison,
                "value": value,
            }

        raise ValueError(
            "Invalid requirement: " + repr(expression)
        )

    def satisfies(actual, comparison, required):
        actual = normalize(actual)
        required = normalize(required)

        if actual is None or required is None:
            if comparison == "==":
                return actual is required

            if comparison == "!=":
                return actual is not required

            return False

        if is_number(actual) and is_number(required):
            actual = float(actual)
            required = float(required)

        elif (
            isinstance(actual, str)
            and isinstance(required, str)
        ):
            actual = actual.casefold()
            required = required.casefold()

        try:
            if comparison == "==":
                return actual == required

            if comparison == "!=":
                return actual != required

            if comparison == ">":
                return actual > required

            if comparison == ">=":
                return actual >= required

            if comparison == "<":
                return actual < required

            if comparison == "<=":
                return actual <= required

        except TypeError:
            return False

        raise ValueError(
            "Unsupported operator: " + comparison
        )

    requirements = []

    for expression in user_request.split(";"):
        expression = expression.strip()

        if expression:
            requirements.append(
                parse_requirement(expression)
            )

    if not requirements:
        raise ValueError(
            "The user request contains no requirements"
        )

    candidates = {}

    for backend_name, metrics in backend_metrics.items():
        if not isinstance(metrics, dict):
            continue

        if "error" in metrics:
            continue

        candidates[backend_name] = metrics

    if not candidates:
        return None

    for requirement in requirements:
        matching_candidates = {}

        for backend_name, metrics in candidates.items():
            actual_value = get_metric(
                metrics,
                requirement["attribute"],
            )

            if satisfies(
                actual_value,
                requirement["operator"],
                requirement["value"],
            ):
                matching_candidates[backend_name] = metrics

        candidates = matching_candidates

        if not candidates:
            return None

    if len(candidates) == 1:
        selected_name = next(iter(candidates))

        return (
            selected_name,
            candidates[selected_name],
        )

    def ranking_key(backend_name):
        metrics = candidates[backend_name]
        key = []

        for requirement in requirements:
            attribute = requirement["attribute"]
            metric_name = attribute.split(".", 1)[0]
            value = get_metric(metrics, attribute)

            if metric_name in maximize_metrics:
                if is_number(value):
                    key.append((0, -float(value)))
                else:
                    key.append((1, 0.0))

            elif metric_name in minimize_metrics:
                if is_number(value):
                    key.append((0, float(value)))
                else:
                    key.append((1, 0.0))

            else:
                key.append((0, 0.0))

        key.append((0, backend_name))

        return tuple(key)

    selected_name = min(
        candidates,
        key=ranking_key,
    )

    return (
        selected_name,
        candidates[selected_name],
    )



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
        if isinstance(val, QPUAttributes):
           value_string = val.to_lsf_string()
        else:
            value_string = str(val)
        lsf_var += f"ESUB_USER_REQ_{key.upper()}={value_string},"

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

    req_qpu = os.getenv("ESUB_USER_REQ_QPU", None)
    config.update({"ESUB_USER_REQ_QPU": req_qpu})

    return config

def extract_qubits(user_request: str) -> int | None:
    for item in user_request.split(";"):
        key, separator, value = item.partition("=")

        if separator and key.strip().lower() == "qubits":
            try:
                return int(value.strip())
            except ValueError as error:
                raise ValueError(
                    f"Invalid qubits value: {value!r}"
                ) from error

    return None



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

# If user wants a specific device, just use it. 
device = config["ESUB_USER_REQ_DEVICE"]
if isinstance(device, str) and device.strip().lower() in {"none", "null", ""}:
    device = None
if device is not None:
    # Set {device}_QRMI variables for a job
    build_qrmi_vars_job(config, device)
    # Launch the job
    subprocess.run(job_args)
    sys.exit(0)

# Get IAM access token
token, crn = read_env_vars_from_config(config)
print_debug("Obtained authentication token and CRN")

# Select best device for a job 
selector = config['ESUB_USER_REQ_SELECTOR']
print_debug("Device selection:", selector)

selectors = {
    "basic": select_device_default,
    "health": select_device_health,
    "priority": select_device_priority,
}

try:
    select_device = selectors[selector]
except KeyError:
    print_error(f"Unknown selector: {selector}")
    raise SystemExit(1)

user_request = config['ESUB_USER_REQ_QPU']
print_debug("User request:", user_request)

if selector == 'priority':
    backend_metrics = get_all_backend_metrics_rest(
        access_token=token,
        service_crn=crn,
    )

    if debug == 'level2':    
        pprint(
            backend_metrics,
            sort_dicts=False,
            stream=sys.stderr
        )

    selection = select_device(
        backend_metrics,
        user_request
    )

    if selection is None:
        print_error("No backend satisfies the requirements")
    else:
        best_device, metrics = selection
        print_debug("Selected backend:", best_device) 
        if debug == 'level2':    
            pprint(
                metrics,
                stream=sys.stderr,
                sort_dicts=False,
            )
else:    
    # Original implementation
    # Extract number of qubits from a user request
    qubits = extract_qubits(user_request)

    if qubits is None:
        print_error("No qubits requirement was specified")
    print_debug("Requested qubits", qubits)

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

    # Get attributes from  each device.
    devices_config = get_device_topology(token, devices, "configuration")
    if not devices_config:
       print_error("No configuration of quantum devices.")
    if debug == 'level2':
       print_debug("Configuration: ", devices_config)

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
