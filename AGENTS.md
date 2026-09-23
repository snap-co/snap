# Snap Rust spike

This is a local architecture experiment, not a complete Snap port.
Read [README.md](README.md) before changing crate seams or execution flow.
Before adding or changing tests, read [TESTING.md](TESTING.md). It defines consumer
contracts and the verification gates; internal rewrites must preserve the suite.

- Keep application, shared runtime, and client core code `no_std` with `alloc`. Platform crates
  own concrete IO, clocks, randomness, task execution, and environment access.
- Application execution is synchronous input/state/action processing. Add external
  work as host actions and completion inputs when a real tracer requires it.
- Use Rust modules for new concerns. Extract crates when a dependency constraint,
  independent consumer, or portability requirement justifies the split.
- Preserve the selected TypeScript wire behavior before proposing protocol changes.
  Inspect `~/code/bod/snap` as the reference implementation.
- Default behavior tests to the Client SDK or wire Protocol. Rust functions and
  `Module::update` are implementation details for this purpose. Test a lower seam
  only after naming an observable promise the consumer interfaces cannot express.
- Reuse SDK contracts across client implementations. Keep launchers and binding
  construction in adapters; assertions describe the application-facing behavior.
- Match the milestone's scope. Healthy is the HTTP health check only.
- Launch development with `./bin/dev`. It replaces listeners owned by the current
  user on the selected port, default 3846. Tests own ephemeral ports and processes.

This repository currently has no remote, issue tracker, or publication workflow.
