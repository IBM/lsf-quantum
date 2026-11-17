# (C) Copyright 2026 IBM. All Rights Reserved.
#
# Licensed under the Apache License, Version 2.0.
# See LICENSE in the repository root.

"""Submit a 128-shot Bell circuit using the LSF-selected QRMI resource."""

import os

from qiskit import QuantumCircuit
from qiskit.transpiler.preset_passmanagers import generate_preset_pass_manager
from qrmi import QuantumResource, ResourceType
from qrmi.primitives.ibm import SamplerV2, get_target


def main():
    device = os.environ["QRMI_JOB_QPU_RESOURCES"]
    if os.environ["QRMI_JOB_QPU_TYPES"] != "ibm-quantum-compute-service":
        raise ValueError("This example requires ibm-quantum-compute-service")

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
    result = job.result()
    counts = result[0].data.meas.get_counts()
    print("Counts:", counts, flush=True)
    if sum(counts.values()) != 128:
        raise RuntimeError("Expected 128 measurement shots")
    print("Quantum result verified: 128 shots", flush=True)


if __name__ == "__main__":
    main()
