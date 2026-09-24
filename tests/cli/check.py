"""Project checks select one application's validation and own their processes."""
import json
import selectors
import signal
import socket
import subprocess
import unittest

from dev import ProjectContract, ROOT, SNAP, kill_session, stop_cli


class CheckContract(ProjectContract):
    def setUp(self):
        super().setUp()
        (self.root / "src/main.rs").write_text('fn main() {\n    std::process::exit(37);\n}\n')

    def test_failures_and_missing_tools_cannot_report_success(self):
        for commands, code, message in [
            ([["python3", "-c", "import sys;sys.exit(43)"]], 43, "Project check failed"),
            ([["snap-check-missing-tool"]], 1, "Install the command"),
        ]:
            with self.subTest(commands=commands):
                (self.root / "snap.toml").write_text(self.config + '\n[check]\ncommands=' +
                    json.dumps(commands + [["python3", "-c", "open('never','w').close()"]]))
                result = self.run_cli("check", cwd=self.root / "src")
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertIn(message, result.stderr)
                self.assertFalse((self.root / "never").exists())
                self.assertFalse((self.root / ".snap/build/debug").exists())
        (self.root / "src/main.rs").write_text('fn main() { missing(); }')
        result = self.run_cli("check")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "never").exists())

    def test_sibling_checks_do_not_inherit_another_projects_commands(self):
        sibling = self.root / "sibling"
        (sibling / "src").mkdir(parents=True)
        (sibling / "Cargo.toml").write_text((self.root / "Cargo.toml").read_text())
        (sibling / "src/main.rs").write_text('fn main() {}\n')
        (self.root / "snap.toml").write_text(self.config +
            '\n[check]\ncommands=[["python3","-c","import sys;sys.exit(44)"]]')
        (sibling / "snap.toml").write_text(self.config +
            '\n[check]\ncommands=[["python3","-c","print(12345)"]]')
        result = self.run_cli("check", "sibling")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("12345", result.stdout)
        (sibling / "snap.toml").write_text("invalid config")
        result = self.run_cli("check", cwd=sibling / "src")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(sibling / "snap.toml"), result.stderr)

    def test_interrupt_check_cleans_its_listener_descendant(self):
        child = ('import socket,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                 's=socket.socket(); s.bind(("127.0.0.1",0)); s.listen(); '
                 'print(s.getsockname()[1],flush=True); time.sleep(60)')
        (self.root / "snap.toml").write_text(self.config +
            '\n[check]\ncommands=[["python3","check.py"]]\n')
        # Separate readiness pipe from Rust's test output.
        readied = self.root / "ready"
        (self.root / "check.py").write_text(
            'import subprocess,time\n'
            f'p=subprocess.Popen(["python3","-c",{child!r}],stdout=subprocess.PIPE)\n'
            'from pathlib import Path\nPath("ready").write_bytes(p.stdout.readline())\n'
            'print("CHECK_READY",flush=True)\ntime.sleep(60)\n')
        process = subprocess.Popen([str(SNAP), "check"], cwd=self.root, env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while not readied.exists():
                    self.assertTrue(selector.select(20), "Check never became ready")
                    self.assertTrue(process.stdout.readline(), "CLI exited before check")
            port = int(readied.read_text())
            process.send_signal(signal.SIGTERM)
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 143)
            with socket.socket() as probe:
                self.assertNotEqual(probe.connect_ex(("127.0.0.1", port)), 0)
        finally:
            stop_cli(process)
            kill_session(process.pid)
            process.stdout.close()
            process.stderr.close()

    def test_check_builds_selected_artifacts_and_runs_literal_commands(self):
        source = 'fn main() {\n    std::process::exit(37);\n}\n'
        (self.root / "src/main.rs").write_text('not generated yet')
        (self.root / "prepare.py").write_text(
            f'from pathlib import Path\nPath("src/main.rs").write_text({source!r})\n')
        (self.root / "check.py").write_text(
            'import os,json,sys,subprocess\nfrom pathlib import Path\n'
            'executable=os.environ["SNAP_CHECK_EXECUTABLE"]\n'
            'assert subprocess.run([executable]).returncode==37\n'
            'assert "SNAP_CHECK_WEB_DIR" not in os.environ\n'
            'Path("checked.json").write_text(json.dumps([os.getcwd(),sys.argv[1:],executable]))\n')
        argv = ["a b", "", "*", "$(touch bad)"]
        (self.root / "snap.toml").write_text(self.config +
            '\n[prepare]\nbuild=[["python3","prepare.py"]]\n[check]\nbuild=true\ncommands=' +
            json.dumps([["python3", "check.py", *argv]]) + '\n')
        self.env["SNAP_CHECK_WEB_DIR"] = "/other/app"
        result = self.run_cli("check", str(self.root), cwd=ROOT)
        self.assertEqual(result.returncode, 0, result.stderr)
        checked = json.loads((self.root / "checked.json").read_text())
        self.assertEqual(checked, [str(self.root), argv,
                                  str(self.root / ".snap/build/debug/cli-fixture")])
        self.assertFalse((self.root / "bad").exists())


if __name__ == "__main__":
    (ROOT / ".tmp").mkdir(exist_ok=True)
    unittest.main()
