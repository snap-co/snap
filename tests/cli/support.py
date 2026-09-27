"""CLI consumer fixtures are independent projects, not Snap applications."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SNAP = ROOT / "target/debug/snap"


def kill_session(session):
    """Linux test fallback: commands have separate groups in our owned session."""
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        try:
            if os.getsid(pid) == session:
                os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass


def stop_cli(process):
    if process.poll() is None:
        process.terminate()
    try:
        return process.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        kill_session(process.pid)
        return process.communicate(timeout=3)


class ProjectContract(unittest.TestCase):
    def setUp(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="snap-cli-", dir=ROOT / ".tmp")
        self.root = Path(self.temp.name)
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[package]\nname="cli-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n'
        )
        (self.root / "src/main.rs").write_text('fn main() { std::process::exit(37); }')
        self.config = 'version=1\napplication="fixture"\n'
        (self.root / "snap.toml").write_text(self.config)
        self.env = {**os.environ, "CARGO_TARGET_DIR": str(self.root / "target")}

    def tearDown(self):
        self.temp.cleanup()

    def run_cli(self, *args, cwd=None, timeout=30):
        process = subprocess.Popen([str(SNAP), *args], cwd=cwd or self.root, env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        try:
            try:
                stdout, stderr = process.communicate(timeout=timeout)
            except subprocess.TimeoutExpired as error:
                error.stdout, error.stderr = stop_cli(process)
                raise
            return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
        finally:
            if process.poll() is None:
                stop_cli(process)
            kill_session(process.pid)
            process.stdout.close()
            process.stderr.close()
