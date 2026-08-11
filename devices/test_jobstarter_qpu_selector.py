import unittest

from jobstarter_qpu_selector import (
    Direction,
    QPU,
    Requirement,
    example,
    select_qpu,
)


class SelectorTests(unittest.TestCase):
    def setUp(self):
        self.qpus = [
            QPU("alpha", {"Qubits": 127, "ReadoutError": 0.010, "PendingJobs": 3}),
            QPU("beta", {"Qubits": 156, "ReadoutError": 0.020, "PendingJobs": 2}),
            QPU("gamma", {"Qubits": 156, "ReadoutError": 0.005, "PendingJobs": 8}),
        ]

    def test_priority_order_and_tie_break(self):
        result = select_qpu(self.qpus, [
            Requirement("Qubits", 1, ">=", 127, Direction.MAX),
            Requirement("ReadoutError", 2, "<=", 0.03, Direction.MIN),
        ])
        self.assertEqual(result.selected.name, "gamma")

    def test_no_feasible_qpu(self):
        result = select_qpu(self.qpus, [Requirement("Qubits", 1, ">=", 1000)])
        self.assertIsNone(result.selected)
        self.assertEqual(result.tied_best, ())

    def test_ranking_only(self):
        result = select_qpu(self.qpus, [
            Requirement("PendingJobs", 1, direction=Direction.MIN)
        ])
        self.assertEqual(result.selected.name, "beta")

    def test_name_tie_break_is_deterministic(self):
        qpus = [QPU("z", {"Qubits": 5}), QPU("a", {"Qubits": 5})]
        result = select_qpu(qpus, [Requirement("Qubits", 1)])
        self.assertEqual(result.selected.name, "a")
        self.assertEqual([q.name for q in result.tied_best], ["a", "z"])

    def test_example_reads_requirements_dictionary(self):
        result = example([
            "--requirements",
            '{"Qubits":{"operator":">=","value":127,"direction":"max"},'
            '"ReadoutError":{"operator":"<=","value":0.01,"direction":"min"}}',
        ])
        self.assertEqual(result.selected.name, "qpu-c")

    def test_dictionary_values_retain_json_types(self):
        result = example([
            "--requirements",
            '{"PendingJobs":{"operator":"<=","value":20,"direction":"min"}}',
        ])
        self.assertEqual(result.selected.name, "qpu-b")

    def test_requirement_dictionary_must_not_be_empty(self):
        with self.assertRaisesRegex(ValueError, "non-empty JSON dictionary"):
            example(["--requirements", "{}"])

    def test_requirement_dictionary_rejects_missing_fields(self):
        with self.assertRaisesRegex(ValueError, "missing fields"):
            example([
                "--requirements",
                '{"Qubits":{"operator":">=","value":127}}',
            ])


if __name__ == "__main__":
    unittest.main()
