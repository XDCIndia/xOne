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


class UnsafeAuditDrift(unittest.TestCase):
    def run_drift(self, fresh_extra=""):
        table = ("| crate | version | unsafe count | FFI | reason | owner review |\n"
                 "|---|---|---|---|---|---|\n| aes | 0.8.4 | 110 | no | via x | pending |\n")
        with tempfile.TemporaryDirectory() as d:
            fresh, kept = Path(d) / "fresh.md", Path(d) / "kept.md"
            fresh.write_text(table + fresh_extra)
            kept.write_text(table)
            return subprocess.run(["python3", "-I", str(ROOT / "ci/unsafe-audit-drift.py"), str(fresh), str(kept)],
                                  capture_output=True, text=True, check=False)

    def test_same_table_passes(self):
        self.assertEqual(self.run_drift().returncode, 0)

    def test_new_package_fails(self):
        r = self.run_drift("| evil | 1.0.0 | 5 | no | via y | pending |\n")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("evil", r.stderr)


class ReleaseTreeFeatureCheck(unittest.TestCase):
    """Check (a) of ci/check-release-tree.sh with a stub `cargo`: a forbidden feature in the
    release tree fails before any build; a clean tree passes (a) (the stub build then stops the run)."""
    def run_check(self, tree):
        with tempfile.TemporaryDirectory() as d:
            executable(Path(d) / "cargo", f'#!/bin/sh\n[ "$1" = tree ] && {{ printf "%s\\n" {tree!r}; exit 0; }}\nexit 1\n')
            env = dict(os.environ, PATH=f"{d}:{os.environ['PATH']}")
            return subprocess.run(["bash", str(ROOT / "ci/check-release-tree.sh")], env=env,
                                  capture_output=True, text=True, check=False)

    def test_mutants_feature_fails(self):
        r = self.run_check('xone v0.1.0\nxone-db v0.1.0\nxone-db feature "mutants"')
        self.assertIn("FAIL (a)", r.stdout)
        self.assertNotEqual(r.returncode, 0)

    def test_fault_injection_feature_fails(self):
        r = self.run_check('xone-db feature "fault-injection"')
        self.assertIn("FAIL (a)", r.stdout)

    def test_clean_tree_passes_check_a(self):
        r = self.run_check('xone v0.1.0\nxone-db feature "default"')
        self.assertIn("ok   (a)", r.stdout)


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


class OracleAccessGate(unittest.TestCase):
    def run_check(self, key):
        with tempfile.TemporaryDirectory() as d:
            out = Path(d) / "out"
            env = {k: v for k, v in os.environ.items() if k != "GETH_XDC_DEPLOY_KEY"}
            env["GITHUB_OUTPUT"] = str(out)
            if key:
                env["GETH_XDC_DEPLOY_KEY"] = key
            r = subprocess.run(["bash", str(ROOT / "ci/check-oracle-access.sh")],
                               env=env, capture_output=True, text=True, check=False)
            return r, out.read_text()

    def test_missing_key_is_not_qualified_and_green(self):
        r, out = self.run_check(None)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("not qualified", r.stdout)
        self.assertEqual(out.strip(), "available=false")

    def test_key_present_qualifies_without_printing_it(self):
        r, out = self.run_check("SECRET-VALUE")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(out.strip(), "available=true")
        self.assertNotIn("SECRET-VALUE", r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
