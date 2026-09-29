# Snap Rust spike

This is a local portable-capability experiment, exercised through Testy, Authy and
Chatty. Authy owns accounts and OIDC issuance. Chatty uses Authy OAuth and private
Document conversations synchronized between clients through guarded WebSocket operations.

- For setup and development commands, read [README.md](README.md).
- Before changing package boundaries, provider composition, carriers, storage, or
  porting a capability, read [ARCHITECTURE.md](ARCHITECTURE.md).
- Before adding or changing tests, read [TESTING.md](TESTING.md).
- Before changing credentials, sessions, HTTP routes or connection bootstrap, read
  [docs/identity.md](docs/identity.md). Identity acquisition uses Transport's HTTP
  carrier and sets the cookie before the platform opens an authenticated WebSocket.
- After package/dependency changes, run
  `mise exec -- ./bin/snap check apps/testy --structure-only --workspace`.
- Keep portable behavior `no_std` with `alloc`; hosts own execution and external IO.
  Document non-obvious guarantees beside the interface that promises them.
- Update the existing authoritative document when a decision or operating procedure
  changes. Keep task plans and review transcripts out of maintained documentation;
  task completion alone does not require a Markdown file.

This repository has no remote, issue tracker, or publication workflow.
