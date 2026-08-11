## Jobstarter QPU selector

A dependency-free Python implementation of the `jobstarter.qpu` device-selection
procedure described in *Practical Example of Resources-Aware Scheduling of Hybrid
Quantum-Classical Workflows*.

## Jobstarter QPU selector

A dependency-free Python implementation of the `jobstarter.qpu` device-selection
procedure described in *Practical Example of Resources-Aware Scheduling of Hybrid
Quantum-Classical Workflows*.

## Run

```bash
python jobstarter_qpu_selector.py \
  --requirements '{"Qubits":{"operator":">=","value":127,"direction":"max"},"ReadoutError":{"operator":"<=","value":0.01,"direction":"min"},"PendingJobs":{"operator":"<=","value":100,"direction":"min"}}'

python -m unittest -v test_jobstarter_qpu_selector.py
```

## Requirements dictionary

Pass all requirements as one JSON dictionary using `--requirements`. Each key is a
QPU attribute. Its value must contain `operator`, `value`, and `direction`.
Dictionary insertion order defines priority, so the first key is processed first.
JSON parsing preserves number, Boolean, array, object, string, and null types.

Supported operators are `>=`, `<=`, `==`, `!=`, `>`, `<`, and `in`. Direction is
`max` or `min`.

## Semantics

At each stage, infeasible devices are removed and all devices tied at the best
feasible value are retained for the next stage. If every requested attribute ties,
QPU name is used as a deterministic final tie-break unless
`--no-deterministic-tie-break` is supplied.
