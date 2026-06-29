#!/usr/bin/env python
# coding: utf-8

# (C) Copyright 2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.

"""Sequential non-hierarchical circuit-cutting baseline.

Purpose
-------
This script is a sequential baseline for comparison with the hierarchical MPI
version.  It does *not* use MPI and does *not* perform hierarchical cuts.

Workflow
--------
1. Build the same 16-qubit test circuit and observables.
2. Apply one flat four-way partition_problem() call on the original circuit:
       L0 | L1 | R0 | R1
3. Generate all QPD subexperiments once with generate_cutting_experiments().
4. Execute every partition stream sequentially on one process using
   qiskit_aer.primitives.SamplerV2.from_backend(FakeMarrakesh()).
5. Reconstruct all expectation values with one reconstruct_expectation_values()
   call.
6. Verify against the original uncut circuit with EstimatorV2.
7. Print timing information for performance comparison.

Example
-------
    python poster_sequential_baseline.py --num-samples 100 --shots 4096

Quick smoke test:
    python poster_sequential_baseline.py --num-samples 8 --shots 512
"""
from __future__ import annotations

import argparse
import time
from typing import Any, Hashable

import numpy as np
from qiskit import QuantumCircuit
from qiskit.quantum_info import PauliList
from qiskit.transpiler.preset_passmanagers import generate_preset_pass_manager
from qiskit_addon_cutting import (
    generate_cutting_experiments,
    partition_problem,
    reconstruct_expectation_values,
)
from qiskit_aer.primitives import EstimatorV2, SamplerV2
from qiskit_ibm_runtime.fake_provider import FakeMarrakesh


# -----------------------------------------------------------------------------
# Circuit and observable construction
# -----------------------------------------------------------------------------


def make_16q_cutting_circuit(theta: float = 0.37) -> QuantumCircuit:
    """Create the same 16-qubit test circuit used by the MPI examples."""
    qc = QuantumCircuit(16, name="sequential_16q_cutting_baseline")

    for q in range(16):
        qc.ry(theta * (q + 1), q)
        qc.rz(0.13 * (q + 1), q)

    blocks = ([0, 1, 2, 3], [4, 5, 6, 7], [8, 9, 10, 11], [12, 13, 14, 15])

    for a, b, c, d in blocks:
        qc.cx(a, b)
        qc.rz(theta, b)
        qc.cx(b, c)
        qc.ry(0.7 * theta, c)
        qc.cx(c, d)
        qc.rz(1.3 * theta, d)
        qc.cx(d, a)
        qc.ry(0.5 * theta, a)

    # Boundary gates for the flat four-way cut.
    qc.cx(3, 4)       # L0 -- L1
    qc.cx(7, 8)       # L1 -- R0
    qc.cx(11, 12)     # R0 -- R1

    for a, b, c, d in blocks:
        qc.rx(0.2 + theta, a)
        qc.ry(0.4 + theta, b)
        qc.rz(0.6 + theta, c)
        qc.rx(0.8 + theta, d)
        qc.cx(a, c)
        qc.cx(b, d)

    qc.cx(1, 6)       # L0 -- L1
    qc.cx(5, 10)      # L1 -- R0
    qc.cx(9, 14)      # R0 -- R1
    qc.cx(2, 13)      # L0 -- R1

    for q in range(16):
        qc.rz(0.11 * (q + 1), q)
        qc.ry(0.07 * (16 - q), q)

    return qc


def make_observables() -> PauliList:
    """Example Pauli observables on the original 16-qubit circuit."""
    return PauliList(
        [
            "ZZZZIIIIIIIIIIII",
            "IIIIZZZZIIIIIIII",
            "IIIIIIIIZZZZIIII",
            "IIIIIIIIIIIIZZZZ",
            "ZIIIZIIIZIIIZIII",
            "XIIIXIIIXIIIXIII",
        ]
    )


def final_four_way_labels() -> list[str]:
    """Flat partition labels for the non-hierarchical baseline."""
    return ["L0"] * 4 + ["L1"] * 4 + ["R0"] * 4 + ["R1"] * 4


# -----------------------------------------------------------------------------
# Helpers
# -----------------------------------------------------------------------------


def parse_num_samples(value: str) -> int:
    """Use finite sampling in the baseline; exact enumeration is intentionally avoided."""
    if value.lower() in {"inf", "infinity", "np.inf"}:
        raise argparse.ArgumentTypeError(
            "Use a finite --num-samples for the baseline. Exact enumeration can be huge."
        )
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("--num-samples must be >= 1")
    return parsed


def sampling_overhead(bases) -> float:
    """Product of QPD basis overheads."""
    if not bases:
        return 1.0
    return float(np.prod([basis.overhead for basis in bases]))


def exact_term_count_estimate(bases) -> int:
    """Approximate exact QPD term count as product of coefficient-list lengths."""
    nterms = 1
    for basis in bases:
        nterms *= len(basis.coeffs)
    return int(nterms)


# -----------------------------------------------------------------------------
# Sequential cutting, execution, reconstruction, verification
# -----------------------------------------------------------------------------


def build_flat_problem_and_experiments(
    circuit: QuantumCircuit,
    observables: PauliList,
    num_samples: int,
) -> dict[str, Any]:
    """Build a non-hierarchical flat four-way cutting problem."""
    t0 = time.perf_counter()
    problem = partition_problem(
        circuit=circuit,
        partition_labels=final_four_way_labels(),
        observables=observables,
    )
    t_partition = time.perf_counter() - t0

    print("[baseline] flat partition labels:", list(problem.subcircuits.keys()), flush=True)
    print("[baseline] number of QPD bases/cuts:", len(problem.bases), flush=True)
    print("[baseline] sampling overhead estimate:", sampling_overhead(problem.bases), flush=True)
    print("[baseline] estimated exact QPD terms:", exact_term_count_estimate(problem.bases), flush=True)

    t0 = time.perf_counter()
    subexperiments, coefficients = generate_cutting_experiments(
        circuits=problem.subcircuits,
        observables=problem.subobservables,
        num_samples=num_samples,
    )
    t_generate = time.perf_counter() - t0

    print("[baseline] generated coefficient count:", len(coefficients), flush=True)
    print(
        "[baseline] stream sizes:",
        [(label, len(circuits)) for label, circuits in subexperiments.items()],
        flush=True,
    )

    return {
        "problem": problem,
        "subexperiments": subexperiments,
        "coefficients": coefficients,
        "subobservables": problem.subobservables,
        "timing": {
            "partition_seconds": t_partition,
            "generate_experiments_seconds": t_generate,
        },
    }


def execute_sequentially(
    subexperiments: dict[Hashable, list[QuantumCircuit]],
    *,
    shots: int,
    seed: int,
    optimization_level: int,
) -> tuple[dict[Hashable, Any], dict[str, float]]:
    """Execute all partition streams sequentially on one process."""
    backend = FakeMarrakesh()
    sampler = SamplerV2.from_backend(backend, default_shots=shots, seed=seed)
    pass_manager = generate_preset_pass_manager(
        backend=backend,
        optimization_level=optimization_level,
    )

    results = {}
    stream_times = {}

    for label, circuits in subexperiments.items():
        print(f"[baseline] transpiling and executing {label}: {len(circuits)} circuits", flush=True)
        t0 = time.perf_counter()
        isa_circuits = pass_manager.run(circuits)
        t_transpile = time.perf_counter() - t0

        t0 = time.perf_counter()
        result = sampler.run(isa_circuits).result()
        t_execute = time.perf_counter() - t0

        results[label] = result
        stream_times[f"{label}_transpile_seconds"] = t_transpile
        stream_times[f"{label}_execute_seconds"] = t_execute

        print(
            f"[baseline] completed {label}: "
            f"transpile={t_transpile:.3f}s, execute={t_execute:.3f}s",
            flush=True,
        )

    return results, stream_times


def reconstruct_baseline(
    results: dict[Hashable, Any],
    coefficients,
    subobservables,
) -> tuple[list[float], float]:
    """Reconstruct expectation values using Qiskit's cutting reconstruction."""
    t0 = time.perf_counter()
    reconstructed = reconstruct_expectation_values(
        results,
        coefficients,
        subobservables,
    )
    t_reconstruct = time.perf_counter() - t0
    return list(reconstructed), t_reconstruct


def verify_against_uncut_circuit(
    circuit: QuantumCircuit,
    observables: PauliList,
    reconstructed_expvals: list[float],
) -> float:
    """Verify reconstructed values against EstimatorV2 on the uncut circuit."""
    print("[baseline] verification against original uncut circuit:", flush=True)
    estimator = EstimatorV2()
    t0 = time.perf_counter()

    for observable, reconstructed_expval in zip(observables, reconstructed_expvals):
        exact_expval = estimator.run([(circuit, observable)]).result()[0].data.evs
        exact_expval = np.asarray(exact_expval).item()

        print(f"Observable: {observable}", flush=True)
        print(
            f"Reconstructed expectation value: "
            f"{np.real(np.round(reconstructed_expval, 8))}",
            flush=True,
        )
        print(f"Exact expectation value: {np.round(exact_expval, 8)}", flush=True)
        print(
            f"Error in estimation: "
            f"{np.real(np.round(reconstructed_expval - exact_expval, 8))}",
            flush=True,
        )
        if np.isclose(exact_expval, 0.0):
            print(
                "Relative error in estimation: undefined because exact expectation value is approximately zero",
                flush=True,
            )
        else:
            print(
                f"Relative error in estimation: "
                f"{np.real(np.round((reconstructed_expval - exact_expval) / exact_expval, 8))}",
                flush=True,
            )

    return time.perf_counter() - t0


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Sequential non-hierarchical circuit-cutting baseline."
    )
    parser.add_argument("--num-samples", type=parse_num_samples, default=100)
    parser.add_argument("--shots", type=int, default=4096)
    parser.add_argument("--seed", type=int, default=1234)
    parser.add_argument("--optimization-level", type=int, default=1)
    args = parser.parse_args()

    total_start = time.perf_counter()

    circuit = make_16q_cutting_circuit()
    observables = make_observables()

    print("[baseline] original circuit qubits:", circuit.num_qubits, flush=True)
    print("[baseline] original circuit depth:", circuit.depth(), flush=True)
    print("[baseline] num_samples:", args.num_samples, flush=True)
    print("[baseline] shots:", args.shots, flush=True)

    payload = build_flat_problem_and_experiments(
        circuit=circuit,
        observables=observables,
        num_samples=args.num_samples,
    )

    results, execution_timing = execute_sequentially(
        payload["subexperiments"],
        shots=args.shots,
        seed=args.seed,
        optimization_level=args.optimization_level,
    )

    reconstructed_expvals, t_reconstruct = reconstruct_baseline(
        results,
        payload["coefficients"],
        payload["subobservables"],
    )

    print("[baseline] reconstructed expectation values:", flush=True)
    for obs, value in zip(observables, reconstructed_expvals):
        print(f"  {obs}: {value}", flush=True)

    t_verify = verify_against_uncut_circuit(
        circuit,
        observables,
        reconstructed_expvals,
    )

    total_seconds = time.perf_counter() - total_start

    print("[baseline] timing summary:", flush=True)
    print(f"  partition_seconds: {payload['timing']['partition_seconds']:.6f}", flush=True)
    print(f"  generate_experiments_seconds: {payload['timing']['generate_experiments_seconds']:.6f}", flush=True)
    for key, value in execution_timing.items():
        print(f"  {key}: {value:.6f}", flush=True)
    print(f"  reconstruct_seconds: {t_reconstruct:.6f}", flush=True)
    print(f"  verify_seconds: {t_verify:.6f}", flush=True)
    print(f"  total_seconds: {total_seconds:.6f}", flush=True)
    print("[baseline] workflow complete", flush=True)


if __name__ == "__main__":
    main()
