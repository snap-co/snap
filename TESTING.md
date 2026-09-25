# Testing

Behavior assertions should survive replacement of the server or client runtime.
Change launchers and adapters when implementations change, rather than rewriting
the promises under test.

## Choose the consumer interface

- SDK contracts exercise operations, observations, errors, and client lifetime.
- Protocol contracts check wire details the SDK hides, with independent examples
  so a shared encoding bug cannot make both sides agree on the wrong behavior.
- Host CLI contracts exercise real commands, artifacts, and process lifetime.
- Browser scenarios cover packaging, rendering, and development reload behavior.

Keep one primary assertion per promise. Reuse scenarios across client adapters;
put construction and cleanup in adapters. Tests should assert observable results,
not private state, SQL text, helper calls, or incidental event ordering.

Before adding a lower-level contract, name the promise the consumer interfaces
cannot express. Store's coherent cross-namespace reads and atomic guarded writes
justify its shared Memory/SQLite contract. Calling `Provider::invoke` merely to
retest application behavior does not.

Fixtures own temporary data and processes, use ephemeral ports, and clean up on
failure. Synchronize on readiness or completion; timeouts bound stalled tests.
Development-port replacement belongs only in the runner's lifecycle tests.

## Run checks

```sh
# Full repository gate; bin/check owns the command sequence:
mise exec -- ./bin/check

# Selected application's gate:
mise exec -- ./bin/snap check apps/authy
mise exec -- ./bin/snap check apps/healthy

# Required after package/dependency changes:
mise exec -- ./bin/snap check apps/healthy --structure-only --workspace

# Optional network-backed dependency advisory check:
mise exec -- ./bin/check-deps --audit
```

Install Chromium once with `bunx playwright install chromium`. The full gate also
requires `~/code/bod/snap` with its TypeScript dependencies installed. Ordinary
builds and application checks do not require that reference checkout.
`SNAP_REFERENCE` selects another checkout for `scripts/healthy-smoke.ts`.

During iteration, run the check for the changed interface. Test entry points live
under `tests/{sdk,protocol,browser,cli,store,journeys}`. Consult `bin/check` and the
app's `snap.toml` for build prerequisites and invocation details instead of copying
their command sequences here. Run the relevant full gate at a milestone; repeat
after relevant changes or failures.

Formatting, Clippy, Rustdoc, dependency policy, and structural checks enforce
source constraints separately from behavior tests.
