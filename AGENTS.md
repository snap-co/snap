# Snap Rust spike

- Keep portable behavior `no_std` with `alloc`; hosts own execution and external IO.
- This project is undeployed. Edit initial version-1 schemas in place and recreate
  affected local development databases when needed. Prefer fresh schemas over
  upgrade migrations or compatibility paths; keep deployment secrets separate.
- Document non-obvious guarantees beside the interface that promises them.
- Read implementation, configuration, tests and command help directly.
- Before adding, changing, moving or reviewing tests, harnesses, fixtures, test
  routing or testing checks, read [TESTING.md](TESTING.md). Update it when harness
  seams or ownership change.
- Keep Markdown for agreed terminology and live agent instructions. Put application
  prompts in the application's `prompts/` directory.
- Do not maintain implementation narration, operating manuals or completed-work
  records. Task completion does not require a document.
