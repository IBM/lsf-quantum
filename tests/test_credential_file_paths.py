# SPDX-License-Identifier: Apache-2.0
import ast
import contextlib
import io
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock
from dotenv import dotenv_values

SOURCE = Path(__file__).resolve().parents[1] / "qrmi-esub-jobstarter.py"
TREE = ast.parse(SOURCE.read_text())


class CredentialPathTests(unittest.TestCase):
    def setUp(self):
        self.original_cwd = Path.cwd()
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        os.chdir(self.root)
        self.addCleanup(self.temporary.cleanup)
        self.addCleanup(os.chdir, self.original_cwd)
        function = next(
            node for node in TREE.body
            if isinstance(node, ast.FunctionDef)
            and node.name == "read_config_file"
        )
        namespace = {
            "os": os, "sys": sys, "dotenv_values": dotenv_values,
        }
        exec(compile(
            ast.Module(body=[function], type_ignores=[]),
            str(SOURCE), "exec",
        ), namespace)
        self.reader = namespace["read_config_file"]
        (self.root / "configs").mkdir()
        self.target = self.root / "configs" / "dummy.env"
        self.target.write_text("TEST_MARKER=expected\n")

    def test_basename(self):
        Path("dummy.env").write_text("TEST_MARKER=expected\n")
        self.assertEqual(
            self.reader(Path("dummy.env"))["TEST_MARKER"], "expected",
        )

    def test_absolute_path(self):
        self.assertEqual(
            self.reader(self.target)["TEST_MARKER"], "expected",
        )

    def test_nested_relative_path(self):
        self.assertEqual(
            self.reader(Path("configs/dummy.env"))["TEST_MARKER"], "expected",
        )

    def test_same_name_collision(self):
        Path("dummy.env").write_text("TEST_MARKER=wrong-file\n")
        for path in [self.target, Path("configs/dummy.env")]:
            with self.subTest(path=path):
                self.assertEqual(self.reader(path)["TEST_MARKER"], "expected")

    def test_missing_requested_path_does_not_fall_back(self):
        Path("missing.env").write_text("TEST_MARKER=wrong-file\n")
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            result = self.reader(Path("configs/missing.env"))
        self.assertIsNone(result)
        self.assertIn(str(self.root / "configs" / "missing.env"), output.getvalue())

    def test_error_message_identifies_requested_file(self):
        esub = next(
            node for node in TREE.body
            if isinstance(node, ast.If)
            and ast.unparse(node.test) == "identity == 'esub'"
        )
        failure = next(
            node for node in esub.body
            if isinstance(node, ast.If)
            and ast.unparse(node.test) == "not creds"
        )
        error = Mock(side_effect=SystemExit(1))
        with self.assertRaises(SystemExit):
            exec(compile(
                ast.Module(body=failure.body, type_ignores=[]),
                str(SOURCE), "exec",
            ), {
                "config": SimpleNamespace(file=self.target),
                "print_error": error,
            })
        error.assert_called_once_with(
            f"Cannot read credentials file {self.target}",
        )


if __name__ == "__main__":
    unittest.main()
