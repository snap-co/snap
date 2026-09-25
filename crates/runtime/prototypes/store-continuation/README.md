# Store continuation experiment, historical evidence

The complete throwaway prototype is preserved in local commit `ee98a70`. It answered
whether the same portable async workflow can return resident data immediately,
suspend on a miss, and resume after host-owned IO with different storage backends.
Its source has been retired in favor of the production interfaces below.

On 2026-09-24, all twelve narrated runs passed through Memory and on-disk SQLite:
cold lookup, completed prefetch, NoCache, stale snapshot rejection, queued-read
cancellation, and accepted-write completion after cancellation. The portable code
compiled with warnings denied for `wasm32v1-none`. The HTML viewer successfully
navigated all recorded traces in Chromium. Native execution was demonstrated;
bare-WASM execution and performance improvements were not measured.

The experiment showed that `.await` need not suspend and that residency is distinct
from authority. Its session-shaped workflow omitted password verification. Its
key/bytes schema, single-thread bridge, and acceptance policy were experimental,
not adopted as the production contract.

Current interfaces and evidence:

- [Current architecture](../../../../ARCHITECTURE.md).
- `crates/store/src/lib.rs`: schema, transaction and advisory snapshot contracts.
- `crates/runtime/src/passport.rs`: real portable password/session continuations.
- `platforms/native/src/store.rs`: shared Memory/SQLite executors.
- `tests/store/contract.rs`: reusable application-facing storage assertions.
- `tests/protocol/carriers.test.ts`: carrier-independent dispatch and accepted
  continuation lifetime through real host IO.
- [Authy compatibility contract](../../../../apps/authy/CONTRACT.md).
