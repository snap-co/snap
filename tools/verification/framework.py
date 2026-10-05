#!/usr/bin/env python3
"""Run framework gates in a source copy that cannot reach apps/."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
GATES = ("check", "test", "properties", "io", "browser")
IGNORED = {"target", "node_modules", ".git", ".deployment", ".snap", "dist", "__pycache__"}


def framework_manifest():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    workspace = manifest["workspace"]
    workspace["members"] = [p for p in workspace["members"] if not p.startswith("apps/")]
    if not workspace["members"]:
        raise RuntimeError("no framework workspace members selected")
    workspace["default-members"] = [
        p for p in workspace["members"] if p not in ("tests/properties", "tests/browser")
    ]
    workspace["dependencies"] = {
        name: dep for name, dep in workspace["dependencies"].items()
        if not isinstance(dep, dict) or not dep.get("path", "").startswith("apps/")
    }
    return manifest


def toml_document(document):
    # Cargo accepts dependency and lint tables in expanded form. Preserve every
    # root table instead of maintaining a second copy of workspace configuration.
    lines = []

    def value(item):
        if isinstance(item, (str, int, float, bool, list)):
            return json.dumps(item)
        raise RuntimeError(f"unsupported workspace TOML value: {item!r}")

    def table(items, path):
        if path:
            lines.append("[" + ".".join(json.dumps(part) for part in path) + "]")
        for key, item in items.items():
            if not isinstance(item, dict):
                lines.append(f"{json.dumps(key)} = {value(item)}")
        lines.append("")
        for key, item in items.items():
            if isinstance(item, dict):
                table(item, (*path, key))

    table(document, ())
    return "\n".join(lines)


def commands(gate, root, target):
    cli = str(target / "debug/snap")
    if gate == "check":
        return [
            (["cargo", "build", "-p", "snap-cli"], False),
            ([cli, "check", str(root), "--workspace"], False),
            (["bun", str(root / "node_modules/typescript/bin/tsc"), "--project",
              str(root / "kits/tsconfig.json"), "--noEmit"], False),
            ([cli, "check-deps", str(root)], False),
        ]
    if gate == "test":
        return [(["python3", "-m", "unittest", "discover", "-s", "tools/verification/tests", "-v"], True),
                (["cargo", "test", "--workspace", "--exclude", "snap-core-properties",
                  "--exclude", "snap-browser-tests", "--features", "snap-identity-native/passkey"], True)]
    if gate == "properties":
        return [(["cargo", "test", "-p", "snap-core-properties", "--", "--nocapture"], True)]
    if gate == "io":
        return [
            (["cargo", "test", "-p", "snap-cli", "--test", "check", "--test", "check_contract",
              "--test", "application", "--", "--ignored", "--skip", "fixture_child"], True),
            (["cargo", "test", "-p", "snap-platform-tests", "--test", "host_tcp",
              "--test", "document_sync", "--", "--ignored"], True),
            (["cargo", "test", "-p", "snap-transport-tcp", "--test", "tls", "--", "--ignored"], True),
            (["cargo", "test", "-p", "snap-store-sqlite", "--test", "recovery", "--", "--ignored"], True),
            (["cargo", "test", "-p", "snap-crypto", "--test", "native", "--test", "token",
              "--", "--ignored"], True),
        ]
    if gate == "browser":
        return [(["cargo", "run", "-p", "snap-browser-tests", "--", "--prepare", "all"], False)]
    raise RuntimeError(f"unknown gate {gate}")


def checkout(manifest, scratch):
    destination = scratch / "source"
    destination.mkdir()
    for name in ("crates", "kits", "tools", "tests", "bin"):
        shutil.copytree(ROOT / name, destination / name, symlinks=True,
                        ignore=lambda _path, names: IGNORED.intersection(names))
    for name in ("Cargo.lock", "mise.toml", "deny.toml", "bun.lock", "tsconfig.json"):
        shutil.copyfile(ROOT / name, destination / name)
    (destination / "Cargo.toml").write_text(toml_document(manifest))
    package = json.loads((ROOT / "package.json").read_text())
    package.pop("workspaces", None)
    (destination / "package.json").write_text(json.dumps(package, indent=2) + "\n")
    # Installed compiler/browser dependencies are external tools, not app source.
    if (ROOT / "node_modules").is_dir():
        (destination / "node_modules").symlink_to(ROOT / "node_modules", target_is_directory=True)
    for path in destination.rglob("*"):
        if path.is_symlink() and path.name != "node_modules":
            try:
                relative = path.resolve().relative_to(ROOT)
            except ValueError:
                continue
            if relative.parts and relative.parts[0] == "apps":
                raise RuntimeError(f"framework source symlink reaches an application: {path}")
    return destination


class Interrupted(KeyboardInterrupt):
    def __init__(self, signum):
        self.signum = signum
        super().__init__(f"interrupted by {signal.Signals(signum).name}")


def interrupted(signum, _frame):
    raise Interrupted(signum)


def retire(process, signum):
    # Gates own a separate process group. Reaping its leader does not prove that
    # its listeners or builds exited; always retire the remaining group as well.
    def send(signum):
        try:
            os.killpg(process.pid, signum)
        except ProcessLookupError:
            pass

    send(signum)
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    finally:
        send(signal.SIGKILL)
        process.wait()


@contextmanager
def owned_process(command, root, environment, **options):
    previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGINT, signal.SIGTERM)}
    process = None
    cancellation = signal.SIGTERM
    try:
        process = subprocess.Popen(command, cwd=root, env=environment, text=True,
                                   start_new_session=True, **options)
        yield process
    except Interrupted as error:
        cancellation = error.signum
        raise
    finally:
        # A repeated interrupt must not bypass teardown of this owned group.
        for sig in previous:
            signal.signal(sig, signal.SIG_IGN)
        try:
            if process is not None:
                retire(process, cancellation)
                if process.stdout is not None:
                    process.stdout.close()
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)


def run(command, root, environment, require_tests):
    print("+ " + shlex.join(command), flush=True)
    count = 0
    with owned_process(command, root, environment, stdout=subprocess.PIPE,
                       stderr=subprocess.STDOUT) as process:
        for line in process.stdout:
            print(line, end="", flush=True)
            match = re.search(r"test result: ok\. (\d+) passed;|^Ran (\d+) tests? in ", line)
            if match:
                count += int(match.group(1) or match.group(2))
        status = process.wait()
    if status:
        raise RuntimeError(f"command exited with status {status}: {shlex.join(command)}")
    if require_tests and not count:
        raise RuntimeError(f"no passing cases selected: {shlex.join(command)}")


def verify_local_graph(root, environment):
    command = ["cargo", "metadata", "--format-version=1", "--all-features"]
    with owned_process(command, root, environment, stdout=subprocess.PIPE) as process:
        output, _ = process.communicate()
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command)
    for package in json.loads(output)["packages"]:
        if package["source"] is None:
            manifest = Path(package["manifest_path"]).resolve()
            if not manifest.is_relative_to(root):
                raise RuntimeError(f"framework dependency escapes its source copy: {manifest}")


def main():
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, interrupted)
    parser = argparse.ArgumentParser(description=__doc__, epilog=
        "All gates run without apps/. Unsupported Wasm/browser conformance setups remain "
        "outside the current shared platform matrix; browser runs the existing adapter contracts.")
    parser.add_argument("gate", nargs="?", default="all", choices=(*GATES, "all"))
    parser.add_argument("--list", action="store_true", help="print selected members and commands without building")
    parser.add_argument("--keep", action="store_true", help="retain this task's isolated checkout and target after success")
    args = parser.parse_args()
    manifest = framework_manifest()
    selected = GATES if args.gate == "all" else (args.gate,)
    print("Framework members: " + ", ".join(manifest["workspace"]["members"]), flush=True)
    print("Shared conformance does not yet cover Wasm execution, browser client carriers, "
          "client-side durable recovery or the full Transport/Store matrix.", flush=True)
    if args.list:
        for gate in selected:
            print(f"[{gate}]")
            for command, _ in commands(gate, Path("<framework>"), Path("<target>")):
                print(shlex.join(command))
        return
    cache = Path.home() / ".cache/coding-agents"
    cache.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="snap-framework-", dir=cache))
    print(f"Isolated verification: {scratch}", flush=True)
    try:
        root = checkout(manifest, scratch)
        target = scratch / "target"
        environment = dict(os.environ, CARGO_TARGET_DIR=str(target), TMPDIR=str(cache),
                           SNAP_BROWSER_ROOT=str(root))
        verify_local_graph(root, environment)
        for gate in selected:
            print(f"\n[{gate}]", flush=True)
            for command, require_tests in commands(gate, root, target):
                run(command, root, environment, require_tests)
    except BaseException:
        print(f"Verification incomplete; retained source and evidence at {scratch}", file=sys.stderr)
        raise
    print("Framework verification passed: " + ", ".join(selected), flush=True)
    if not args.keep:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    try:
        main()
    except Interrupted as error:
        print(f"framework verification: {error}", file=sys.stderr)
        sys.exit(128 + error.signum)
    except (RuntimeError, OSError, subprocess.CalledProcessError, KeyboardInterrupt) as error:
        print(f"framework verification: {error}", file=sys.stderr)
        sys.exit(1)
