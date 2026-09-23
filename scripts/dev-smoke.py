"""Black-box runner contract: replacement and shutdown release the listening port."""

import json
import os
from pathlib import Path
import re
import selectors
import shlex
import socket
import subprocess
import time
import urllib.request


ROOT = Path(__file__).resolve().parent.parent
COMMAND = shlex.split(os.environ.get("SNAP_DEV_COMMAND", "./bin/dev"))
children = []


def start(address):
    process = subprocess.Popen(
        COMMAND,
        cwd=ROOT,
        env={**os.environ, "SNAP_ADDR": address},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    children.append(process)
    output = b""
    deadline = time.monotonic() + 30
    with selectors.DefaultSelector() as selector:
        selector.register(process.stderr, selectors.EVENT_READ)
        while time.monotonic() < deadline:
            events = selector.select(max(0, deadline - time.monotonic()))
            if not events:
                break
            chunk = os.read(process.stderr.fileno(), 65536)
            if not chunk:
                raise AssertionError(f"Runner exited before listening:\n{output.decode()}")
            output += chunk
            match = re.search(rb"listening on http://(127\.0\.0\.1:\d+)\r?\n", output)
            if match:
                return process, match[1].decode()
    raise AssertionError(f"Runner never became ready:\n{output.decode()}")


try:
    first, address = start("127.0.0.1:0")
    second, replaced_address = start(address)
    assert replaced_address == address
    first.wait(timeout=5)
    with urllib.request.urlopen(f"http://{address}/health/up", timeout=5) as response:
        assert json.load(response)["payload"]["payload"] == {"status": "OK"}
    second.terminate()
    assert second.wait(timeout=5) == 0, "Runner should handle SIGTERM gracefully"
    host, port = address.rsplit(":", 1)
    with socket.socket() as probe:
        probe.settimeout(1)
        assert probe.connect_ex((host, int(port))) != 0, "Runner left its listener behind"
    print("PASS: second dev run replaces the listener; stopping it releases the port")
finally:
    for process in children:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        process.stderr.close()
