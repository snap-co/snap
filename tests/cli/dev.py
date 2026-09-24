"""CLI consumer contracts. Fixtures are independent projects, not Snap internals."""
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SNAP = ROOT / "target/debug/snap"


def kill_session(session):
    """Linux test fallback: commands have separate groups, but share our owned session."""
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
        # snap allows six seconds for its active command to stop.
        return process.communicate(timeout=8)
    except subprocess.TimeoutExpired:
        kill_session(process.pid)
        return process.communicate(timeout=3)


class ProjectContract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="snap-cli-", dir=ROOT / ".tmp")
        self.root = Path(self.temp.name)
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[package]\nname="cli-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n'
        )
        (self.root / "src/main.rs").write_text('fn main() { std::process::exit(37); }')
        self.config = (
            'version=1\napplication="fixture"\n'
            '[server]\nmanifest="Cargo.toml"\nbin="cli-fixture"\n'
            '[dev]\naddress="127.0.0.1:0"\n'
        )
        (self.root / "snap.toml").write_text(self.config)
        self.env = {**os.environ, "CARGO_TARGET_DIR": str(self.root / "target")}
        for key in ["SNAP_ADDR", "SNAP_BUILD", "SNAP_WEB_DIR"]:
            self.env.pop(key, None)

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

class DevContract(ProjectContract):
    def test_web_host_startup_failure_preserves_exit_code(self):
        (self.root / "src/main.rs").write_text('''fn main() {
    std::process::Command::new("sleep").arg("60").spawn().unwrap();
    std::process::exit(37);
}
''')
        (self.root / "app.ts").write_text("export default {};\n")
        (self.root / "host.ts").write_text('import app from "snap:application"; console.log(app);\n')
        (self.root / "index.html").write_text('<script type="module" src="/main.js"></script>')
        (self.root / "snap.toml").write_text(self.config + '\n[web]\n' +
            f'package-dir={json.dumps(str(ROOT))}\napplication="app.ts"\nhost="host.ts"\nhtml="index.html"\n' +
            f'wasm-manifest={json.dumps(str(ROOT / "apps/healthy/wasm/Cargo.toml"))}\nbindings=".snap/bindings"\n')
        self.env["CARGO_TARGET_DIR"] = str(ROOT / "target")
        tools = sorted((ROOT / "apps/healthy/.snap/tools").glob("wasm-bindgen-*/bin"))
        self.env["PATH"] = os.pathsep.join([*(str(path) for path in tools), self.env["PATH"]])
        result = self.run_cli("dev", timeout=120)
        self.assertEqual(result.returncode, 37, result.stderr)

    def test_help_and_version_without_project(self):
        (self.root / "snap.toml").unlink()
        self.assertEqual(self.run_cli("--help").returncode, 0)
        self.assertEqual(self.run_cli("--version").returncode, 0)
        missing = self.run_cli("dev")
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("No snap.toml", missing.stderr)
        explicit = self.run_cli("dev", "missing")
        self.assertNotEqual(explicit.returncode, 0)
        self.assertIn("does not exist", explicit.stderr)

    def test_nearest_broken_config_prevents_ancestor_launch(self):
        nested = self.root / "src"
        (nested / "snap.toml").write_text("not valid toml")
        result = self.run_cli("dev", cwd=nested)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(nested / "snap.toml"), result.stderr)
        self.assertFalse((self.root / "target").exists())
        (nested / "snap.toml").unlink()
        (nested / "snap.toml").symlink_to("missing-config")
        result = self.run_cli("dev", cwd=nested)
        self.assertIn("Cannot read", result.stderr)

    def test_invalid_config_does_not_execute_preparation(self):
        for invalid in [self.config + '\nunknown=true\n',
                        self.config.replace('version=1', 'version=9'),
                        self.config.replace('bin="cli-fixture"', 'bin="cli-fixture"\nexample="other"')]:
            (self.root / "snap.toml").write_text(invalid)
            result = self.run_cli("dev")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((self.root / "target").exists())

    def test_preparation_argv_cwd_order_and_failure(self):
        hook = self.root / "prepare.py"
        hook.write_text('import json,sys,os\nfrom pathlib import Path\n'
                        'p=Path("calls.json")\n'
                        'calls=json.loads(p.read_text()) if p.exists() else []\n'
                        'calls.append([os.getcwd(), sys.argv[1:]])\n'
                        'p.write_text(json.dumps(calls))\n'
                        'sys.exit(42 if sys.argv[1]=="fail" else 0)\n')
        commands = [["python3", "prepare.py", "first", "a b", "", "*", "$(touch bad)"],
                    ["python3", "prepare.py", "fail"],
                    ["python3", "prepare.py", "never"]]
        (self.root / "snap.toml").write_text(self.config + '\n[prepare]\ndev=' + json.dumps(commands))
        result = self.run_cli("dev", cwd=self.root / "src")
        self.assertEqual(result.returncode, 42, result.stderr)
        calls = json.loads((self.root / "calls.json").read_text())
        self.assertEqual(calls, [[str(self.root), commands[0][2:]], [str(self.root), ["fail"]]])
        self.assertFalse((self.root / "target").exists())
        self.assertFalse((self.root / "bad").exists())

    def test_real_cargo_target_discovery_environment_and_exit(self):
        (self.root / "src/main.rs").write_text('''fn main() {
            println!("cwd={}", std::env::current_dir().unwrap().display());
            for key in ["SNAP_ADDR", "SNAP_APPLICATION", "SNAP_BUILD", "SNAP_ENV"] {
                println!("{key}={}", std::env::var(key).unwrap());
            }
            assert!(std::env::var("SNAP_WEB_DIR").is_err());
            std::process::exit(37);
        }''')
        self.env["SNAP_WEB_DIR"] = "/wrong/app/assets"
        nested = self.run_cli("dev", cwd=self.root / "src")
        self.assertEqual(nested.returncode, 37, nested.stderr)
        self.assertIn(f"cwd={self.root}", nested.stdout)
        self.assertIn("SNAP_APPLICATION=fixture", nested.stdout)
        self.assertIn("SNAP_ADDR=127.0.0.1:0", nested.stdout)
        self.env["SNAP_BUILD"] = "explicit-build"
        explicit = self.run_cli("dev", str(self.root), cwd=ROOT)
        self.assertEqual(explicit.returncode, 37, explicit.stderr)
        self.assertIn("SNAP_BUILD=explicit-build", explicit.stdout)

    def test_interrupt_preparation_cleans_descendants(self):
        # Both processes ignore TERM, exercising bounded forced cleanup of the group.
        child = ('import socket,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                 's=socket.socket(); s.bind(("127.0.0.1",0)); s.listen(); '
                 'print(s.getsockname()[1],flush=True); time.sleep(60)')
        hook = self.root / "prepare.py"
        hook.write_text('import signal,subprocess,time\n'
                        'signal.signal(signal.SIGTERM,signal.SIG_IGN)\n'
                        f'subprocess.Popen(["python3","-c",{child!r}])\n'
                        'time.sleep(60)\n')
        (self.root / "snap.toml").write_text(self.config + '\n[prepare]\ndev=[["python3","prepare.py"]]\n')
        process = subprocess.Popen([str(SNAP), "dev"], cwd=self.root, env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                self.assertTrue(selector.select(10), "Preparation never became ready")
                port = int(process.stdout.readline())
            process.terminate()
            process.communicate(timeout=10)
            self.assertNotEqual(process.returncode, 0)
            with socket.socket() as probe:
                self.assertNotEqual(probe.connect_ex(("127.0.0.1", port)), 0)
            self.assertFalse((self.root / "target").exists())
        finally:
            stop_cli(process)
            kill_session(process.pid)
            process.stdout.close()
            process.stderr.close()

    def test_launcher_timeout_releases_stalled_commands(self):
        # Exercise graceful cleanup and the fallback when snap itself cannot respond.
        hook = self.root / "prepare.py"
        for freeze_cli in [False, True]:
            with self.subTest(freeze_cli=freeze_cli):
                hook.write_text('import os,socket,signal,time\nfrom pathlib import Path\n'
                                'signal.signal(signal.SIGTERM,signal.SIG_IGN)\n'
                                's=socket.socket(); s.bind(("127.0.0.1",0)); s.listen()\n'
                                'Path("port").write_text(str(s.getsockname()[1]))\n'
                                + ('os.kill(os.getppid(),signal.SIGSTOP)\n' if freeze_cli else '')
                                + 'time.sleep(60)\n')
                (self.root / "snap.toml").write_text(self.config + '\n[prepare]\ndev=[["python3","prepare.py"]]\n')
                with self.assertRaises(subprocess.TimeoutExpired):
                    self.run_cli("dev", timeout=2)
                port = int((self.root / "port").read_text())
                with socket.socket() as probe:
                    self.assertNotEqual(probe.connect_ex(("127.0.0.1", port)), 0)
                self.assertFalse((self.root / "target").exists())
                (self.root / "port").unlink()

    def test_checkout_wrapper_preserves_mise_toolchain_environment(self):
        mise, bun = shutil.which("mise"), shutil.which("bun")
        if not mise or not bun or shutil.which("cargo", path=os.defpath):
            self.skipTest("Requires mise, Bun, and no system Cargo to isolate toolchain PATH")
        tools = self.root / "tools"
        tools.mkdir()
        (tools / "mise").symlink_to(mise)
        (tools / "bun").symlink_to(bun)
        env = {**self.env, "PATH": str(tools) + os.pathsep + os.defpath,
               "SNAP_ADDR": "127.0.0.1:0"}
        env.pop("CARGO_TARGET_DIR", None)
        self.assertIsNone(shutil.which("cargo", path=env["PATH"]))
        process = subprocess.Popen([str(ROOT / "bin/dev")], cwd=self.root, env=env,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                   start_new_session=True)
        try:
            logs = b""
            deadline = time.monotonic() + 120
            with selectors.DefaultSelector() as selector:
                selector.register(process.stderr, selectors.EVENT_READ)
                while b"listening on http://127.0.0.1:" not in logs:
                    self.assertTrue(selector.select(max(0, deadline - time.monotonic())), logs.decode())
                    chunk = os.read(process.stderr.fileno(), 65536)
                    self.assertTrue(chunk, logs.decode())
                    logs += chunk
            process.terminate()
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0)
        finally:
            stop_cli(process)
            kill_session(process.pid)
            process.stderr.close()


if __name__ == "__main__":
    (ROOT / ".tmp").mkdir(exist_ok=True)
    unittest.main()
