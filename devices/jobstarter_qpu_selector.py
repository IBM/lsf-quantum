"""Resource-aware QPU selection inspired by the jobstarter.qpu algorithm.

The paper describes an iterative algorithm that:
1. represents available QPUs as rows of an attribute matrix;
2. extracts and prioritises resource requirements;
3. sorts by the highest-priority requested attribute;
4. keeps the best subset satisfying that requirement;
5. repeats for each remaining requested attribute; and
6. terminates when no candidates remain or all requirements are processed.

This implementation makes two details explicit that are implicit/ambiguous in the
paper: each requirement has (a) a hard predicate and (b) a preference direction.
Hard predicates determine feasibility; preference directions rank feasible QPUs.
Ties are retained at each stage, so lower-priority attributes can break them.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from math import isclose
from typing import Any, Callable, Iterable, Mapping, Sequence


class Direction(str, Enum):
    MAX = "max"
    MIN = "min"


Comparator = Callable[[Any, Any], bool]


def _eq(actual: Any, target: Any) -> bool:
    if isinstance(actual, (int, float)) and isinstance(target, (int, float)):
        return isclose(float(actual), float(target), rel_tol=1e-12, abs_tol=1e-15)
    return actual == target


COMPARATORS: dict[str, Comparator] = {
    ">=": lambda a, b: a >= b,
    "<=": lambda a, b: a <= b,
    "==": _eq,
    "!=": lambda a, b: not _eq(a, b),
    ">": lambda a, b: a > b,
    "<": lambda a, b: a < b,
    "in": lambda a, b: a in b,
}


@dataclass(frozen=True)
class QPU:
    """A quantum device and the properties returned by discovery/QRMI."""

    name: str
    attributes: Mapping[str, Any]

    def value(self, attribute: str) -> Any:
        if attribute not in self.attributes:
            raise KeyError(f"QPU {self.name!r} has no attribute {attribute!r}")
        return self.attributes[attribute]


@dataclass(frozen=True)
class Requirement:
    """One requested device attribute.

    priority: smaller numbers are processed first (submission order can be used).
    operator/value: hard feasibility condition. Set operator=None for ranking only.
    direction: which feasible value is preferred at this stage.
    """

    attribute: str
    priority: int
    operator: str | None = None
    value: Any = None
    direction: Direction = Direction.MAX

    def matches(self, qpu: QPU) -> bool:
        if self.operator is None:
            return True
        try:
            comparator = COMPARATORS[self.operator]
        except KeyError as exc:
            raise ValueError(
                f"Unsupported operator {self.operator!r}; "
                f"choose one of {sorted(COMPARATORS)}"
            ) from exc
        return comparator(qpu.value(self.attribute), self.value)


@dataclass(frozen=True)
class SelectionTrace:
    priority: int
    attribute: str
    candidates_before: tuple[str, ...]
    feasible: tuple[str, ...]
    best_value: Any | None
    candidates_after: tuple[str, ...]


@dataclass(frozen=True)
class SelectionResult:
    selected: QPU | None
    tied_best: tuple[QPU, ...]
    trace: tuple[SelectionTrace, ...]
    reason: str


def _same_value(a: Any, b: Any) -> bool:
    return _eq(a, b)


def select_qpu(
    qpus: Iterable[QPU],
    requirements: Sequence[Requirement],
    *,
    deterministic_tie_break: bool = True,
) -> SelectionResult:
    """Select the best QPU by priority-ordered constraint filtering.

    At every priority stage:
      * remove QPUs failing the hard predicate;
      * sort feasible QPUs in the requested direction;
      * retain every QPU tied at the best value.

    This is equivalent to lexicographic optimisation over requested attributes,
    while preserving the paper's repeated-sort-and-subset structure.
    """

    candidates = list(qpus)
    if not candidates:
        return SelectionResult(None, (), (), "No QPUs were supplied")

    names = [q.name for q in candidates]
    if len(names) != len(set(names)):
        raise ValueError("QPU names must be unique")

    ordered = sorted(enumerate(requirements), key=lambda x: (x[1].priority, x[0]))
    trace: list[SelectionTrace] = []

    for _, requirement in ordered:
        before = tuple(q.name for q in candidates)
        try:
            feasible = [q for q in candidates if requirement.matches(q)]
        except (KeyError, TypeError) as exc:
            raise ValueError(
                f"Cannot evaluate requirement for {requirement.attribute!r}: {exc}"
            ) from exc

        if not feasible:
            trace.append(
                SelectionTrace(
                    requirement.priority,
                    requirement.attribute,
                    before,
                    (),
                    None,
                    (),
                )
            )
            return SelectionResult(
                None,
                (),
                tuple(trace),
                f"No QPU satisfies {requirement.attribute} "
                f"{requirement.operator or '(ranking only)'} {requirement.value!r}",
            )

        reverse = requirement.direction == Direction.MAX
        try:
            feasible.sort(key=lambda q: q.value(requirement.attribute), reverse=reverse)
        except (KeyError, TypeError) as exc:
            raise ValueError(
                f"Attribute {requirement.attribute!r} is missing or not mutually comparable: {exc}"
            ) from exc

        best_value = feasible[0].value(requirement.attribute)
        candidates = [
            q for q in feasible if _same_value(q.value(requirement.attribute), best_value)
        ]
        trace.append(
            SelectionTrace(
                requirement.priority,
                requirement.attribute,
                before,
                tuple(q.name for q in feasible),
                best_value,
                tuple(q.name for q in candidates),
            )
        )

        if len(candidates) == 1:
            break

    tied = tuple(sorted(candidates, key=lambda q: q.name))
    selected = tied[0] if deterministic_tie_break and tied else (tied[0] if len(tied) == 1 else None)
    reason = (
        "Selected by priority-ordered filtering"
        if selected is not None and len(tied) == 1
        else "Attributes are tied; selected lexicographically by QPU name"
        if selected is not None
        else "Multiple QPUs remain tied"
    )
    return SelectionResult(selected, tied, tuple(trace), reason)


def qpu_from_qrmi(name: str, qrmi_properties: Mapping[str, Any]) -> QPU:
    """Small adapter for already-decoded QRMI/device-property dictionaries."""
    return QPU(name=name, attributes=dict(qrmi_properties))


def example() -> SelectionResult:
    qpus = [
        QPU("qpu-a", {"Qubits": 156, "ReadoutError": 8.30e-3, "PendingJobs": 73}),
        QPU("qpu-b", {"Qubits": 127, "ReadoutError": 3.10e-3, "PendingJobs": 12}),
        QPU("qpu-c", {"Qubits": 156, "ReadoutError": 4.20e-3, "PendingJobs": 21}),
    ]
    requirements = [
        Requirement("Qubits", priority=1, operator=">=", value=127, direction=Direction.MAX),
        Requirement("ReadoutError", priority=2, operator="<=", value=1.0e-2, direction=Direction.MIN),
        Requirement("PendingJobs", priority=3, operator=None, direction=Direction.MIN),
    ]
    return select_qpu(qpus, requirements)


if __name__ == "__main__":
    result = example()
    print("selected:", result.selected.name if result.selected else None)
    print("reason:", result.reason)
    for stage in result.trace:
        print(stage)
