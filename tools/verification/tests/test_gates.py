"""Executable routing and OS-process ownership, without compiling app fixtures."""
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
CACHE = Path.home() / ".cache/coding-agents"


class Gates(unittest.TestCase):
    def setUp(self):
        CACHE.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="snap-gate-contract-", dir=CACHE)
        self.root = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def executable(self, name, body):
        path = self.root / name
        path.write_text(f"#!{sys.executable}\n" + body)
        path.chmod(0o755)
        return path

    def test_browser_selection_preserves_arguments_and_both_aggregates(self):
        record = self.root / "commands"
        self.executable("mise", """
import json, os, sys
with open(os.environ['SNAP_GATE_COMMANDS'], 'a') as output:
    output.write(json.dumps(sys.argv[1:]) + '\\n')
sys.exit(int(os.environ.get('SNAP_GATE_EXIT', '0')))
""")
        env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}", SNAP_GATE_COMMANDS=str(record))
        framework, app = "snap-browser-tests", "snap-app-browser-tests"
        for arguments, packages in [
            ([], [framework, app]),
            (["all"], [framework, app]),
            (["testy", "--browser", "chromium"], [app]),
            (["--browser", "chromium", "testy"], [app]),
            (["--filter", "production", "factorio"], [app]),
            (["--artifacts=authy", "--filter=journey", "--", "chatty"], [app]),
            (["--browser", "authy", "client"], [framework]),
            (["--artifacts", "testy", "react"], [framework]),
        ]:
            with self.subTest(arguments=arguments):
                record.write_text("")
                result = subprocess.run([ROOT / "bin/browser-tests", *arguments], env=env,
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = [json.loads(line) for line in record.read_text().splitlines()]
                self.assertEqual(commands, [
                    ["exec", "--", "cargo", "run", "-p", package, "--", "--prepare", *arguments]
                    for package in packages
                ])
        record.write_text("")
        env["SNAP_GATE_EXIT"] = "43"
        result = subprocess.run([ROOT / "bin/browser-tests", "all"], env=env, timeout=10)
        self.assertEqual(result.returncode, 43)
        self.assertEqual(len(record.read_text().splitlines()), 1)

    def cancel_verifier(self, signum, phase, resistant):
        ready = self.root / "ready"
        child = self.executable("listener", """
import json, os, signal, socket
from pathlib import Path
if os.environ['SNAP_GATE_RESISTANT'] == '1':
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
listener = socket.socket()
listener.bind(('127.0.0.1', 0))
listener.listen()
ready = Path(os.environ['SNAP_GATE_READY'])
ready.with_suffix('.pending').write_text(json.dumps([os.getpid(), listener.getsockname()[1]]))
ready.with_suffix('.pending').rename(ready)
while True:
    connection, _ = listener.accept()
    connection.close()
""")
        self.executable("cargo", """
import json, os, subprocess, sys
if sys.argv[1] == 'metadata' and os.environ['SNAP_GATE_PHASE'] != 'metadata':
    print(json.dumps({'packages': []}))
else:
    child = subprocess.Popen([os.environ['SNAP_GATE_LISTENER']])
    child.wait()
""")
        env = dict(os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
                   SNAP_GATE_READY=str(ready), SNAP_GATE_PHASE=phase,
                   SNAP_GATE_LISTENER=str(child), SNAP_GATE_RESISTANT=str(int(resistant)))
        log = self.root / "verification.log"
        listener_pid = None
        scratch = None
        with log.open("w") as output:
            supervisor = subprocess.Popen([sys.executable, ROOT / "tools/verification/framework.py", "io"],
                                          env=env, stdout=output, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 20
                while not ready.exists() and time.monotonic() < deadline and supervisor.poll() is None:
                    time.sleep(0.02)
                self.assertTrue(ready.exists(), log.read_text())
                listener_pid, port = json.loads(ready.read_text())
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    pass
                supervisor.send_signal(signum)
                status = supervisor.wait(timeout=15)
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                            pass
                    except ConnectionRefusedError:
                        break
                    time.sleep(0.02)
                else:
                    self.fail("cancelled verifier left its listener alive")
                self.assertEqual(status, 128 + signum, log.read_text())
            finally:
                if supervisor.poll() is None:
                    supervisor.kill()
                    supervisor.wait()
                # Clean only this probe's surviving dependency, including on red.
                if listener_pid is not None:
                    try:
                        os.kill(listener_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                match = re.search(r"^Isolated verification: (.+)$", log.read_text(), re.MULTILINE)
                if match:
                    scratch = Path(match[1])
                if scratch is not None and scratch.parent == CACHE and scratch.name.startswith("snap-framework-"):
                    shutil.rmtree(scratch)

    def test_sigterm_retires_gate_listener(self):
        self.cancel_verifier(signal.SIGTERM, "gate", False)

    def test_sigint_retires_resistant_descendant_after_leader_exits(self):
        self.cancel_verifier(signal.SIGINT, "gate", True)

    def test_sigterm_retires_metadata_listener(self):
        self.cancel_verifier(signal.SIGTERM, "metadata", False)


if __name__ == "__main__":
    unittest.main()
