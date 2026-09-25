# Host-driven module refactor: independent Spec review, round 1

## Verdict

**BLOCKED.** Two blocking findings affect the declared Store namespace guarantees and existing HTTPS cookie compatibility. One follow-up affects Passport when an application selects the supplied MemoryCache. The main host-driven execution, authority transactions, and legacy Authy migration paths follow the accepted design.

This is one independent, read-only review in round 1 of the new two-total-round budget. No review was delegated. Findings are recorded here for coordinator disposition.

## Revisions and scope

- Repository: `/home/cc444/code/snapco/snap`.
- Immutable base: `ee98a70afac25a3caee8a0306db633efb1ab18c1`.
- Reviewed HEAD: `a47a4018c7755c548eeb4d3bac55edc7ca72afe6`.
- Sole commit: `Implement host-driven providers, shared Store and carrier bindings`.
- Scope: `git diff ee98a70afac25a3caee8a0306db633efb1ab18c1...a47a4018c7755c548eeb4d3bac55edc7ca72afe6` and affected interactions. The coordinator's captured diff is `/tmp/opencode/module-pattern-round1.diff`; inspection used Git and source files directly.
- HEAD matched at the start and end. The only tracked working-tree modification was the permitted coordinator-owned `docs/reviews/host-driven-module-pattern.md`.
- Selected TypeScript reference: `9689a8ed3108f58233721c2000d2b9ea96259fe7` in `/home/cc444/code/bod/snap`. I used `git show` for Identity declarations and Passport session lifecycle at that revision, rather than the checkout's current files.

Contracts read: `AGENTS.md`, `README.md`, `TESTING.md`, accepted ADR `docs/adr/0001-host-driven-capability-providers.md`, `docs/plans/host-driven-module-pattern.md`, `docs/architecture/module-pattern.md`, `docs/architecture/store.md`, and the observable compatibility portions of `docs/plans/authy-password-sessions.md`. The ADR supersedes the old manual-stage and Lane implementation choices.

## Findings

### SPEC-1: SQLite can alias two accepted logical namespaces

- Disposition: **BLOCKER**.
- Severity: **High**. Valid Store declarations can silently share records that Memory keeps separate.
- Location: `platforms/native/src/store.rs:440-468`, especially the case-sensitive physical-name collision check at line 457. Registration at lines 61-85 and 130-135 also treats registry keys case-sensitively while SQLite resolves table identifiers case-insensitively.
- Contract: `docs/architecture/store.md:15-24` promises physical collision rejection and recorded logical ownership. `crates/store/src/lib.rs:167-173` and `docs/plans/host-driven-module-pattern.md:77-87` require interchangeable declared atomic semantics through Memory and SQLite. ADR lines 38-42 assign each module its namespace.

Trigger: register two schemas with identical columns and primary keys, no indexes or foreign keys, and tables `Table { namespace: "Alpha", name: "records" }` and `Table { namespace: "alpha", name: "records" }`. Both names satisfy `valid`. Their physical strings differ, so `validate_schemas` accepts them.

Precise path: Memory creates two distinct BTreeMap entries. SQLite creates `Alpha_records`; its later `CREATE TABLE IF NOT EXISTS "alpha_records"` reuses that table. Column and primary-key verification passes for the identical shapes. The case-sensitive `snap_store_schemas` primary key admits both ownership records. A query addressed to either logical table then accesses the same physical data. Registering the second spelling in a later opening also bypasses the exact-key registry lookup.

Observed evidence: the isolated Python probe executed the same generated SQLite registration statements for those shapes. It recorded both `Alpha.records` and `alpha.records`, but SQLite contained only `Alpha_records`. Inserting `only-in-Alpha` through `Alpha_records` made it readable through `alpha_records`. This was a SQLite-level probe, not a compiled invocation of the Rust Store API; the validation and registration paths above establish that the public constructors reach these statements.

Bounded remedy: compare physical ownership and reserved names using SQLite's case-insensitive identifier rules, both within the supplied declarations and against persisted registrations. Apply the same declaration collision rule to Memory. Add a shared Store contract for this accepted-name collision and an SQLite reopen case proving the second owner is rejected without changing existing data.

### SPEC-2: HTTPS origin normalization no longer determines cookie security

- Disposition: **BLOCKER**.
- Severity: **Medium**. A previously accepted HTTPS configuration changes cookie names, loses existing-session recognition, and emits cookies without Secure.
- Location: `apps/authy/native/src/main.rs:49-53`, specifically `origin.starts_with("https:")` at line 52. The host separately parses and normalizes that origin in `platforms/native/src/lib.rs:171-185`; `platforms/native/src/cookie.rs:27,71-75` consumes the inconsistent boolean.
- Contract: `docs/plans/authy-password-sessions.md:74-76` requires `__Host-authy_session` and Secure for a configured HTTPS origin. `README.md:32-35` states the same guarantee. `docs/plans/host-driven-module-pattern.md:38-43` requires preserved sessions and observable compatibility.

Trigger: launch Authy with the valid URL `SNAP_ORIGIN=HTTPS://example.test`. URL schemes are case-insensitive. The existing base implementation parsed the URL first and selected security with `origin.scheme() == "https"`. HEAD instead performs a case-sensitive test on the raw environment string. The host still accepts and normalizes this origin to `https://example.test`.

Observed evidence: using a private copy of the existing Authy executable, two fresh temporary databases, OS-assigned ports, and the same account-creation wire request with `Origin: https://example.test`:

| Configured origin | HTTP status | Cookie name | Secure attribute |
| --- | --- | --- | --- |
| `https://example.test` | 200 | `__Host-authy_session` | present |
| `HTTPS://example.test` | 200 | `authy_session` | absent |

Both responses were successful empty completions with `sessionChanged: true`. Existing `__Host-authy_session` cookies are also ignored under the uppercase spelling because Cookie::read matches the newly selected name exactly.

Bounded remedy: derive cookie security from the same parsed, validated origin used by the carrier. Preserve accepted URL normalization. Add a wire-level regression using the uppercase HTTPS scheme and verify secure cookie naming and existing-cookie recognition.

### SPEC-3: Cached absence can prevent login after successful enrollment

- Disposition: **FOLLOW_UP**.
- Severity: **Medium**. This affects the newly supplied cache-enabled Passport composition; the shipped Authy composition selects NoCache.
- Location: `crates/runtime/src/passport.rs:269-272`, with the empty snapshot retained by `crates/store/src/lib.rs:195-207` and `platforms/native/src/store.rs:423-435`.
- Contract: `docs/plans/host-driven-module-pattern.md:99-101` requires cache hits and prefetch to preserve authoritative checks. `docs/architecture/module-pattern.md:95-98` distinguishes snapshots from authority. `docs/architecture/store.md:63-66` describes cache-enabled credential lookup with authoritative fallback before password rejection.

Supported trigger using existing operations: an application selects `MemoryCache::new(8)` for Passport. A password-acquire attempt for an email with no account caches an empty credential query. `account.create` then successfully enrolls that email. After sign-out, password acquisition for the new account reuses the cached empty query and immediately returns `InvalidCredentialError`, even with the correct password.

Precise path evidence: `snapshot` stores empty row sets. Enrollment does not invalidate or update that query. At line 272, `rows.pop().ok_or_else(bad_credential)?` returns before the authoritative fallback at lines 279-286. MemoryCache has no expiry, so repeated login attempts can continue failing until eviction or cache replacement. Dropping the advisory cache changes the login result back to success. The later transaction guard correctly protects successful session creation, but cannot repair this early rejection.

Bounded remedy: reload Store authority before rejecting a cached absence, as the provider already does for failed verification against a cached hash. Exercise the enrollment-after-negative-lookup sequence through a cache-enabled application adapter. No new authentication feature is needed.

## Coverage and confirmed alignment

- Contract/provider separation: inspected Protocol, Identity and Store declarations, metadata roles, the structural checker change, application composition, and the new architecture contract. Identity no longer imports Passport or exposes password hashes. Store does not publish operations.
- Carrier independence: inspected removal of Lane from operation declarations and provider dispatch, explicit web bindings, native routing/socket admission, codec extraction, and native/browser client normalization. The independent carrier contract passed for one operation exposed through HTTP and WebSocket.
- Host ownership and accepted work: inspected the single scheduler, owned futures, private response slots, semaphore lifetime, observer-loss handling, completion projection and revocation broadcast. The new scheduler retains the base's pre-dispatch closed-observer check. Once invoked, a continuation is held in FuturesUnordered until completion, independently of its observer. The wire lifecycle test passed.
- Storage: inspected typed validation, Memory candidate-state publication, SQLite immediate transactions, guards, statement ordering, foreign keys, unique constraints, commit/error handling, namespace registration and transactional legacy renames. Existing shared Store tests passed through both backends, including separate SQLite connections.
- Authy authority and compatibility: inspected account/session atomic creation, digest resolution, credential/hash fencing, protected-read and revoke guards, revocation delivery, signing-key persistence and cookie extraction. Compared relevant base implementation paths. The legacy migration wire test passed with existing accounts, hashes, sessions, signing key and restart behavior.
- Cache: Authy's NoCache selection keeps its authority reads current. Transactional guards do not read MemoryCache. SPEC-3 concerns the optional supplied cache composition.
- Healthy, clients and tooling: inspected affected Healthy completion normalization, native/browser adapters, structural roles and verification commands. Full browser, HMR, packaging and CLI verification was supplied or visible in the coordinator log, rather than independently rerun here.
- Prototype retirement: the retained README identifies the base commit containing the prototype and records its results and limitations. The implementation stays within the selected local Store and Authy scope.

## Checks performed by this reviewer

1. Verified full HEAD, sole-commit range and allowed working-tree state. `git diff --check BASE...HEAD` passed.
2. Ran the existing compiled test executable directly, without Cargo or a build:
   `target/debug/deps/store_contract-e50395d7e3e1236b --nocapture`.
   Result: **2 passed**, Memory and SQLite, approximately 0.01 seconds.
3. Ran `bun test tests/protocol/carriers.test.ts tests/protocol/migration.test.ts`.
   Result: **2 passed**, 11 assertions, approximately 1.06 seconds.
4. Ran `python3 /tmp/opencode/module-pattern-spec-probe.py`.
   Result: reproduced SPEC-2 through HTTP and the SQLite aliasing behavior supporting SPEC-1. The script copied `target/debug/authy` into an owned temporary directory, used fresh databases and ephemeral ports, terminated its processes and removed those resources. Its executable SHA-256 was `42fed8387c5f831667b755c6822e17bc939c36993061eb05c371c693a36560c8`.
5. Read the tail of `/tmp/opencode/module-pattern-full-check.log`. At inspection it showed eight recovery cases passing, TypeScript Authy interoperability passing, nine browser cases passing, and the final dev-listener smoke check passing. I did not own that process or independently collect its exit status.

The probe script remains at `/tmp/opencode/module-pattern-spec-probe.py` for reproducibility. No product files, tracked tests, review ledger, real database, or generated `.snap` outputs were edited by this reviewer. No builds or full suites were launched, and no commits, publication or tracker operations were performed.

## Supplied evidence and limits

The coordinator supplied passing workspace all-target compilation, warnings-denied Clippy, initial bare-WASM structural checks, Store contracts, carrier/lifecycle contracts, Authy wire/migration and eight native/browser recovery cases, plus TypeScript checks. Final structural checking was included in its full gate. Those statements are supplied evidence, distinct from the focused checks above.

This is a Spec review of the changed design and affected interactions, not a repository-wide audit or independent certification of every gate. SPEC-1 has precise Rust code-path evidence plus an SQLite-level probe. SPEC-3 has code-path evidence and was not executed in a custom cache-enabled binary because this assignment prohibited builds. Existing binaries were reused for executed consumer checks. I did not test crash recovery, distributed coherence, new authentication features, or undeclared backends; none is required by the accepted scope.

The coordinator should resolve SPEC-1 and SPEC-2 before marking this axis CLEAR. SPEC-3 is separately recorded for disposition because current Authy selects NoCache. No second review round was initiated.
