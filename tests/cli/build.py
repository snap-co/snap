"""Build command contracts through independent projects and their packaged executables."""
import json
import os
import selectors
import shutil
import signal
import socket
import subprocess
import unittest

from dev import ProjectContract, ROOT, SNAP, kill_session, stop_cli


class BuildContract(ProjectContract):
    def test_web_package_uses_the_selected_rust_and_browser_profile(self):
        web = self.root / "web"
        (web / "wasm/src").mkdir(parents=True)
        (web / "package.json").write_text('{"name":"build-contract","private":true}')
        (web / "index.html").write_text('<script type="module" src="main.js"></script>')
        (web / "wasm/Cargo.toml").write_text(
            '[package]\nname="build-web-contract"\nversion="0.0.0"\nedition="2024"\n'
            '[lib]\ncrate-type=["cdylib"]\n[dependencies]\nwasm-bindgen="=0.2.128"\n[workspace]\n')
        (web / "wasm/src/lib.rs").write_text(
            'use wasm_bindgen::prelude::*;\n'
            '#[wasm_bindgen] pub fn debug() -> bool { cfg!(debug_assertions) }\n')
        (web / "app.ts").write_text(
            'import init, { debug } from "../.snap/bindings/build_web_contract.js";\n'
            'export default async (wasm) => { await init({module_or_path:wasm}); '
            'return {debug:debug(), mode:process.env.NODE_ENV}; };\n')
        (web / "host.ts").write_text(
            'import app from "snap:application"; globalThis.checkBuild = app;\n')
        (self.root / "snap.toml").write_text(self.config + '''
[web]
package-dir="web"
application="web/app.ts"
host="web/host.ts"
html="web/index.html"
wasm-manifest="web/wasm/Cargo.toml"
bindings=".snap/bindings"
''')
        # Reuse dependency compilation and any installed real binding tool. The CLI
        # still discovers the fixture's artifacts and checks the tool's version.
        self.env["CARGO_TARGET_DIR"] = str(ROOT / "target")
        tools = sorted((ROOT / "apps/healthy/.snap/tools").glob("wasm-bindgen-*/bin"))
        self.env["PATH"] = os.pathsep.join([*(str(path) for path in tools), self.env["PATH"]])
        for profile, flags, expected in [
            ("debug", [], {"debug": True, "mode": "development"}),
            ("release", ["--release"], {"debug": False, "mode": "production"}),
        ]:
            with self.subTest(profile=profile):
                result = self.run_cli("build", *flags, timeout=120)
                self.assertEqual(result.returncode, 0, result.stderr)
                package = self.root / ".snap/build" / profile / "web"
                probe = subprocess.run(["bun", "--eval",
                    'await import("./main.js"); console.log(JSON.stringify(await '
                    'globalThis.checkBuild(await Bun.file("snap_client_wasm_bg.wasm").arrayBuffer())));'],
                    cwd=package, env=self.env, capture_output=True, text=True, timeout=10)
                self.assertEqual(probe.returncode, 0, probe.stderr)
                self.assertEqual(json.loads(probe.stdout), expected)

    def test_sibling_selection_and_example_target_produce_independent_packages(self):
        (self.root / "src/main.rs").write_text('fn main() { println!("first"); }')
        sibling = self.root / "sibling"
        (sibling / "examples").mkdir(parents=True)
        (sibling / "Cargo.toml").write_text(
            '[package]\nname="other-project"\nversion="0.0.0"\nedition="2024"\n[workspace]\n')
        (sibling / "examples/other.rs").write_text('fn main() { println!("second"); }')
        (sibling / "snap.toml").write_text(
            self.config.replace('application="fixture"', 'application="other"')
            .replace('bin="cli-fixture"', 'example="other"'))
        for args in [["build"], ["build", "sibling"]]:
            result = self.run_cli(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
        for root, name, expected in [(self.root, "cli-fixture", "first"),
                                     (sibling, "other", "second")]:
            result = subprocess.run([str(root / ".snap/build/debug" / name)],
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.stdout.strip(), expected)
        (sibling / "snap.toml").write_text("invalid config")
        result = self.run_cli("build", cwd=sibling / "examples")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(str(sibling / "snap.toml"), result.stderr)

    def test_failures_keep_the_previous_package_and_recover(self):
        result = self.run_cli("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = self.root / ".snap/build/debug/cli-fixture"
        (self.root / "snap.toml").write_text(self.config +
            '\n[prepare]\nbuild=[["python3","-c","import sys;sys.exit(42)"]]\n')
        result = self.run_cli("build")
        self.assertEqual(result.returncode, 42, result.stderr)
        self.assertEqual(subprocess.run([str(artifact)], timeout=5).returncode, 37)
        (self.root / "snap.toml").write_text(self.config)
        (self.root / "src/main.rs").write_text('fn main() { missing_symbol(); }')
        result = self.run_cli("build")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing_symbol", result.stderr)
        self.assertEqual(subprocess.run([str(artifact)], timeout=5).returncode, 37)
        (self.root / "src/main.rs").write_text('fn main() {}')
        result = self.run_cli("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(subprocess.run([str(artifact)], timeout=5).returncode, 0)

    def test_build_leaves_the_configured_listener_alive(self):
        peer = subprocess.Popen(["python3", "-c",
            'import socket,time; s=socket.socket(); s.bind(("127.0.0.1",0)); '
            's.listen(); print(s.getsockname()[1],flush=True); time.sleep(60)'],
            stdout=subprocess.PIPE, text=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(peer.stdout, selectors.EVENT_READ)
                self.assertTrue(selector.select(5), "Listener never became ready")
                port = int(peer.stdout.readline())
            (self.root / "snap.toml").write_text(
                self.config.replace("127.0.0.1:0", f"127.0.0.1:{port}"))
            result = self.run_cli("build")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIsNone(peer.poll())
            with socket.create_connection(("127.0.0.1", port), timeout=2):
                pass
            # Runtime-only overrides cannot break a build.
            self.env["SNAP_ADDR"] = "not a listen address"
            result = self.run_cli("build")
            self.assertEqual(result.returncode, 0, result.stderr)
        finally:
            peer.kill()
            peer.wait(timeout=5)
            peer.stdout.close()

    def test_interrupt_releases_descendants_and_the_project_build_lock(self):
        child = ('import socket,signal,time; signal.signal(signal.SIGINT,signal.SIG_IGN); '
                 's=socket.socket(); s.bind(("127.0.0.1",0)); s.listen(); '
                 'print(s.getsockname()[1],flush=True); time.sleep(60)')
        (self.root / "prepare.py").write_text(
            f'import subprocess,time\nsubprocess.Popen(["python3","-c",{child!r}])\ntime.sleep(60)\n')
        (self.root / "snap.toml").write_text(self.config +
            '\n[prepare]\nbuild=[["python3","prepare.py"]]\n')
        process = subprocess.Popen([str(SNAP), "build"], cwd=self.root, env=self.env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                self.assertTrue(selector.select(10), "Preparation never became ready")
                port = int(process.stdout.readline())
            busy = self.run_cli("build", "--release")
            self.assertNotEqual(busy.returncode, 0)
            self.assertIn("Another build is running", busy.stderr)
            process.send_signal(signal.SIGINT)
            process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130)
            with socket.socket() as probe:
                self.assertNotEqual(probe.connect_ex(("127.0.0.1", port)), 0)
        finally:
            stop_cli(process)
            kill_session(process.pid)
            process.stdout.close()
            process.stderr.close()
        (self.root / "snap.toml").write_text(self.config)
        result = self.run_cli("build")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_shared_preparation_precedes_dev_hooks_once(self):
        (self.root / "prepare.py").write_text(
            'import json,sys,os\nfrom pathlib import Path\n'
            'p=Path("calls.json")\n'
            'calls=json.loads(p.read_text()) if p.exists() else []\n'
            'calls.append([os.getcwd(), sys.argv[1:]])\n'
            'p.write_text(json.dumps(calls))\n')
        literal = ["shared", "a b", "", "*", "$(touch bad)"]
        config = (self.config + '\n[prepare]\nbuild=' +
                  json.dumps([["python3", "prepare.py", *literal]]) +
                  '\ndev=[["python3","prepare.py","dev"]]\n')
        (self.root / "snap.toml").write_text(config)
        result = self.run_cli("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads((self.root / "calls.json").read_text()),
                         [[str(self.root), literal]])
        result = self.run_cli("dev")
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual(json.loads((self.root / "calls.json").read_text()),
                         [[str(self.root), literal], [str(self.root), literal],
                          [str(self.root), ["dev"]]])
        self.assertFalse((self.root / "bad").exists())

    def test_build_packages_selected_profiles_without_launching(self):
        (self.root / "src/main.rs").write_text('''fn main() {
            std::fs::write("launched", "yes").unwrap();
            println!("debug={}", cfg!(debug_assertions));
            std::process::exit(37);
        }''')
        for profile, args, cwd, expected in [
            ("debug", ["build"], self.root / "src", "debug=true"),
            ("release", ["build", str(self.root), "--release"], ROOT, "debug=false"),
        ]:
            with self.subTest(profile=profile):
                result = self.run_cli(*args, cwd=cwd)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse((self.root / "launched").exists())
                package = self.root / ".snap/build" / profile
                self.assertFalse((package / "web").exists())
                artifact = package / "cli-fixture"
                self.assertIn(str(artifact), result.stdout)
                executed = subprocess.run([str(artifact)], cwd=package,
                                          capture_output=True, text=True, timeout=5)
                self.assertEqual(executed.returncode, 37, executed.stderr)
                self.assertEqual(executed.stdout.strip(), expected)
        # The output is runnable after copying it away and removing the source/target.
        relocated = self.root / "relocated"
        shutil.copytree(self.root / ".snap/build/release", relocated)
        shutil.rmtree(self.root / "src")
        shutil.rmtree(self.root / "target")
        result = subprocess.run([str(relocated / "cli-fixture")], cwd=relocated,
                                env={"PATH": os.defpath}, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 37)
        self.assertEqual(result.stdout.strip(), "debug=false")


if __name__ == "__main__":
    (ROOT / ".tmp").mkdir(exist_ok=True)
    unittest.main()
