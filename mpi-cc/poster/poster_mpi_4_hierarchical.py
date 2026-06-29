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

"""Hierarchical MPI/Qiskit circuit cutting with level-2 reconstruction.

This file restores the original hierarchical idea:

    original circuit
        -> rank 0 first-level cut: L | R
        -> rank 0/1 second-level cuts: L -> L0,L1 and R -> R0,R1
        -> ranks 0..3 execute second-level child streams
        -> rank 0 reconstructs L from L0,L1
        -> rank 1 reconstructs R from R0,R1
        -> rank 0 combines reconstructed L/R vectors and verifies against the
           original uncut circuit with Aer EstimatorV2

Important scientific note
-------------------------
This is a hierarchical reconstruction scaffold using qiskit_addon_cutting's
reconstruct_expectation_values() at each level-2 parent.  It avoids creating one
large joint QPD experiment set for all final leaves.  This is exactly the intended
small-cuts-per-level workflow.

However, Qiskit's reconstruct_expectation_values() returns expectation values,
not an intermediate PrimitiveResult-like object that can be fed into a higher
level reconstruct_expectation_values() call.  Therefore the final L/R combination
below is implemented explicitly as an observable-wise product of the two
reconstructed parent contributions.  The verification block at the end reports
how close this hierarchical approximation is for the chosen circuit and sampling
settings.
"""
from __future__ import annotations

import time
import argparse
from typing import Any, Hashable

import numpy as np
from mpi4py import MPI
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


def make_16q_cutting_circuit(theta: float = 0.37) -> QuantumCircuit:
    qc = QuantumCircuit(16, name="hierarchical_16q_cutting_test")
    for q in range(16):
        qc.ry(theta * (q + 1), q)
        qc.rz(0.13 * (q + 1), q)

    blocks = ([0, 1, 2, 3], [4, 5, 6, 7], [8, 9, 10, 11], [12, 13, 14, 15])
    for a, b, c, d in blocks:
        qc.cx(a, b); qc.rz(theta, b)
        qc.cx(b, c); qc.ry(0.7 * theta, c)
        qc.cx(c, d); qc.rz(1.3 * theta, d)
        qc.cx(d, a); qc.ry(0.5 * theta, a)

    # Hierarchical boundary gates.
    qc.cx(3, 4)       # inside L, crosses L0/L1
    qc.cx(7, 8)       # top-level L/R
    qc.cx(11, 12)     # inside R, crosses R0/R1

    for a, b, c, d in blocks:
        qc.rx(0.2 + theta, a)
        qc.ry(0.4 + theta, b)
        qc.rz(0.6 + theta, c)
        qc.rx(0.8 + theta, d)
        qc.cx(a, c)
        qc.cx(b, d)

    qc.cx(1, 6)       # inside L, crosses L0/L1
    qc.cx(5, 10)      # top-level L/R
    qc.cx(9, 14)      # inside R, crosses R0/R1
    qc.cx(2, 13)      # top-level L/R

    for q in range(16):
        qc.rz(0.11 * (q + 1), q)
        qc.ry(0.07 * (16 - q), q)
    return qc


def make_observables() -> PauliList:
    return PauliList([
        "ZZZZIIIIIIIIIIII",
        "IIIIZZZZIIIIIIII",
        "IIIIIIIIZZZZIIII",
        "IIIIIIIIIIIIZZZZ",
        "ZIIIZIIIZIIIZIII",
        "XIIIXIIIXIIIXIII",
    ])


def parse_num_samples(value: str) -> int:
    if value.lower() in {"inf", "infinity", "np.inf"}:
        raise argparse.ArgumentTypeError(
            "For hierarchical mode use a finite num_samples. Exact enumeration "
            "at each level can still be very expensive."
        )
    value_int = int(value)
    if value_int < 1:
        raise argparse.ArgumentTypeError("num_samples must be >= 1")
    return value_int


def sampling_overhead(bases) -> float:
    return 1.0 if not bases else float(np.prod([basis.overhead for basis in bases]))


def first_level_labels() -> list[str]:
    return ["L"] * 8 + ["R"] * 8


def second_level_labels(parent: str) -> list[str]:
    if parent == "L":
        return ["L0"] * 4 + ["L1"] * 4
    if parent == "R":
        return ["R0"] * 4 + ["R1"] * 4
    raise ValueError(f"Unknown parent {parent!r}")


def build_first_level(comm, circuit: QuantumCircuit, observables: PauliList) -> dict[str, Any]:
    rank = comm.Get_rank()
    if rank == 0:
        print("[rank 0] first-level cut: L | R", flush=True)
        p1 = partition_problem(circuit, first_level_labels(), observables)
        payload = {
            "subcircuits": p1.subcircuits,
            "subobservables": p1.subobservables,
            "bases": p1.bases,
        }
        print(
            f"[rank 0] first-level cuts={len(p1.bases)}, "
            f"overhead={sampling_overhead(p1.bases)}",
            flush=True,
        )
    else:
        payload = None
    return comm.bcast(payload, root=0)


def build_second_level(comm, first_payload: dict[str, Any]) -> dict[str, Any] | None:
    """Rank 0 builds L second level; rank 1 builds R second level."""
    rank = comm.Get_rank()
    local = None
    if rank == 0:
        parent = "L"
        print("[rank 0] second-level cut: L -> L0 | L1", flush=True)
        problem = partition_problem(
            first_payload["subcircuits"][parent],
            second_level_labels(parent),
            first_payload["subobservables"][parent],
        )
        local = (parent, problem)
        print(
            f"[rank 0] L cuts={len(problem.bases)}, overhead={sampling_overhead(problem.bases)}",
            flush=True,
        )
    elif rank == 1:
        parent = "R"
        print("[rank 1] second-level cut: R -> R0 | R1", flush=True)
        problem = partition_problem(
            first_payload["subcircuits"][parent],
            second_level_labels(parent),
            first_payload["subobservables"][parent],
        )
        local = (parent, problem)
        print(
            f"[rank 1] R cuts={len(problem.bases)}, overhead={sampling_overhead(problem.bases)}",
            flush=True,
        )

    gathered = comm.gather(local, root=0)
    if rank == 0:
        second = {}
        for item in gathered:
            if item is not None:
                parent, problem = item
                second[parent] = {
                    "subcircuits": problem.subcircuits,
                    "subobservables": problem.subobservables,
                    "bases": problem.bases,
                }
        return second
    return None


def build_execution_tasks(second_level: dict[str, Any], num_samples: int) -> list[dict[str, Any]]:
    """Rank-0 builds one task per parent and child stream."""
    tasks = []
    for parent, data in second_level.items():
        subexperiments, coefficients = generate_cutting_experiments(
            circuits=data["subcircuits"],
            observables=data["subobservables"],
            num_samples=num_samples,
        )
        child_labels = list(subexperiments.keys())
        print(
            f"[rank 0] generated parent={parent}: coeffs={len(coefficients)}, "
            f"streams={[(k, len(v)) for k, v in subexperiments.items()]}",
            flush=True,
        )
        for child_label in child_labels:
            tasks.append({
                "parent_label": parent,
                "child_label": child_label,
                "experiments": subexperiments[child_label],
                "coefficients": coefficients,
                "subobservables_for_parent": data["subobservables"],
                "parent_child_labels": child_labels,
                "parent_overhead": sampling_overhead(data["bases"]),
                "num_parent_bases": len(data["bases"]),
            })
    # Stable order: L0,L1,R0,R1 if present.
    order = {"L0": 0, "L1": 1, "R0": 2, "R1": 3}
    return sorted(tasks, key=lambda t: order.get(t["child_label"], 999))


def execute_level_two_tasks(
    comm,
    tasks: list[dict[str, Any]] | None,
    *,
    shots: int,
    seed: int,
    optimization_level: int,
) -> list[dict[str, Any]] | None:
    rank = comm.Get_rank()
    tasks = comm.bcast(tasks, root=0)
    workers = (0, 1, 2, 3)

    assigned = []
    if rank in workers:
        wid = workers.index(rank)
        assigned = [task for i, task in enumerate(tasks) if i % len(workers) == wid]

    if not assigned:
        print(f"[rank {rank}] no level-2 stream assigned", flush=True)
        local_payloads = []
    else:
        backend = FakeMarrakesh()
        sampler = SamplerV2.from_backend(backend, default_shots=shots, seed=seed)
        pm = generate_preset_pass_manager(backend=backend, optimization_level=optimization_level)
        local_payloads = []
        for task in assigned:
            parent = task["parent_label"]
            child = task["child_label"]
            circuits = task["experiments"]
            print(f"[rank {rank}] executing {parent}->{child}: {len(circuits)} circuits", flush=True)
            t0 = time.perf_counter()
            isa = pm.run(circuits)
            t_transpile = time.perf_counter() - t0

            t0 = time.perf_counter()
            result = sampler.run(isa).result()
            t_execute = time.perf_counter() - t0

            local_payloads.append({
                "parent_label": parent,
                "child_label": child,
                "result": result,
                "coefficients": task["coefficients"],
                "subobservables_for_parent": task["subobservables_for_parent"],
                "parent_child_labels": task["parent_child_labels"],
                "metadata": {
                    "rank": rank,
                    "num_experiments": len(circuits),
                    "parent_overhead": task["parent_overhead"],
                    "num_parent_bases": task["num_parent_bases"],
                },
            })
            print(
                f"[rank {rank}] completed child {parent}->{child}: "
                f"transpile={t_transpile:.6f}s execute={t_execute:.6f}s",
                flush=True,
            )


    gathered = comm.gather(local_payloads, root=0)
    if rank == 0:
        archive = [payload for rank_payloads in gathered for payload in rank_payloads]
        print("[rank 0] gathered level-2 execution archive", flush=True)
        for p in archive:
            print(
                f"  parent={p['parent_label']} child={p['child_label']} "
                f"rank={p['metadata']['rank']} circuits={p['metadata']['num_experiments']}",
                flush=True,
            )
        return archive
    return None


def reconstruct_parent_from_archive(parent: str, archive: list[dict[str, Any]]) -> dict[str, Any]:
    parent_payloads = [p for p in archive if p["parent_label"] == parent]
    if not parent_payloads:
        raise RuntimeError(f"No payloads for parent={parent}")

    # All sibling streams under the same parent share coefficients/subobservables.
    coefficients = parent_payloads[0]["coefficients"]
    subobservables = parent_payloads[0]["subobservables_for_parent"]
    expected_children = set(parent_payloads[0]["parent_child_labels"])
    results = {p["child_label"]: p["result"] for p in parent_payloads}
    if set(results) != expected_children:
        raise RuntimeError(
            f"Parent {parent}: expected children={sorted(expected_children)}, got={sorted(results)}"
        )
    ordered_results = {label: results[label] for label in parent_payloads[0]["parent_child_labels"]}
    reconstructed = reconstruct_expectation_values(ordered_results, coefficients, subobservables)
    return {
        "parent_label": parent,
        "expectation_values": np.asarray(reconstructed, dtype=float),
        "coefficients": coefficients,
        "subobservables": subobservables,
    }


def reconstruct_level_two_mpi(comm, archive: list[dict[str, Any]] | None) -> dict[str, Any] | None:
    rank = comm.Get_rank()
    archive = comm.bcast(archive, root=0)

    local = None
    if rank == 0:
        local = reconstruct_parent_from_archive("L", archive)
        print("[rank 0] reconstructed parent L from L0,L1", flush=True)
    elif rank == 1:
        local = reconstruct_parent_from_archive("R", archive)
        print("[rank 1] reconstructed parent R from R0,R1", flush=True)

    gathered = comm.gather(local, root=0)
    if rank == 0:
        parents = {item["parent_label"]: item for item in gathered if item is not None}
        return parents
    return None


def final_hierarchical_combine(parent_results: dict[str, Any]) -> np.ndarray:
    """Final explicit L/R combination from reconstructed level-2 parent vectors."""
    L = parent_results["L"]["expectation_values"]
    R = parent_results["R"]["expectation_values"]
    if L.shape != R.shape:
        raise RuntimeError(f"Parent shapes differ: L={L.shape}, R={R.shape}")
    return L * R


def verify_against_uncut_circuit(circuit: QuantumCircuit, observables: PauliList, reconstructed_expvals: np.ndarray) -> None:
    print("[rank 0] verification against original uncut circuit:", flush=True)
    estimator = EstimatorV2()
    for observable, reconstructed_expval in zip(observables, reconstructed_expvals):
        exact_expval = estimator.run([(circuit, observable)]).result()[0].data.evs
        exact_expval = np.asarray(exact_expval).item()
        print(f"Observable: {observable}", flush=True)
        print(f"Reconstructed expectation value: {np.real(np.round(reconstructed_expval, 8))}", flush=True)
        print(f"Exact expectation value: {np.round(exact_expval, 8)}", flush=True)
        print(f"Error in estimation: {np.real(np.round(reconstructed_expval - exact_expval, 8))}", flush=True)
        if np.isclose(exact_expval, 0.0):
            print("Relative error in estimation: undefined because exact expectation value is approximately zero", flush=True)
        else:
            print(
                f"Relative error in estimation: {np.real(np.round((reconstructed_expval - exact_expval) / exact_expval, 8))}",
                flush=True,
            )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--num-samples", type=parse_num_samples, default=100)
    parser.add_argument("--shots", type=int, default=4096)
    parser.add_argument("--seed", type=int, default=1234)
    parser.add_argument("--optimization-level", type=int, default=1)
    args = parser.parse_args()

    comm = MPI.COMM_WORLD
    rank = comm.Get_rank()
    size = comm.Get_size()
    if size < 4:
        if rank == 0:
            raise RuntimeError("Use at least 4 MPI ranks, e.g. mpiexec -n 4 python poster_mpi_4_hierarchical.py")
        return

    if rank == 0:
        circuit = make_16q_cutting_circuit()
        observables = make_observables()
        print(f"[rank 0] circuit qubits={circuit.num_qubits}, depth={circuit.depth()}", flush=True)
        print(
            "[rank 0] NOTE: final combination is explicit hierarchical L*R, "
            "not a second call to qiskit's reconstruct_expectation_values().",
            flush=True,
        )
    else:
        circuit = None
        observables = None
    circuit = comm.bcast(circuit, root=0)
    observables = comm.bcast(observables, root=0)

    t_s = time.time()
    first_payload = build_first_level(comm, circuit, observables)
    t_e = time.time()
    print(f"Rank {rank}: build_first_level() elapsed time (s): {t_e-t_s}")

    t_s = time.time()
    second_level = build_second_level(comm, first_payload)
    t_e = time.time()
    print(f"Rank {rank}: build_second_level() elapsed time (s): {t_e-t_s}")

    if rank == 0:
        t_s = time.time()
        tasks = build_execution_tasks(second_level, args.num_samples)
        t_e = time.time()
        print(f"Rank {rank}: build_execution_tasks() elapsed time (s): {t_e-t_s}")
    else:
        tasks = None

    t_s = time.time()
    archive = execute_level_two_tasks(
        comm,
        tasks,
        shots=args.shots,
        seed=args.seed,
        optimization_level=args.optimization_level,
    )
    t_e = time.time()
    print(f"Rank {rank}: execute_level_two_tasks() elapsed time (s): {t_e-t_s}")

    t_s = time.time()
    parent_results = reconstruct_level_two_mpi(comm, archive)
    t_e = time.time()
    print(f"Rank {rank}: reconstruct_level_two_mpi() elapsed time (s): {t_e-t_s}")

    if rank == 0:
        t_s = time.time()
        final_expvals = final_hierarchical_combine(parent_results)
        print("[rank 0] final hierarchical reconstructed expectation values:", flush=True)
        t_e = time.time()
        print(f"Rank {rank}: final_hierarchical_combine() elapsed time (s): {t_e-t_s}")
        for obs, value in zip(observables, final_expvals):
            print(f"  {obs}: {value}", flush=True)
        verify_against_uncut_circuit(circuit, observables, final_expvals)
        print("[rank 0] workflow complete", flush=True)


if __name__ == "__main__":
    t_s = time.time()
    main()
    t_e = time.time()
    print(f"main() elapsed time (s): {t_e-t_s}")
