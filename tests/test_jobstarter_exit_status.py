# SPDX-License-Identifier: Apache-2.0
import ast
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import Mock

SOURCE = Path(__file__).resolve().parents[1] / "qrmi-esub-jobstarter.py"
TREE = ast.parse(SOURCE.read_text())


def launch_blocks():
    explicit = next(
        n for n in TREE.body
        if isinstance(n, ast.If)
        and ast.unparse(n.test) == "device is not None"
    )
    blocks = {}
    for name, nodes in [
        ("explicit", explicit.body), ("automatic", TREE.body),
    ]:
        for index, node in enumerate(nodes):
            if isinstance(node, ast.Try) and any(
                isinstance(item, ast.Call)
                and ast.unparse(item.func) == "subprocess.run"
                and item.args
                and isinstance(item.args[0], ast.Name)
                and item.args[0].id == "job_args"
                for statement in node.body
                for item in ast.walk(statement)
            ):
                blocks[name] = compile(
                    ast.Module(body=nodes[index:index + 2], type_ignores=[]),
                    str(SOURCE), "exec",
                )
                break
    assert set(blocks) == {"explicit", "automatic"}
    return blocks


class ExitStatusTests(unittest.TestCase):
    def namespace(self, run, release):
        return {
            "subprocess": Mock(run=run),
            "sys": sys,
            "job_args": ["fake-application"],
            "job_env": {},
            "resource": object(),
            "device": "fake-backend",
            "best_device": "fake-backend",
            "acquisition_token": "fake-acquisition",
            "release_quantum_resource": release,
        }

    def test_exit_codes_and_cleanup(self):
        for path, code in launch_blocks().items():
            for status in [0, 1, 42, 255]:
                with self.subTest(path=path, status=status):
                    run = Mock(return_value=subprocess.CompletedProcess([], status))
                    release = Mock()
                    namespace = self.namespace(run, release)
                    with self.assertRaises(SystemExit) as result:
                        exec(code, namespace)
                    self.assertEqual(result.exception.code, status)
                    run.assert_called_once_with(
                        namespace["job_args"], env=namespace["job_env"],
                    )
                    release.assert_called_once_with(
                        namespace["resource"], "fake-backend", "fake-acquisition",
                    )

    def test_launch_failure_still_releases(self):
        for path, code in launch_blocks().items():
            with self.subTest(path=path):
                release = Mock()
                namespace = self.namespace(
                    Mock(side_effect=OSError("Application could not start")),
                    release,
                )
                with self.assertRaises(OSError):
                    exec(code, namespace)
                release.assert_called_once_with(
                    namespace["resource"], "fake-backend", "fake-acquisition",
                )


if __name__ == "__main__":
    unittest.main()
