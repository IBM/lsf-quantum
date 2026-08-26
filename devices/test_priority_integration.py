#!/usr/bin/env python3

import unittest

from jobstarter_qpu_selector import (
    QPU,
    select_qpu_from_dict,
)


class PriorityIntegrationTests(unittest.TestCase):

    def test_ibm_style_qpu_selection(self):
        qpus = [
            QPU(
                "ibm_alpha",
                {
                    "qubits": 127,
                    "readout_error_median": 0.008,
                    "pending_jobs": 20,
                },
            ),
            QPU(
                "ibm_beta",
                {
                    "qubits": 156,
                    "readout_error_median": 0.009,
                    "pending_jobs": 10,
                },
            ),
            QPU(
                "ibm_gamma",
                {
                    "qubits": 156,
                    "readout_error_median": 0.005,
                    "pending_jobs": 8,
                },
            ),
        ]

        requirements = {
            "qubits": {
                "operator": ">=",
                "value": 127,
                "direction": "max",
            },
            "readout_error_median": {
                "operator": "<=",
                "value": 0.01,
                "direction": "min",
            },
            "pending_jobs": {
                "operator": "<=",
                "value": 100,
                "direction": "min",
            },
        }

        result = select_qpu_from_dict(
            qpus,
            requirements,
        )

        self.assertIsNotNone(result.selected)
        self.assertEqual(
            result.selected.name,
            "ibm_gamma",
        )


if __name__ == "__main__":
    unittest.main()
