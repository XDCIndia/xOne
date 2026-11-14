"""Black-box failure-path checks for CI orchestration; no Rust toolchain required.

The fixture runner test stubs successful compiler/layering dependencies and injects
one failing globals check. It checks the runner's final result, not Rust lint behavior.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RULE_CRATES = (
    "chainspec consensus evm state primitives signer net executor sync snap builder rpc"
).split()


def executable(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


class GlobalsGate(unittest.TestCase):
    def run_scan(self, *paths):
        return subprocess.run(
            ["bash", str(ROOT / "ci/check-globals.sh"), *map(str, paths)],
            capture_output=True, text=True, check=False,
        )

    def test_clean_source_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lib.rs"
            path.write_text("pub const VALUE: u8 = 1;\n")
            self.assertEqual(self.run_scan(path).returncode, 0)

    def test_forbidden_global_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "lib.rs"
            path.write_text("static mut VALUE: u8 = 0;\n")
            self.assertNotEqual(self.run_scan(path).returncode, 0)

    def test_missing_input_is_not_a_clean_scan(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_scan(Path(directory) / "absent.rs")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("scan failed", result.stderr)

    def test_default_scope_includes_executor(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ci").mkdir()
            shutil.copy2(ROOT / "ci/check-globals.sh", root / "ci/check-globals.sh")
            for name in RULE_CRATES:
                path = root / "crates" / ("xone-" + name)
                path.mkdir(parents=True)
                (path / "lib.rs").write_text("pub const VALUE: u8 = 1;\n")
            (root / "crates/xone-executor/lib.rs").write_text("static mut VALUE: u8 = 0;\n")
            result = subprocess.run(
                ["bash", str(root / "ci/check-globals.sh")],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("xone-executor", result.stdout)


class OracleAccessGate(unittest.TestCase):
    def test_missing_access_cannot_qualify(self):
        env = os.environ.copy()
        env.pop("GETH_XDC_DEPLOY_KEY", None)
        result = subprocess.run(
            ["bash", str(ROOT / "ci/check-oracle-access.sh")],
            env=env, capture_output=True, text=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot qualify", result.stderr)

    def test_access_check_does_not_print_credential(self):
        env = os.environ.copy()
        env["GETH_XDC_DEPLOY_KEY"] = "test-only-not-a-real-credential"
        result = subprocess.run(
            ["bash", str(ROOT / "ci/check-oracle-access.sh")],
            env=env, capture_output=True, text=True, check=False,
        )
        self.assertEqual(result.returncode, 0)
        self.assertNotIn(env["GETH_XDC_DEPLOY_KEY"], result.stdout + result.stderr)


class FixtureRunnerGate(unittest.TestCase):
    def run_runner(self, globals_fail):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ci/fixtures/as-conversions").mkdir(parents=True)
            (root / "ci/fixtures/globals-grep").mkdir()
            shutil.copy2(ROOT / "ci/check-fixtures.sh", root / "ci/check-fixtures.sh")
            (root / "ci/fixtures/as-conversions/lib.rs").write_text("// expect: as_conversions\n")
            (root / "ci/fixtures/globals-grep/global.rs").write_text("static mut X: u8 = 0;\n")
            (root / "ci/rule2.clippy.toml").write_text("")
            (root / "Cargo.toml").write_text("[workspace.lints.rust]\nunsafe_code = 'forbid'\n")
            for name in RULE_CRATES:
                path = root / "crates" / ("xone-" + name)
                path.mkdir(parents=True)
                (path / "clippy.toml").write_text("")
                (path / "Cargo.toml").write_text("[lints]\nworkspace = true\n")
            executable(root / "ci/check-layering.sh", "#!/usr/bin/env bash\nexit 0\n")
            executable(root / "ci/check-globals.sh", "#!/usr/bin/env bash\n"
                       "if [ $# -gt 0 ]; then exit 1; fi\n"
                       "exit " + ("1" if globals_fail else "0") + "\n")
            executable(root / "tools/cargo", "#!/usr/bin/env bash\n"
                       "for arg in \"$@\"; do\n"
                       "  case \"$arg\" in\n"
                       "    */_control/Cargo.toml) exit 0 ;;\n"
                       "    */as-conversions/Cargo.toml) echo as_conversions >&2; exit 1 ;;\n"
                       "  esac\n"
                       "done\nexit 0\n")
            env = os.environ.copy()
            env["PATH"] = str(root / "tools") + os.pathsep + env["PATH"]
            return subprocess.run(
                ["bash", str(root / "ci/check-fixtures.sh")],
                env=env, capture_output=True, text=True, check=False,
            )

    def test_positive_control(self):
        result = self.run_runner(False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_real_globals_failure_fails_runner(self):
        result = self.run_runner(True)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("FAIL  check-globals", result.stdout)


class LocBudgetGate(unittest.TestCase):
    def run_loc(self, lines, ceiling):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/xone-consensus"]\n')
            crate = root / "crates/xone-consensus"
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text('[package]\nname = "xone-consensus"\nversion = "0.0.0"\nedition = "2021"\n')
            (crate / "src/lib.rs").write_text("// comment\n\n" + "pub fn f() {}\n" * lines
                                              + "#[cfg(test)]\nmod t {\n    fn x() {}\n}\n")
            budget = root / "budget.toml"
            budget.write_text(f"[ceiling]\nxone-consensus = {ceiling}\n")
            return subprocess.run(
                ["python3", "-I", str(ROOT / "ci/loc.py"), "--root", str(root), "--budget", str(budget)],
                capture_output=True, text=True, check=False)

    def test_at_ceiling_passes(self):
        r = self.run_loc(10, 10)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("| xone-consensus | 10 |", r.stdout)

    def test_over_ceiling_fails(self):
        r = self.run_loc(11, 10)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("OVER BUDGET xone-consensus", r.stderr)


if __name__ == "__main__":
    unittest.main()
