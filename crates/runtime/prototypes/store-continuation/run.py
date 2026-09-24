#!/usr/bin/env python3
"""Throwaway probe: compile portable Rust, run host scenarios, render their trace."""
import json
from pathlib import Path
import subprocess

here = Path(__file__).resolve().parent
root = here.parents[3]
output = Path("/tmp/opencode/store-continuation-prototype")
output.mkdir(parents=True, exist_ok=True)

subprocess.run([
    "rustc", "--edition=2024", "--crate-type=rlib",
    "--crate-name=store_continuation_prototype", "--target=wasm32v1-none",
    "--deny=warnings", str(here / "portable.rs"),
    "-o", str(output / "portable.rlib"),
], cwd=root, check=True)
print("PASS portable futures/cache/request code compiles for wasm32v1-none", flush=True)
result = subprocess.run([
    "cargo", "run", "--quiet", "-p", "snap-native",
    "--example", "store-continuation-prototype",
], cwd=root, check=True, text=True, stdout=subprocess.PIPE)
traces = json.loads(result.stdout)
(output / "trace.json").write_text(result.stdout)
html = (here / "demo.html").read_text().replace(
    "/* RECORDED_TRACES */ []", json.dumps(traces).replace("</", "<\\/")
)
(output / "demo.html").write_text(html)
print(f"Recorded {len(traces)} scenarios. Open {output / 'demo.html'}")
