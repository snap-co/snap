# Snap Rust spike

This is a local architecture experiment, not a complete Snap port.
Read [README.md](README.md) before changing crate seams or execution flow.
When porting a module or changing provider composition, carrier handling, or storage,
follow [the module pattern](docs/architecture/module-pattern.md). It records the
architecture and Authy's reference composition.
Before adding or changing tests, read [TESTING.md](TESTING.md). It defines consumer
contracts and the verification gates; internal rewrites must preserve the suite.

- Keep application, shared runtime, and client core code `no_std` with `alloc`. Platform crates
  own concrete IO, clocks, randomness, task execution, and environment access.
- Keep portable behavior IO-free. Hosts drive synchronous turns or poll portable
  futures and execute external work; lifecycle guarantees belong at the interface.
- Use Rust modules for new concerns. Extract crates when a dependency constraint,
  independent consumer, or portability requirement justifies the split.
- When adding a package or dependency, declare its `package.metadata.snap.role` and
  run `snap check apps/healthy --structure-only --workspace`. See README's structural
  checks section for dependency kinds and target/feature coverage.
- Document ownership, ordering, cancellation, recovery, and compatibility exceptions
  beside the interface that promises them. Comments should explain constraints a
  caller cannot infer from the types; compiler/check diagnostics enforce structure.
- Preserve the selected TypeScript wire behavior before proposing protocol changes.
  Inspect `~/code/bod/snap` as the reference implementation.
- Default behavior tests to the Client SDK or wire Protocol. Rust functions and
  `Provider::invoke` are implementation details for this purpose. Test a lower seam
  only after naming an observable promise the consumer interfaces cannot express.
- Reuse SDK contracts across client implementations. Keep launchers and binding
  construction in adapters; assertions describe the application-facing behavior.
- Match the milestone's scope. Healthy is the HTTP health check only.
- Launch development with `./bin/dev`. It replaces listeners owned by the current
  user on the selected port, default 3846. Tests own ephemeral ports and processes.

This repository currently has no remote, issue tracker, or publication workflow.
