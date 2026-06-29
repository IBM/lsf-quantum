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

"""Three-level hierarchical MPI/Qiskit circuit cutting prototype.

Goal
----
Compare a two-level hierarchical MPI strategy with a three-level strategy.
This version uses:

Level 1:
    Original 16-qubit circuit -> L | R

Level 2:
    L -> L0 | L1
    R -> R0 | R1

Level 3:
    L0 -> L00 | L01
    L1 -> L10 | L11
    R0 -> R00 | R01
    R1 -> R10 | R11

Execution:
    Eight 2-qubit leaf streams are executed by MPI ranks 0..7.

Reconstruction:
    1. Ranks 0..3 reconstruct 4-qubit parents from leaf pairs:
          L00,L01 -> L0
          L10,L11 -> L1
          R00,R01 -> R0
          R10,R11 -> R1
       using qiskit_addon_cutting.reconstruct_expectation_values().

    2. Ranks 0..1 combine 4-qubit parent vectors into 8-qubit parent vectors:
          L0,L1 -> L
          R0,R1 -> R
       using an explicit observable-wise product.

    3. Rank 0 combines L and R into the final reconstructed vector:
          L,R -> final
       using an explicit observable-wise product.

Scientific note
---------------
Qiskit's reconstruct_expectation_values() returns numerical expectation values,
not a PrimitiveResult-like object.  Therefore, after the lowest reconstruction
level, the upper-level reconstruction is explicit and vector-valued.  This file is
intended as a performance-comparison scaffold for the three-level decomposition.
The verification block compares the final vector with an ideal EstimatorV2 result
on the original uncut circuit.

Run:
    mpiexec -n 8 python poster_mpi_8_hierarchical_3level.py --num-samples 100 --shots 4096

Smoke test:
    mpiexec -n 8 python poster_mpi_8_hierarchical_3level.py --num-samples 8 --shots 512
"""
from __future__ import annotations

import argparse
import time
from typing import Any

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


LEAF_ORDER = ["L00", "L01", "L10", "L11", "R00", "R01", "R10", "R11"]
LEVEL3_PARENT_ORDER = ["L0", "L1", "R0", "R1"]


# -----------------------------------------------------------------------------
# Circuit construction
# -----------------------------------------------------------------------------


def make_16q_cutting_circuit(theta: float = 0.37) -> QuantumCircuit:
    """Create the same 16-qubit circuit used in the two-level MPI prototype."""
    qc = QuantumCircuit(16, name="hierarchical_16q_3level_test")

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

    # Level-2 and level-1 boundaries.
    qc.cx(3, 4)       # L0 -- L1
    qc.cx(7, 8)       # L -- R
    qc.cx(11, 12)     # R0 -- R1

    for a, b, c, d in blocks:
        qc.rx(0.2 + theta, a)
        qc.ry(0.4 + theta, b)
        qc.rz(0.6 + theta, c)
        qc.rx(0.8 + theta, d)
        qc.cx(a, c)   # inside 4q block, crosses level-3 2q split
        qc.cx(b, d)   # inside 4q block, crosses level-3 2q split

    qc.cx(1, 6)       # L0 -- L1
    qc.cx(5, 10)      # L -- R
    qc.cx(9, 14)      # R0 -- R1
    qc.cx(2, 13)      # L -- R

    for q in range(16):
        qc.rz(0.11 * (q + 1), q)
        qc.ry(0.07 * (16 - q), q)

    return qc


def make_observables() -> PauliList:
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


# -----------------------------------------------------------------------------
# Labels and utilities
# -----------------------------------------------------------------------------


def parse_num_samples(value: str) -> int:
    if value.lower() in {"inf", "infinity", "np.inf"}:
        raise argparse.ArgumentTypeError(
            "Use finite num_samples for the three-level performance comparison."
        )
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("num_samples must be >= 1")
    return parsed


def sampling_overhead(bases) -> float:
    return 1.0 if not bases else float(np.prod([basis.overhead for basis in bases]))


def first_level_labels() -> list[str]:
    return ["L"] * 8 + ["R"] * 8


def second_level_labels(parent: str) -> list[str]:
    if parent == "L":
        return ["L0"] * 4 + ["L1"] * 4
    if parent == "R":
        return ["R0"] * 4 + ["R1"] * 4
    raise ValueError(f"Unknown level-1 parent: {parent}")


def third_level_labels(parent4q: str) -> list[str]:
    mapping = {
        "L0": ["L00"] * 2 + ["L01"] * 2,
        "L1": ["L10"] * 2 + ["L11"] * 2,
        "R0": ["R00"] * 2 + ["R01"] * 2,
        "R1": ["R10"] * 2 + ["R11"] * 2,
    }
    return mapping[parent4q]


def level3_parent_of_leaf(leaf: str) -> str:
    return {"L00": "L0", "L01": "L0", "L10": "L1", "L11": "L1", "R00": "R0", "R01": "R0", "R10": "R1", "R11": "R1"}[leaf]


def level2_parent_of_node(node: str) -> str:
    return {"L0": "L", "L1": "L", "R0": "R", "R1": "R"}[node]


# -----------------------------------------------------------------------------
# Partitioning stages
# -----------------------------------------------------------------------------


def build_first_level(comm, circuit: QuantumCircuit, observables: PauliList) -> dict[str, Any]:
    rank = comm.Get_rank()
    if rank == 0:
        t0 = time.perf_counter()
        problem = partition_problem(circuit, first_level_labels(), observables)
        elapsed = time.perf_counter() - t0
        payload = {
            "subcircuits": problem.subcircuits,
            "subobservables": problem.subobservables,
            "bases": problem.bases,
            "timing": {"partition_level1_seconds": elapsed},
        }
        print(
            f"[rank 0] level 1 L|R: cuts={len(problem.bases)}, "
            f"overhead={sampling_overhead(problem.bases)}, time={elapsed:.6f}s",
            flush=True,
        )
    else:
        payload = None
    return comm.bcast(payload, root=0)


def build_second_level(comm, first_payload: dict[str, Any]) -> dict[str, Any] | None:
    rank = comm.Get_rank()
    local = None

    if rank == 0:
        parent = "L"
        t0 = time.perf_counter()
        problem = partition_problem(
            first_payload["subcircuits"][parent],
            second_level_labels(parent),
            first_payload["subobservables"][parent],
        )
        elapsed = time.perf_counter() - t0
        local = (parent, problem, elapsed)
        print(
            f"[rank 0] level 2 L0|L1: cuts={len(problem.bases)}, "
            f"overhead={sampling_overhead(problem.bases)}, time={elapsed:.6f}s",
            flush=True,
        )
    elif rank == 1:
        parent = "R"
        t0 = time.perf_counter()
        problem = partition_problem(
            first_payload["subcircuits"][parent],
            second_level_labels(parent),
            first_payload["subobservables"][parent],
        )
        elapsed = time.perf_counter() - t0
        local = (parent, problem, elapsed)
        print(
            f"[rank 1] level 2 R0|R1: cuts={len(problem.bases)}, "
            f"overhead={sampling_overhead(problem.bases)}, time={elapsed:.6f}s",
            flush=True,
        )

    gathered = comm.gather(local, root=0)
    if rank == 0:
        second = {}
        for item in gathered:
            if item is None:
                continue
            parent, problem, elapsed = item
            second[parent] = {
                "subcircuits": problem.subcircuits,
                "subobservables": problem.subobservables,
                "bases": problem.bases,
                "timing": {"partition_level2_seconds": elapsed},
            }
        return second
    return None


def build_third_level(comm, second_level_root: dict[str, Any] | None) -> dict[str, Any] | None:
    """Partition each 4-qubit node into two 2-qubit leaves using ranks 0..3."""
    rank = comm.Get_rank()
    second_level = comm.bcast(second_level_root, root=0)

    local = None
    if rank < 4:
        node = LEVEL3_PARENT_ORDER[rank]
        parent8 = level2_parent_of_node(node)
        t0 = time.perf_counter()
        problem = partition_problem(
            second_level[parent8]["subcircuits"][node],
            third_level_labels(node),
            second_level[parent8]["subobservables"][node],
        )
        elapsed = time.perf_counter() - t0
        local = (node, problem, elapsed)
        print(
            f"[rank {rank}] level 3 {node}: children={list(problem.subcircuits.keys())}, "
            f"cuts={len(problem.bases)}, overhead={sampling_overhead(problem.bases)}, "
            f"time={elapsed:.6f}s",
            flush=True,
        )

    gathered = comm.gather(local, root=0)
    if rank == 0:
        third = {}
        for item in gathered:
            if item is None:
                continue
            node, problem, elapsed = item
            third[node] = {
                "subcircuits": problem.subcircuits,
                "subobservables": problem.subobservables,
                "bases": problem.bases,
                "children": list(problem.subcircuits.keys()),
                "timing": {"partition_level3_seconds": elapsed},
            }
        return third
    return None


# -----------------------------------------------------------------------------
# Execution tasks for eight 2-qubit leaf streams
# -----------------------------------------------------------------------------


def build_leaf_execution_tasks(third_level: dict[str, Any], num_samples: int) -> list[dict[str, Any]]:
    tasks = []
    for node in LEVEL3_PARENT_ORDER:
        data = third_level[node]
        t0 = time.perf_counter()
        subexperiments, coefficients = generate_cutting_experiments(
            circuits=data["subcircuits"],
            observables=data["subobservables"],
            num_samples=num_samples,
        )
        elapsed = time.perf_counter() - t0
        print(
            f"[rank 0] generated level-3 experiments for {node}: "
            f"coeffs={len(coefficients)}, streams={[(k, len(v)) for k, v in subexperiments.items()]}, "
            f"time={elapsed:.6f}s",
            flush=True,
        )
        for leaf in data["children"]:
            tasks.append(
                {
                    "level3_parent": node,
                    "leaf_label": leaf,
                    "experiments": subexperiments[leaf],
                    "coefficients": coefficients,
                    "subobservables_for_parent": data["subobservables"],
                    "parent_children": data["children"],
                    "num_parent_bases": len(data["bases"]),
                    "parent_overhead": sampling_overhead(data["bases"]),
                }
            )

    return sorted(tasks, key=lambda t: LEAF_ORDER.index(t["leaf_label"]))


def execute_leaf_tasks(
    comm,
    tasks_root: list[dict[str, Any]] | None,
    *,
    shots: int,
    seed: int,
    optimization_level: int,
) -> list[dict[str, Any]] | None:
    rank = comm.Get_rank()
    tasks = comm.bcast(tasks_root, root=0)

    if rank < 8:
        assigned = [task for i, task in enumerate(tasks) if i % 8 == rank]
    else:
        assigned = []

    if not assigned:
        print(f"[rank {rank}] no leaf stream assigned", flush=True)
        local_payloads = []
    else:
        backend = FakeMarrakesh()
        sampler = SamplerV2.from_backend(backend, default_shots=shots, seed=seed)
        pass_manager = generate_preset_pass_manager(
            backend=backend,
            optimization_level=optimization_level,
        )
        local_payloads = []

        for task in assigned:
            leaf = task["leaf_label"]
            parent = task["level3_parent"]
            circuits = task["experiments"]
            print(
                f"[rank {rank}] executing leaf {parent}->{leaf}: {len(circuits)} circuits",
                flush=True,
            )

            t0 = time.perf_counter()
            isa = pass_manager.run(circuits)
            t_transpile = time.perf_counter() - t0

            t0 = time.perf_counter()
            result = sampler.run(isa).result()
            t_execute = time.perf_counter() - t0

            local_payloads.append(
                {
                    "level3_parent": parent,
                    "leaf_label": leaf,
                    "result": result,
                    "coefficients": task["coefficients"],
                    "subobservables_for_parent": task["subobservables_for_parent"],
                    "parent_children": task["parent_children"],
                    "metadata": {
                        "rank": rank,
                        "num_experiments": len(circuits),
                        "transpile_seconds": t_transpile,
                        "execute_seconds": t_execute,
                        "num_parent_bases": task["num_parent_bases"],
                        "parent_overhead": task["parent_overhead"],
                    },
                }
            )
            print(
                f"[rank {rank}] completed leaf {parent}->{leaf}: "
                f"transpile={t_transpile:.6f}s execute={t_execute:.6f}s",
                flush=True,
            )

    gathered = comm.gather(local_payloads, root=0)
    if rank == 0:
        archive = [payload for rank_payloads in gathered for payload in rank_payloads]
        print("[rank 0] gathered leaf execution archive", flush=True)
        for payload in archive:
            md = payload["metadata"]
            print(
                f"  parent={payload['level3_parent']} leaf={payload['leaf_label']} "
                f"rank={md['rank']} circuits={md['num_experiments']}",
                flush=True,
            )
        return archive
    return None


# -----------------------------------------------------------------------------
# Reconstruction
# -----------------------------------------------------------------------------


def reconstruct_level3_parent_from_archive(parent: str, archive: list[dict[str, Any]]) -> dict[str, Any]:
    payloads = [payload for payload in archive if payload["level3_parent"] == parent]
    if not payloads:
        raise RuntimeError(f"No leaf payloads for level-3 parent {parent}")

    coefficients = payloads[0]["coefficients"]
    subobservables = payloads[0]["subobservables_for_parent"]
    children = payloads[0]["parent_children"]
    results = {payload["leaf_label"]: payload["result"] for payload in payloads}

    if set(results) != set(children):
        raise RuntimeError(
            f"Parent {parent}: expected children={children}, got={sorted(results)}"
        )

    ordered_results = {child: results[child] for child in children}
    reconstructed = reconstruct_expectation_values(ordered_results, coefficients, subobservables)

    return {
        "node": parent,
        "expectation_values": np.asarray(reconstructed, dtype=float),
        "children": children,
    }


def reconstruct_level3_mpi(comm, leaf_archive_root: list[dict[str, Any]] | None) -> dict[str, Any] | None:
    """Ranks 0..3 reconstruct L0, L1, R0, R1 from their two leaves."""
    rank = comm.Get_rank()
    archive = comm.bcast(leaf_archive_root, root=0)

    local = None
    if rank < 4:
        node = LEVEL3_PARENT_ORDER[rank]
        t0 = time.perf_counter()
        local = reconstruct_level3_parent_from_archive(node, archive)
        local["reconstruct_seconds"] = time.perf_counter() - t0
        print(
            f"[rank {rank}] reconstructed {node} from leaves {local['children']} "
            f"in {local['reconstruct_seconds']:.6f}s",
            flush=True,
        )

    gathered = comm.gather(local, root=0)
    if rank == 0:
        return {item["node"]: item for item in gathered if item is not None}
    return None


def combine_vectors(label: str, left: np.ndarray, right: np.ndarray) -> np.ndarray:
    if left.shape != right.shape:
        raise RuntimeError(f"Cannot combine {label}: shapes differ {left.shape} vs {right.shape}")
    return left * right


def reconstruct_level2_and_final_mpi(comm, level3_results_root: dict[str, Any] | None) -> dict[str, Any] | None:
    """Ranks 0..1 combine L0/L1 and R0/R1. Rank 0 combines L/R."""
    rank = comm.Get_rank()
    level3_results = comm.bcast(level3_results_root, root=0)

    local = None
    if rank == 0:
        t0 = time.perf_counter()
        L = combine_vectors(
            "L=L0*L1",
            level3_results["L0"]["expectation_values"],
            level3_results["L1"]["expectation_values"],
        )
        local = {"node": "L", "expectation_values": L, "combine_seconds": time.perf_counter() - t0}
        print(f"[rank 0] combined L0,L1 -> L in {local['combine_seconds']:.6f}s", flush=True)
    elif rank == 1:
        t0 = time.perf_counter()
        R = combine_vectors(
            "R=R0*R1",
            level3_results["R0"]["expectation_values"],
            level3_results["R1"]["expectation_values"],
        )
        local = {"node": "R", "expectation_values": R, "combine_seconds": time.perf_counter() - t0}
        print(f"[rank 1] combined R0,R1 -> R in {local['combine_seconds']:.6f}s", flush=True)

    gathered = comm.gather(local, root=0)
    if rank == 0:
        level2_results = {item["node"]: item for item in gathered if item is not None}
        t0 = time.perf_counter()
        final = combine_vectors(
            "final=L*R",
            level2_results["L"]["expectation_values"],
            level2_results["R"]["expectation_values"],
        )
        final_seconds = time.perf_counter() - t0
        print(f"[rank 0] combined L,R -> final in {final_seconds:.6f}s", flush=True)
        return {
            "level2_results": level2_results,
            "final_expectation_values": final,
            "final_combine_seconds": final_seconds,
        }
    return None


# -----------------------------------------------------------------------------
# Verification
# -----------------------------------------------------------------------------


def verify_against_uncut_circuit(circuit: QuantumCircuit, observables: PauliList, reconstructed_expvals: np.ndarray) -> float:
    print("[rank 0] verification against original uncut circuit:", flush=True)
    estimator = EstimatorV2()
    t0 = time.perf_counter()

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

    return time.perf_counter() - t0


# -----------------------------------------------------------------------------
# Main workflow
# -----------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description="Three-level hierarchical MPI/Qiskit circuit cutting prototype.")
    parser.add_argument("--num-samples", type=parse_num_samples, default=100)
    parser.add_argument("--shots", type=int, default=4096)
    parser.add_argument("--seed", type=int, default=1234)
    parser.add_argument("--optimization-level", type=int, default=1)
    args = parser.parse_args()

    comm = MPI.COMM_WORLD
    rank = comm.Get_rank()
    size = comm.Get_size()

    if size < 8:
        if rank == 0:
            raise RuntimeError(
                "This three-level version expects at least 8 MPI ranks: "
                "mpiexec -n 8 python poster_mpi_8_hierarchical_3level.py"
            )
        return

    total_start = time.perf_counter()

    if rank == 0:
        circuit = make_16q_cutting_circuit()
        observables = make_observables()
        print(f"[rank 0] circuit qubits={circuit.num_qubits}, depth={circuit.depth()}", flush=True)
        print(f"[rank 0] num_samples={args.num_samples}, shots={args.shots}", flush=True)
        print(
            "[rank 0] NOTE: level-3 leaf reconstruction uses Qiskit; upper-level "
            "combines are explicit observable-wise products.",
            flush=True,
        )
    else:
        circuit = None
        observables = None

    circuit = comm.bcast(circuit, root=0)
    observables = comm.bcast(observables, root=0)

    first_payload = build_first_level(comm, circuit, observables)
    second_level_root = build_second_level(comm, first_payload)
    third_level_root = build_third_level(comm, second_level_root)

    if rank == 0:
        leaf_tasks = build_leaf_execution_tasks(third_level_root, args.num_samples)
    else:
        leaf_tasks = None

    leaf_archive_root = execute_leaf_tasks(
        comm,
        leaf_tasks,
        shots=args.shots,
        seed=args.seed,
        optimization_level=args.optimization_level,
    )

    level3_results_root = reconstruct_level3_mpi(comm, leaf_archive_root)
    final_result_root = reconstruct_level2_and_final_mpi(comm, level3_results_root)

    if rank == 0:
        final_expvals = final_result_root["final_expectation_values"]
        print("[rank 0] final three-level hierarchical reconstructed expectation values:", flush=True)
        for obs, value in zip(observables, final_expvals):
            print(f"  {obs}: {value}", flush=True)

        verify_seconds = verify_against_uncut_circuit(circuit, observables, final_expvals)
        total_seconds = time.perf_counter() - total_start

        print("[rank 0] timing summary:", flush=True)
        print(f"  verify_seconds: {verify_seconds:.6f}", flush=True)
        print(f"  total_wall_seconds_rank0: {total_seconds:.6f}", flush=True)
        print("[rank 0] three-level workflow complete", flush=True)


if __name__ == "__main__":
    main()
