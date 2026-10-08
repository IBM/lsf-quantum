# SPDX-License-Identifier: Apache-2.0
"""Regression tests for application exit status in both jobstarter paths."""
import ast
import contextlib
import io
import os
import signal
import subprocess
import sys
import unittest
from unittest.mock import Mock, patch

from test_qrmi_resource_post import TREE, acquisition_tail, load_nodes


class JobstarterExitStatusTests(unittest.TestCase):
    def paths(self):
        explicit = next(
            node for node in TREE.body
            if isinstance(node, ast.If)
            and ast.unparse(node.test) == "device is not None"
        )
        return {
            "explicit": acquisition_tail(explicit.body),
            "automatic": acquisition_tail(TREE.body),
        }

    def namespace(self, command, resource):
        namespace = {
            "os": os,
            "sys": sys,
            "subprocess": subprocess,
            "device": "ibm_fez",
            "best_device": "ibm_fez",
            "qpu_type": "qiskit-runtime-service",
            "resource": resource,
            "env_before_enumeration": {},
            "job_args": command,
            "acquire_quantum_resource": Mock(return_value="fake-id"),
            "post_qrmi_resource_id": Mock(),
            "job_environment": Mock(return_value=dict(os.environ)),
        }
        helper = next(
            node for node in TREE.body
            if isinstance(node, ast.FunctionDef)
            and node.name == "release_quantum_resource"
        )
        load_nodes([helper], namespace)
        return namespace

    def check_exit(self, nodes, command, expected, release_failure=False):
        resource = Mock()
        if release_failure:
            resource.release.side_effect = RuntimeError("release failed")
        namespace = self.namespace(command, resource)
        token_key = "ibm_fez_QRMI_JOB_ACQUISITION_TOKEN"

        with patch.dict(os.environ, {token_key: "fake-id"}):
            with contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as caught:
                    load_nodes(nodes, namespace)
            self.assertEqual(caught.exception.code, expected)
            resource.release.assert_called_once_with("fake-id")
            self.assertNotIn(token_key, os.environ)

    def test_normal_exit_codes(self):
        for path, nodes in self.paths().items():
            for code in (0, 1, 42, 143, 255):
                with self.subTest(path=path, code=code):
                    self.check_exit(
                        nodes,
                        [sys.executable, "-c", f"raise SystemExit({code})"],
                        code,
                    )

    @unittest.skipUnless(os.name == "posix", "Requires POSIX signals")
    def test_signal_exit_codes_and_cleanup(self):
        for path, nodes in self.paths().items():
            for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGKILL):
                for release_failure in (False, True):
                    with self.subTest(
                        path=path,
                        signal=sig,
                        release_failure=release_failure,
                    ):
                        program = "import os, signal; "
                        if sig != signal.SIGKILL:
                            program += (
                                f"signal.signal({int(sig)}, signal.SIG_DFL); "
                            )
                        program += f"os.kill(os.getpid(), {int(sig)})"
                        self.check_exit(
                            nodes,
                            [sys.executable, "-c", program],
                            128 + int(sig),
                            release_failure,
                        )

    def test_launch_failure_still_releases(self):
        for path, nodes in self.paths().items():
            with self.subTest(path=path):
                resource = Mock()
                namespace = self.namespace(["missing-app"], resource)
                token_key = "ibm_fez_QRMI_JOB_ACQUISITION_TOKEN"

                with patch.dict(os.environ, {token_key: "fake-id"}):
                    with patch.object(
                        subprocess, "run", side_effect=FileNotFoundError()
                    ):
                        with self.assertRaises(FileNotFoundError):
                            load_nodes(nodes, namespace)
                    resource.release.assert_called_once_with("fake-id")
                    self.assertNotIn(token_key, os.environ)


if __name__ == "__main__":
    unittest.main()
