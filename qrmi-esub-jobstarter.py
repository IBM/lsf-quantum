#!/usr/bin/env python3
# -*- coding: utf-8 -*-
# (C) Copyright 2025-2026 IBM. All Rights Reserved.
# Modified to preserve explicit-device application exit codes.
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
import ctypes
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

def print_help() -> None:
    print("Usage:")
    print("  esub.qrmi arguments")
    print("  esub.qrmi -h | --help")
    print()

    print("""Arguments syntax:
            file=<filename>,
            device=<device_name>|qpu.<attribute>=<value>,..,qpu.<attribute>=<value>,
            [selector=basic|health|priority]
    """)

    print("Arguments description:")
    print("  file                       Path to REST API credentials")
    print("  device                     QPU name")
    print("  qpu.qubits                 Minimum number of qubits")
    print("  qpu.processor_type         Required processor family")
    print("  qpu.clops                  Minimum CLOPS value")
    print("  qpu.t1_median_us           T1 median on QPU")
    print("  qpu.t2_median_us           T2 median on QPU")
    print("  qpu.cz_error_median        Median CZ error on QPU  ")
    print("  qpu.sx_error_median        Median SX error on QPU  ")
    print("  qpu.readout_error_median   Median readout error on QPU ")
    print()
    print("Note: device and qpu arguments are mutually exclusive")

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
       print_error("The requirements string contains no requirements")
        

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
    get_all_backend_metrics_qrmi():

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


def validate_env_vars_from_config(config):
    """
    Validate the QRMI related variables read from $CWD/envfile.

    QRMI builds its own authenticated clients from these variables and renews
    the IAM bearer token internally, so this only checks that the required
    variables are present. No token is generated here.
    """
    required = (
        "QRMI_IBM_QCS_IAM_APIKEY",
        "QRMI_IBM_QCS_SERVICE_CRN",
        "QRMI_IBM_QCS_ENDPOINT",
        "QRMI_IBM_QCS_IAM_ENDPOINT",
    )

    for name in required:
        if not config.get(name):
            print_error(f"No {name} provided")


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


def extract_clops(device: dict[str, Any]) -> int | float | str | None:
    """
    Extract CLOPS from a backend-list entry or a backend configuration.

    The backend-list representation is:

        {
            "clops": {
                "type": "hardware",
                "value": 12345
            }
        }

    A backend configuration instead publishes the figure as a scalar under
    clops_h (or clops_v), so all three keys are checked in that order. Scalar
    CLOPS values are also accepted.
    """
    for key in ("clops", "clops_h", "clops_v"):
        clops = normalize_value(device.get(key))

        if clops is None:
            continue

        if isinstance(clops, dict):
            clops = normalize_value(clops.get("value"))

            if clops is None:
                continue

        return clops

    return None


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


def build_provider(config, qpu_type):
    """
    Build a QRMI ResourceProvider used to enumerate available backends.

    Replaces the REST calls to GET /backends. The provider is constructed from
    the QRMI_* variables already present in the configuration, so no bearer
    token has to be generated or cached here -- QRMI renews it internally.
    """
    from qrmi import ResourceProvider, ResourceType

    resource_types = {
        "ibm-quantum-system": ResourceType.IBMQuantumSystem,
        "ibm-quantum-compute-service": ResourceType.IBMQuantumComputeService,
        "qiskit-runtime-service": ResourceType.IBMQiskitRuntimeService,
    }

    try:
        resource_type = resource_types[qpu_type]
    except KeyError as error:
        raise ValueError(
            f"Unsupported QRMI QPU type for backend enumeration: {qpu_type!r}"
        ) from error

    environment = {
        key: str(value)
        for key, value in config.items()
        if key.startswith("QRMI_") and value is not None
    }

    return ResourceProvider(resource_type, environment)


def get_resource_status(resource):
    """
    Return a QRMI resource's status as a plain dict.

    QuantumResource.status() landed after the 0.24.5 release; QRMI 0.25.1 is the
    first published wheel that has it. On 0.24.5 only is_accessible() exists,
    and calling status() unguarded there raises AttributeError, which --
    because both callers wrap their queries in try/except -- would silently
    drop every backend and leave the selectors with nothing to choose from.
    The guard below shapes the older API into the same dict instead, so
    selection keeps working with pending_jobs unavailable.

    Mirrors get_device_status() in elim.qpu.
    """
    if hasattr(resource, "status"):
        return resource.status().to_dict()

    accessible = resource.is_accessible()
    return {
        "status": "online" if accessible else "offline",
        "status_reason": None,
        "healthy": accessible,
        "busy": None,
        "capacity": None,
        "pending_job_count": None,
    }


def extract_qrmi_backend_metrics(
    configuration: dict[str, Any],
    properties: dict[str, Any],
    status: dict[str, Any],
) -> dict[str, Any]:
    """
    Construct metrics for one QRMI backend.

    This is the QRMI counterpart of extract_rest_backend_metrics(). The
    configuration and properties documents are the ones QRMI returns from
    target(); because QRMI passes the service payload through verbatim,
    vendor-specific fields such as clops_h and sample_name are preserved and
    the same extraction helpers apply.

    Unlike the REST variant there is no backend-list "device" entry, so
    pending_jobs comes from the QRMI status document rather than from
    queue_length, and CLOPS and the processor type are read from the
    configuration.
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

    qubits = normalize_value(
        configuration.get("n_qubits")
    )

    qpu_version = normalize_value(
        configuration.get("backend_version")
    )

    processor_type = extract_processor_type(configuration)

    return {
        "qubits": qubits,
        "qpu_version": qpu_version,
        "processor_type": processor_type,
        "clops": extract_clops(configuration),
        "pending_jobs": normalize_value(
            status.get("pending_job_count")
        ),
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


def extract_processor_type(
    configuration: dict[str, Any],
) -> Any | None:
    """
    Extract the processor type from a backend configuration.

    Two representations are accepted:

      - a processor_type object, as published by the backend configuration
        schema: {"family": "Heron", "revision": "2"}
      - a sample_name string: "family: Heron, revision: 2"

    The object form is returned unchanged so that selectors comparing
    processor_type["family"] keep working. The string form is converted to the
    same shape.
    """
    processor_type = normalize_value(
        configuration.get("processor_type")
    )

    if processor_type is not None:
        return processor_type

    sample_name = normalize_value(
        configuration.get("sample_name")
    )

    if not isinstance(sample_name, str):
        return None

    parsed: dict[str, str] = {}

    for part in sample_name.split(","):
        key, separator, value = part.partition(":")

        if not separator:
            continue

        parsed[key.strip().lower()] = value.strip()

    family = parsed.get("family")

    if family is None:
        return None

    processor_type = {"family": family}

    revision = parsed.get("revision")

    if revision is not None:
        processor_type["revision"] = revision

    return processor_type


def get_all_backend_metrics_qrmi(
    config,
    qpu_type: str,
    *,
    filters: str | None = None,
) -> dict[str, dict[str, Any]]:
    """
    Retrieve all accessible backends via QRMI and return a nested metrics
    dictionary keyed by backend name.

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
    provider = build_provider(config, qpu_type)

    resources = provider.resources(filters)

    result: dict[str, dict[str, Any]] = {}

    for resource in resources:
        try:
            backend_name = str(resource.resource_id())
        except Exception as error:
            print(
                f"Skipping a backend whose identifier could not be read: {error}",
                file=sys.stderr,
            )
            continue

        try:
            target = json.loads(resource.target().value)

            configuration = target.get("configuration") or {}
            properties = target.get("properties") or {}

            status = get_resource_status(resource)

            result[backend_name] = (
                extract_qrmi_backend_metrics(
                    configuration,
                    properties,
                    status,
                )
            )

        except Exception as error:
            # Record the backend as unusable rather than aborting the whole
            # selection, matching the REST implementation's behaviour.
            result[backend_name] = {
                "qubits": None,
                "qpu_version": None,
                "processor_type": None,
                "clops": None,
                "pending_jobs": None,
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


def get_devices_topology_qrmi(
    config,
    qpu_type: str,
    *,
    filters: str | None = None,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """
    Enumerate backends via QRMI and return (devices_status, devices_config) in
    the shape the "basic" and "health" selectors expect.
    """
    provider = build_provider(config, qpu_type)

    resources = provider.resources(filters)

    devices_status: list[dict[str, Any]] = []
    devices_config: list[dict[str, Any]] = []

    for resource in resources:
        try:
            backend_name = str(resource.resource_id())

            status = get_resource_status(resource)

            target = json.loads(resource.target().value)

            configuration = target.get("configuration") or {}
            properties = target.get("properties") or {}
        except Exception as error:
            print(
                f"Skipping a backend that could not be queried: {error}",
                file=sys.stderr,
            )
            continue

        # QRMI reports 'online' where the REST status endpoint reported the
        # message 'available'.
        message = (
            "available"
            if status.get("status") == "online"
            else str(status.get("status_reason") or status.get("status"))
        )

        # Key order matters: select_device_default() flattens these dicts into
        # positional lists by iterating them, and then indexes those lists. It
        # expects message, length_queue, device for the status entries and
        # n_qubits, device for the configuration entries.
        # The selectors sort and do arithmetic on length_queue, so it must be
        # an int. The REST status endpoint always supplied one; QRMI reports
        # None when the vendor omits it, and QRMI <= 0.24.5 has no status() at
        # all (see get_resource_status). Fall back to 0 -- treating an unknown
        # queue as empty keeps every backend eligible, which matches the old
        # behaviour of ranking on the other criteria.
        queue_length = normalize_value(status.get("pending_job_count"))
        if queue_length is None:
            queue_length = 0

        devices_status.append(
            {
                "message": message,
                "length_queue": queue_length,
                "device": backend_name,
            }
        )

        # The "health" selector additionally scores on median T1 and readout
        # error. QRMI returns the calibration data in the same target()
        # document as the configuration, so these are filled in here rather
        # than requiring a separate properties request. Any key added below
        # must come after n_qubits to keep the positional indexing above
        # valid.
        readout_errors, t1_values_us, _ = (
            extract_qubit_metrics(properties)
        )

        devices_config.append(
            {
                "n_qubits": normalize_value(
                    configuration.get("n_qubits")
                ),
                "device": backend_name,
                "T1_median_µs": safe_median(
                    t1_values_us
                ),
                "readout_error_median": safe_median(
                    readout_errors
                ),
            }
        )

    return devices_status, devices_config


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
                print_error(
                    "Missing attribute in requirement: "
                    + repr(expression)
                )

            if not raw_value:
                print_error(
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
        print_error(
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

    Picks the highest composite-health-score device with enough qubits,
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
# To be implemented for more complex topology decisions. The calibration
# properties are already available from the QRMI target() document, which
# get_devices_topology_qrmi() reads; the defaults (pulse-level) document is
# not exposed by QRMI and needs a different implementation.
#-------------------------------------------------------------------------

def build_quantum_resource(device, qpu_type):
    """Build a QRMI resource using the configured QPU resource type."""
    from qrmi import QuantumResource, ResourceType

    resource_types = {
        "ibm-quantum-system": ResourceType.IBMQuantumSystem,
        "ibm-quantum-compute-service": ResourceType.IBMQuantumComputeService,
        "qiskit-runtime-service": ResourceType.IBMQiskitRuntimeService,
        "pasqal-cloud": ResourceType.PasqalCloud,
        "iqm-server": ResourceType.IQMServer,
        "alice-bob-felis": ResourceType.AliceBobFelis,
    }

    try:
        resource_type = resource_types[qpu_type]
    except KeyError as error:
        raise ValueError(
            f"Unsupported QRMI QPU type: {qpu_type!r}"
        ) from error

    return QuantumResource(
        device,
        resource_type,
    )


def acquire_quantum_resource(resource, device):
    """Acquire the selected QRMI resource and export its acquisition token."""
    token = resource.acquire()
    os.environ[f"{device}_QRMI_JOB_ACQUISITION_TOKEN"] = token
    return token


def release_quantum_resource(resource, device, token):
    """Release the QRMI resource and remove its acquisition token."""
    try:
        resource.release(token)
    except Exception as error:
        print(
            f"Failed to release QRMI resource {device}: {error}",
            file=sys.stderr,
        )
    finally:
        os.environ.pop(
            f"{device}_QRMI_JOB_ACQUISITION_TOKEN",
            None,
        )


def build_qrmi_vars_job(config, device):
    """
    Build the resource-scoped QRMI environment variables for a job.
    """
    job_variables = {
        "QRMI_JOB_QPU_RESOURCES",
        "QRMI_JOB_QPU_TYPES",
    }

    for key, value in config.items():
        if not key.startswith("QRMI_"):
            continue

        if key in job_variables:
            continue

        os.environ[f"{device}_{key}"] = str(value)
        os.environ.pop(key, None)

    os.environ["QRMI_IBM_QCS_BEST_DEVICE"] = device
    os.environ["QRMI_JOB_QPU_RESOURCES"] = device


def read_process_environ():
    """
    Read this process's real environment, not Python's cached copy.

    QRMI's ResourceProvider injects its per-backend variables from Rust via
    env::set_var(), which calls libc setenv(). That mutates the C-level
    `environ` that execve() passes to children, but Python populated
    os.environ once at interpreter start and never re-reads it -- so those
    names are absent from os.environ while still being inherited by every
    subprocess. Deleting them from os.environ is a no-op for the same reason.

    The C `environ` symbol is read through ctypes, which reflects setenv()
    calls made by native code and works wherever libc does. /proc/self/environ
    is deliberately not used: on Linux it reports the environment as it was at
    exec() time, so it would miss exactly the writes this function exists to
    find.

    Falls back to os.environ if the symbol cannot be reached, in which case the
    prune sees only what Python knows about -- no worse than not pruning.
    """
    try:
        libc = ctypes.CDLL(None)
        environ = ctypes.POINTER(ctypes.c_char_p).in_dll(libc, "environ")
    except (OSError, ValueError):
        return dict(os.environ)

    live = {}
    index = 0
    while environ[index]:
        name, separator, value = environ[index].partition(b"=")
        if separator:
            live[name.decode("utf-8", "replace")] = value.decode("utf-8", "replace")
        index += 1
    return live


def snapshot_backend_env():
    """
    Record the environment as it stands before backend enumeration.

    QRMI's ResourceProvider calls setenv() for every backend it enumerates:
    inject_backend_env() in
    qrmi/src/ibm/quantum_compute_service_provider/mod.rs writes
    "<backend>_<KEY>" for each provider setting, because
    IBMQuantumComputeService::new() resolves its configuration only through
    that resource-scoped name. Enumerating N backends therefore leaves N sets
    of prefixed variables in this process, all of which the job would otherwise
    inherit.

    Values are captured, not just names, because inject_backend_env()
    overwrites an existing "<backend>_<KEY>" as readily as it creates one: a
    site-set value for an unselected device would otherwise be silently
    replaced by the provider's own, which is harder to notice than an outright
    removal.
    """
    return read_process_environ()


def job_environment(before, device):
    """
    Build the environment for the job, with QRMI's per-backend injections for
    the unselected backends undone.

    Returns a dict to hand to subprocess.run(env=...). The injected names live
    only in the C-level environment (see read_process_environ()), so they
    cannot be removed via os.environ -- the job's environment is therefore
    constructed explicitly rather than inherited.

    Each affected variable is restored to what `before` recorded: a name QRMI
    created is dropped, and one it overwrote is put back to the site-set value
    rather than dropped. Variables for the selected device are kept as QRMI
    wrote them, since acquire()/release() and the job itself rely on them.

    Pruning is unconditional: the job's QRMI variables always describe only the
    selected device. LSF_QRMI_DEBUG=level1 or level2 adds an informational
    listing of the backends QRMI enumerated, which would otherwise be lost, but
    does not change what the job receives.
    """
    live = read_process_environ()

    # os.environ carries what this script set itself (build_qrmi_vars_job,
    # acquire_quantum_resource); `live` carries those plus QRMI's native
    # writes. Merge so neither is lost.
    environment = dict(live)
    environment.update(os.environ)

    def is_foreign(name):
        # Resource-scoped names only: "QRMI_*" is the unprefixed provider
        # configuration, which build_qrmi_vars_job() handles.
        if "QRMI" not in name or name.startswith("QRMI_"):
            return False
        return not name.startswith(f"{device}_")

    created = [
        name for name in environment if is_foreign(name) and name not in before
    ]
    overwritten = [
        name
        for name in environment
        if is_foreign(name) and name in before and environment[name] != before[name]
    ]

    if not created and not overwritten:
        return environment

    # Report the enumeration before discarding it: these names are the only
    # record of which backends QRMI considered, and pruning removes them.
    if debug:
        enumerated = sorted(
            {name.split("_QRMI", 1)[0] for name in created + overwritten}
            | {device}
        )
        print_debug(
            f"QRMI enumerated {len(enumerated)} available device(s):",
            ", ".join(enumerated),
        )
        print_debug("Setting QRMI variables for the selected device only:", device)

    for name in created:
        environment.pop(name, None)
    for name in overwritten:
        environment[name] = before[name]

    print_debug(
        f"Pruned {len(created)} injected and restored {len(overwritten)} "
        f"overwritten QRMI variable(s) for unselected backends"
    )

    return environment


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
    Read QRMI configuration and ESUB requests from the environment.

    QRMI provider variables are preserved generically so that the
    jobstarter does not assume a specific backend namespace.
    """
    config = {
        key: value
        for key, value in os.environ.items()
        if key.startswith("QRMI_")
    }

    config.update(
        {
            "ESUB_USER_REQ_SELECTOR": os.getenv(
                "ESUB_USER_REQ_SELECTOR"
            ),
            "ESUB_USER_REQ_DEVICE": os.getenv(
                "ESUB_USER_REQ_DEVICE"
            ),
            "ESUB_USER_REQ_QPU": os.getenv(
                "ESUB_USER_REQ_QPU"
            ),
        }
    )

    qpu_type = config.get("QRMI_JOB_QPU_TYPES")
    if not qpu_type:
        print_error("No QRMI_JOB_QPU_TYPES provided")

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

    if len(sys.argv) == 1 or sys.argv[1] in ("-h", "--help"):
        print_help()
        exit(0)

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
qpu_type = config["QRMI_JOB_QPU_TYPES"].strip()
#print(config, file=sys.stderr)

# If user wants a specific device, just use it. 
device = config["ESUB_USER_REQ_DEVICE"]
if isinstance(device, str) and device.strip().lower() in {"none", "null", ""}:
    device = None
if device is not None:
    # No enumeration on this path, so QRMI injects nothing: snapshot and prune
    # are still applied so the job's environment is built the same way.
    env_before_enumeration = snapshot_backend_env()

    # Set {device}_QRMI variables for a job
    build_qrmi_vars_job(config, device)

    resource = build_quantum_resource(
        device,
        qpu_type,
    )
    acquisition_token = acquire_quantum_resource(resource, device)

    job_env = job_environment(env_before_enumeration, device)

    try:
        # Launch the job
        return_code = subprocess.run(job_args, env=job_env).returncode
    finally:
        release_quantum_resource(
            resource,
            device,
            acquisition_token,
        )

    sys.exit(return_code)

# Validate the QRMI configuration. No IAM access token is generated here:
# QRMI acquires and renews the bearer token internally on each call.
validate_env_vars_from_config(config)
print_debug("Validated QRMI configuration")

# Record the environment before enumeration: QRMI's ResourceProvider setenv()s
# a "<backend>_<KEY>" set for every backend it inspects, and only the selected
# device's set should reach the job.
env_before_enumeration = snapshot_backend_env()

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
    backend_metrics = get_all_backend_metrics_qrmi(
        config,
        qpu_type,
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
    # Extract number of qubits from a user request
    qubits = extract_qubits(user_request)

    if qubits is None:
        print_error("No qubits requirement was specified")
    print_debug("Requested qubits", qubits)

    # Get status and attributes of each available device. 
    devices_status, devices_config = get_devices_topology_qrmi(
        config,
        qpu_type,
    )

    if not devices_status:
        print_error("No status of quantum devices.")
    print_debug("Status: ", devices_status)

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

resource = build_quantum_resource(
    best_device,
    qpu_type,
)

acquisition_token = acquire_quantum_resource(
    resource,
    best_device,
)

# Build the job's environment explicitly, dropping the per-backend variables
# QRMI injected for the backends it enumerated but we did not select. They live
# in the C-level environment, so they cannot be removed via os.environ.
job_env = job_environment(env_before_enumeration, best_device)

try:
    # Launch the job
    return_code = subprocess.run(job_args, env=job_env).returncode
finally:
    release_quantum_resource(
        resource,
        best_device,
        acquisition_token,
    )

sys.exit(return_code)
