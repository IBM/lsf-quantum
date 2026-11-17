# SPDX-License-Identifier: Apache-2.0
"""Use the resource already acquired by the LSF QRMI jobstarter."""
import json
import os
from pathlib import Path
import sys

import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--expected-uid", type=int, required=True)
parser.add_argument("--expected-gid", type=int, required=True)
parser.add_argument("--fail", action="store_true")
args = parser.parse_args()

assert os.getuid() == args.expected_uid
assert os.getgid() == args.expected_gid

device = os.environ["QRMI_JOB_QPU_RESOURCES"]
assert os.environ["QRMI_JOB_QPU_TYPES"] == "ibm-quantum-compute-service"

required = [
    device + "_QRMI_JOB_ACQUISITION_TOKEN",
    *[
        device + "_QRMI_IBM_QCS_" + suffix
        for suffix in (
            "ENDPOINT", "IAM_ENDPOINT", "IAM_APIKEY",
            "SERVICE_CRN", "SESSION_MODE",
        )
    ],
]
missing = [key for key in required if not os.environ.get(key)]
if missing:
    sys.exit("Missing environment variables: " + ", ".join(missing))

print("Container identity and QRMI environment: PASS", flush=True)
print("Selected device:", device, flush=True)

if args.fail:
    print("Controlled failure before quantum submission", flush=True)
    sys.exit(42)

from qiskit import QuantumCircuit
from qiskit.transpiler.preset_passmanagers import generate_preset_pass_manager
from qrmi import QuantumResource, ResourceType
from qrmi.primitives.ibm import SamplerV2, get_target

resource = QuantumResource(device, ResourceType.IBMQuantumComputeService)
circuit = QuantumCircuit(2)
circuit.h(0)
circuit.cx(0, 1)
circuit.measure_all()

isa = generate_preset_pass_manager(
    optimization_level=1, target=get_target(resource)
).run(circuit)

job = SamplerV2(resource).run([isa], shots=128)
print("Quantum job ID:", job.job_id(), flush=True)

counts = job.result()[0].data.meas.get_counts()
if sum(counts.values()) != 128:
    raise RuntimeError("Expected exactly 128 shots")
if not set(counts).issubset({"00", "01", "10", "11"}):
    raise RuntimeError("Unexpected two-qubit measurement keys")

report = {
    "device": device,
    "quantum_job_id": job.job_id(),
    "uid": os.getuid(),
    "gid": os.getgid(),
    "shots": 128,
    "counts": counts,
    "bell_correlated_fraction": (
        counts.get("00", 0) + counts.get("11", 0)
    ) / 128,
}
Path("/results/bell-result.json").write_text(
    json.dumps(report, indent=2) + "\n"
)
print(json.dumps(report, indent=2), flush=True)
print("Quantum result verified: 128 shots", flush=True)
