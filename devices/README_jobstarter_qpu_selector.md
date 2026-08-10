# Ongoing work on quantum devices selection algorithms

## Jobstarter QPU selector

A dependency-free Python implementation of the `jobstarter.qpu` device-selection
procedure described in *Practical Example of Resources-Aware Scheduling of Hybrid
Quantum-Classical Workflows*.

## Run

```bash
python jobstarter_qpu_selector.py
python -m unittest -v test_jobstarter_qpu_selector.py
```

## Semantics

Requirements are processed by ascending `priority`. Each requirement can contain:

- a hard predicate (`>=`, `<=`, `==`, `!=`, `>`, `<`, or `in`); and
- a preference direction (`Direction.MAX` or `Direction.MIN`).

At a stage, infeasible devices are removed and all devices tied at the best feasible
value are retained for the next stage. This makes lower-priority attributes useful as
tie-breakers. If every requested attribute ties, QPU name is used as a deterministic
final tie-break unless `deterministic_tie_break=False`.

The paper states that attributes are sorted in descending order. Real device metrics
have different desirability directions, however: more qubits is usually maximised,
while readout error and pending jobs are usually minimised. The explicit `direction`
field avoids silently treating every larger value as better.
