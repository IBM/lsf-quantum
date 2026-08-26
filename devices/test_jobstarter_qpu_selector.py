#!/usr/bin/env python3
# -*- coding: utf-8 -*-
# (C) Copyright 2025-2026 IBM. All Rights Reserved.
#
# This code is licensed under the Apache License, Version 2.0. You may
# obtain a copy of this license in the LICENSE.txt file in the root directory
# of this source tree or at http://www.apache.org/licenses/LICENSE-2.0.
#
# Any modifications or derivative works of this code must retain this
# copyright notice, and modified files need to carry a notice indicating
# that they have been altered from the originals.

import unittest

from jobstarter_qpu_selector import (
    Direction,
    QPU,
    Requirement,
    example,
    requirements_from_dict,
    select_qpu,
    select_qpu_from_dict,
)


class SelectorTests(unittest.TestCase):
    def setUp(self):
        self.qpus = [
            QPU("alpha", {"qubits": 127, "readout_error_median": 0.010, "pending_jobs": 3}),
            QPU("beta", {"qubits": 156, "readout_error_median": 0.020, "pending_jobs": 2}),
            QPU("gamma", {"qubits": 156, "readout_error_median": 0.005, "pending_jobs": 8}),
        ]

    def test_priority_order_and_tie_break(self):
        result = select_qpu(self.qpus, [
            Requirement("qubits", 1, ">=", 127, Direction.MAX),
            Requirement("readout_error_median", 2, "<=", 0.03, Direction.MIN),
        ])
        self.assertEqual(result.selected.name, "gamma")

    def test_no_feasible_qpu(self):
        result = select_qpu(self.qpus, [Requirement("qubits", 1, ">=", 1000)])
        self.assertIsNone(result.selected)
        self.assertEqual(result.tied_best, ())

    def test_ranking_only(self):
        result = select_qpu(self.qpus, [
            Requirement("pending_jobs", 1, direction=Direction.MIN)
        ])
        self.assertEqual(result.selected.name, "beta")

    def test_name_tie_break_is_deterministic(self):
        qpus = [QPU("z", {"qubits": 5}), QPU("a", {"qubits": 5})]
        result = select_qpu(qpus, [Requirement("qubits", 1)])
        self.assertEqual(result.selected.name, "a")
        self.assertEqual([q.name for q in result.tied_best], ["a", "z"])

    def test_example_reads_requirements_dictionary(self):
        result = example([
            "--requirements",
            '{"qubits":{"operator":">=","value":127,"direction":"max"},'
            '"readout_error_median":{"operator":"<=","value":0.01,"direction":"min"}}',
        ])
        self.assertEqual(result.selected.name, "qpu-c")

    def test_dictionary_values_retain_json_types(self):
        result = example([
            "--requirements",
            '{"pending_jobs":{"operator":"<=","value":20,"direction":"min"}}',
        ])
        self.assertEqual(result.selected.name, "qpu-b")

    def test_requirement_dictionary_must_not_be_empty(self):
        with self.assertRaisesRegex(ValueError, "non-empty JSON dictionary"):
            example(["--requirements", "{}"])

    def test_requirement_dictionary_rejects_missing_fields(self):
        with self.assertRaisesRegex(ValueError, "missing fields"):
            example([
                "--requirements",
                '{"qubits":{"operator":">=","value":127}}',
            ])


    def test_requirements_from_dictionary_preserves_priority(self):
        requirements = requirements_from_dict({
            "qubits": {
                "operator": ">=",
                "value": 127,
                "direction": "max",
            },
            "readout_error_median": {
                "operator": "<=",
                "value": 0.03,
                "direction": "min",
            },
        })

        self.assertEqual(requirements[0].attribute, "qubits")
        self.assertEqual(requirements[0].priority, 1)
        self.assertEqual(
            requirements[1].attribute,
            "readout_error_median",
        )
        self.assertEqual(requirements[1].priority, 2)

    def test_select_qpu_from_dictionary(self):
        result = select_qpu_from_dict(
            self.qpus,
            {
                "qubits": {
                    "operator": ">=",
                    "value": 127,
                    "direction": "max",
                },
                "readout_error_median": {
                    "operator": "<=",
                    "value": 0.03,
                    "direction": "min",
                },
            },
        )

        self.assertEqual(result.selected.name, "gamma")


if __name__ == "__main__":
    unittest.main()
