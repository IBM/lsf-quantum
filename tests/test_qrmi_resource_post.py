# SPDX-License-Identifier: Apache-2.0
# Tests for posting QRMI acquisition IDs to LSF jobs.
import ast
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[1] / "qrmi-esub-jobstarter.py"
TREE = ast.parse(SOURCE.read_text())


def load_nodes(nodes, namespace):
    module = ast.Module(body=nodes, type_ignores=[])
    exec(compile(module, str(SOURCE), "exec"), namespace)


def helper_namespace():
    namespace = {
        "os": os, "sys": sys, "json": json, "subprocess": subprocess,
    }
    helper = next(
        node for node in TREE.body
        if isinstance(node, ast.FunctionDef)
        and node.name == "post_qrmi_resource_id"
    )
    load_nodes([helper], namespace)
    return namespace


def acquisition_tail(nodes):
    for index, node in enumerate(nodes):
        if (
            isinstance(node, ast.Assign)
            and any(
                isinstance(target, ast.Name)
                and target.id == "acquisition_token"
                for target in node.targets
            )
        ):
            return nodes[index:]
    raise AssertionError("Acquisition assignment not found")


class PostingTests(unittest.TestCase):
    def test_job_and_array_messages(self):
        helper = helper_namespace()["post_qrmi_resource_id"]
        for index, expected in [
            (None, "123"), ("0", "123"), ("7", "123[7]"),
        ]:
            with self.subTest(index=index):
                environment = {"LSB_JOBID": "123"}
                if index is not None:
                    environment["LSB_JOBINDEX"] = index
                with patch.dict(os.environ, environment, clear=True):
                    with patch.object(
                        subprocess, "run",
                        return_value=subprocess.CompletedProcess([], 0),
                    ) as run:
                        helper("ibm_fez", "qiskit-runtime-service", "fake-id")
                args, kwargs = run.call_args
                command = args[0]
                self.assertEqual(command[:4], ["bpost", "-i", "1", "-d"])
                self.assertEqual(command[5], expected)
                self.assertEqual(json.loads(command[4]), {
                    "qrmi_resource": "ibm_fez",
                    "qrmi_resource_type": "qiskit-runtime-service",
                    "qrmi_acquisition_id": "fake-id",
                })
                self.assertEqual(kwargs["timeout"], 10)
                self.assertFalse(kwargs.get("shell", False))

    def test_invalid_job_environment_does_not_post(self):
        helper = helper_namespace()["post_qrmi_resource_id"]
        for environment in [
            {}, {"LSB_JOBID": "0"}, {"LSB_JOBID": "invalid"},
            {"LSB_JOBID": "123", "LSB_JOBINDEX": "invalid"},
        ]:
            with self.subTest(environment=environment):
                with patch.dict(os.environ, environment, clear=True):
                    with patch.object(subprocess, "run") as run:
                        with contextlib.redirect_stderr(io.StringIO()):
                            helper("ibm_fez", "qiskit-runtime-service", "fake-id")
                run.assert_not_called()

    def test_posting_failures_warn_without_exposing_details(self):
        helper = helper_namespace()["post_qrmi_resource_id"]
        for failure in [
            FileNotFoundError("secret-marker"),
            subprocess.TimeoutExpired(["secret-marker"], 10),
            OSError("secret-marker"),
            subprocess.CompletedProcess([], 1),
        ]:
            with self.subTest(failure=type(failure).__name__):
                kwargs = (
                    {"side_effect": failure}
                    if isinstance(failure, Exception)
                    else {"return_value": failure}
                )
                output = io.StringIO()
                with patch.dict(os.environ, {"LSB_JOBID": "123"}, clear=True):
                    with patch.object(subprocess, "run", **kwargs):
                        with contextlib.redirect_stderr(output):
                            helper("ibm_fez", "qiskit-runtime-service", "fake-id")
                self.assertIn("[WARNING]", output.getvalue())
                self.assertNotIn("secret-marker", output.getvalue())

    def test_both_acquisition_paths(self):
        explicit = next(
            node for node in TREE.body
            if isinstance(node, ast.If)
            and ast.unparse(node.test) == "device is not None"
        )
        paths = {
            "explicit": acquisition_tail(explicit.body),
            "automatic": acquisition_tail(TREE.body),
        }
        for path, nodes in paths.items():
            for scenario in [
                "success", "post_failure", "environment_failure",
                "application_failure", "acquisition_failure",
            ]:
                with self.subTest(path=path, scenario=scenario):
                    events = []

                    def acquire(*args):
                        events.append("acquire")
                        if scenario == "acquisition_failure":
                            raise RuntimeError("acquire failed")
                        return "fake-id"

                    def environment(*args):
                        events.append("environment")
                        if scenario == "environment_failure":
                            raise RuntimeError("environment failed")
                        return {}

                    def run(command, **kwargs):
                        if command[0] == "bpost":
                            events.append("post")
                            if scenario == "post_failure":
                                raise FileNotFoundError()
                        else:
                            events.append("application")
                            if scenario == "application_failure":
                                raise RuntimeError("application failed")
                        return subprocess.CompletedProcess(command, 0)

                    namespace = helper_namespace()
                    release = Mock(
                        side_effect=lambda *args: events.append("release")
                    )
                    namespace.update({
                        "device": "ibm_fez",
                        "best_device": "ibm_fez",
                        "qpu_type": "qiskit-runtime-service",
                        "resource": object(),
                        "env_before_enumeration": {},
                        "job_args": ["fake-application"],
                        "acquire_quantum_resource": acquire,
                        "release_quantum_resource": release,
                        "job_environment": environment,
                    })
                    expected_exception = (
                        SystemExit if scenario in {"success", "post_failure"}
                        else RuntimeError
                    )
                    with patch.dict(os.environ, {"LSB_JOBID": "123"}, clear=True):
                        with patch.object(subprocess, "run", side_effect=run):
                            with contextlib.redirect_stderr(io.StringIO()):
                                with self.assertRaises(expected_exception):
                                    load_nodes(nodes, namespace)

                    if scenario == "acquisition_failure":
                        self.assertEqual(events, ["acquire"])
                        release.assert_not_called()
                    else:
                        release.assert_called_once_with(
                            namespace["resource"], "ibm_fez", "fake-id"
                        )
                        self.assertEqual(events[-1], "release")
                        self.assertEqual(events[:3], [
                            "acquire", "post", "environment",
                        ])
                        self.assertEqual(
                            "application" in events,
                            scenario != "environment_failure",
                        )


if __name__ == "__main__":
    unittest.main()
