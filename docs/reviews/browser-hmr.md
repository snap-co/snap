# Browser HMR review

## Human-authorized recovery checkpoint

On 2026-09-23 the user reviewed STD-2 / SPEC-5 and changed the startup design.
Snap dictates both addresses, kills existing current-user listeners by default,
and probes HTTP readiness. The server binds its supplied port or exits; it does
not choose a replacement. Stderr is diagnostic output only. The user authorized
recording this ruling and continuing implementation and the remaining plan.

Recovery starts from `6389c52`. Original rounds 1 and 2 below remain valid history.
One additional recovery validation will inspect this explicit contract amendment,
the resolved startup blocker, and affected port/process/proxy behavior. It is not
a reset of the milestone's review counter. The initial failed repair confused pipe
closure with process exit. Removing that coupling gives readiness one HTTP deadline,
with independent child-exit and interruption handling. Regression cases cover both
orderings, plus a working server with redirected stderr and prescribed-port replacement.
Recovery implemented. `mise exec -- ./bin/check` passed, including all 11 dev,
7 build, 4 check, 5 architecture CLI cases, three Chromium scenarios, SDK/protocol/
journey, portability, dependency policy, docs, and both-port listener replacement.
The first gate exposed two old launcher assertions: expecting port zero in the child
and treating a forwarded host log as public readiness. They now assert Snap selects
a nonzero port and wait for the CLI's own ready line. No test was skipped.
The reqwest dependency reuses the workspace's existing version; CLI role is tool.
Recovery validation, round 3 authorized by the human, recorded before dispatch.

Slice 05 of `docs/plans/project-tooling-and-authy.md`. Base
`629c46944d1dd79ded7c272b2574798202e77ba5`. Linux, trusted projects, main branch.
Vite 8.3.0 and React plugin 6.1.1 are pinned JS development tools. Snap embeds its
driver, resolves tooling from web.package-dir, and retains app-definition/shared-host
composition. React Fast Refresh and CSS updates preserve compatible component/page/
Rust client lifetime. Incompatible exports may use Vite's documented reload fallback.
Rust watching remains slice 06. Releases remain static packages.

The frontend owns the public address and proxies unmatched application HTTP to the
native host on an OS-assigned loopback port. Original Host, Origin, and cookie
headers are retained. Existing application routes and Build discovery retain their
URLs. Resident service startup consumes readiness, owns process groups, propagates
early host failure codes, and stops both services on either exit or CLI interruption.
No WebSocket application transport is promised in Healthy's HTTP-only scope.

Verification: `mise exec -- ./bin/check` passed on 2026-09-23. Includes 9 dev, 7 build,
4 check, 5 architecture CLI tests; Rustdoc, dependency policy and portable compilation;
all SDK/protocol/journey contracts; three Chromium tests; listener replacement.
The new browser contract edits owned source copies, proves state-preserving React
and CSS updates, stable Build identity, and closure of both listeners. It failed at
the missing updated heading before implementation. The early web host failure test
first reproduced lost exit status, then passed after repair. TypeScript passed.

Round 1 recorded before dispatch. Standards and Spec reviewers inherit Astra under
the harness's model override policy. Two-round limit applies to this milestone;
earlier milestone reviews are closed.

Round 1 SHA: `9389e4367d13d5d9fa09b41867a4af4b0629d4a3`.
Standards session `ses_f2ee90e5bffelGkK22kapMipz5`, BLOCKED; complete report retained
at `/tmp/opencode/hmr-standards-r1.md`. Spec session
`ses_f2ee90e1dffeX2BCfe4Q7vxjGb`, BLOCKED; complete report retained at
`/tmp/opencode/hmr-spec-r1.md`. These are the original unabridged reviewer artifacts.

| Finding | Disposition and repair |
| --- | --- |
| STD-1 / SPEC-3 | Accepted. Race service readiness with direct child exit, release the process group before draining logs, preserve exit status. CLI regression now spawns an inherited-stderr descendant before exiting 37. |
| SPEC-1 | Accepted. Disable Vite CORS so OPTIONS reaches native method handling. Browser launch test compares public/private method rejection. |
| SPEC-2 | Accepted. Preserve native hostname acceptance with allowedHosts=true in this trusted local development driver. Check custom-host health and frontend module requests. |
| SPEC-4 | Accepted. Interpret URL's normalized empty HTTP port as 80, retaining explicit port zero. Reviewer confirmed original URL normalization through Bun; no existing port-80 listener was replaced. |

One repair batch complete. Full `mise exec -- ./bin/check` passed again, including
the extended descendant and proxy regressions; `git diff --check` passed.
Round 2 recorded before dispatch. It will validate this delta and affected
interactions only. No third round or second autonomous repair batch is authorized.

## Final status: BLOCKED

Round 2 reviewed `da14da4e0fbcbda99da3d8b312913a8a7c17e714`. Both reviewers confirmed
every original finding resolved, but independently reproduced the same repair-induced
blocker, STD-2 / SPEC-5. A host that closes stderr without announcing readiness and
stays alive bypasses the existing 20-second startup deadline. The fallback wait at
`tools/cli/src/process.rs:75-80` now sits outside the timeout. Both real-CLI probes
remained waiting past 25 seconds, then cleaned up under explicit termination.

The passing repository suite does not cover this state. This is a confirmed code
and verification gap, not an environment failure or disputed review. The repair
covered parent exit with inherited stderr but missed the opposite ordering, stderr
closure while the parent remains alive.

Standards final report: `/tmp/opencode/hmr-standards-r2.md`, session
`ses_f2ee90e5bffelGkK22kapMipz5`. Spec final report:
`/tmp/opencode/hmr-spec-r2.md`, session `ses_f2ee90e1dffeX2BCfe4Q7vxjGb`.
Both complete reports are reproduced below. Original round-1 reports remain at the
paths above. Two rounds used. No further product repair or review was attempted.

Human decision requested: authorize one bounded repair and validation of the
closed-stderr startup case, then resume slice 06 if clear. Proposed repair keeps
one deadline around the entire pre-readiness phase while racing child exit. Add a
real-CLI regression for stderr closure with a live child, retaining the inherited-
stderr early-exit regression. Validate readiness, EOF, parent exit, timeout, and
interruption as distinct transitions before resuming the remaining milestone.

Slice 06 is unstarted. Slices 01–04 remain READY under their completed reviews.
No remote or publication occurred; the blocked code is preserved locally on main.

## Complete round 2 Standards report

# Slice 05 Standards fix validation, final round 2

Axis result: **BLOCKED**. STD-1 is resolved. A fix-induced startup-timeout regression remains unresolved as STD-2. The two-round limit requires human intervention before further repair or review.

## Revisions and scope

- Repository: `/home/cc444/code/snapco/snap`
- Prior reviewed head: `9389e4367d13d5d9fa09b41867a4af4b0629d4a3`
- Repair head: `da14da4e0fbcbda99da3d8b312913a8a7c17e714`
- Repair commit: `da14da4 Preserve native proxy behavior and early service failure status`
- Original milestone base: `629c46944d1dd79ded7c272b2574798202e77ba5`

Validated the fixed repair diff and `docs/reviews/browser-hmr.md` ledger against the prior Standards report. Scope was STD-1 and regressions introduced by this repair, including the shared exit-status helper and readiness/cleanup interactions. The frontend configuration and consumer-test changes were inspected for repair-induced standards issues. Unaffected implementation was not re-audited. Linux trusted-project assumptions and the HTTP-only scope remain in force.

Both revision references resolved. HEAD matched the repair revision, and tracked files were clean before and after validation.

## Finding disposition

### STD-1 / SPEC-3: Resolved

The inherited-stderr early-exit regression is repaired at `tools/cli/src/process.rs:84-88`:

- Startup now races the actual child exit against readiness.
- `service.finish()` drops the process group before waiting for the log task, so inherited stderr cannot keep the reader alive after host termination.
- `successful(status)` retains the prior status and signal mapping used by both ordinary commands and resident services.
- `tests/cli/dev.py:83-99` extends the consumer-level regression with an inherited-stderr `sleep 60` descendant and the expected status 37.

Independent real-CLI reproduction returned **37 in 0.26 seconds**, compared with status 1 after 20.21 seconds in round 1. The descendant was no longer running before fixture fallback cleanup. This resolves the assigned finding, rather than merely hiding its timeout.

### STD-2: Stderr EOF disables the startup deadline

- Disposition: **BLOCKER**, unresolved, fix-induced regression.
- Severity: Medium.
- Location: `tools/cli/src/process.rs:75-80`, specifically `service.wait(self).await?` in the readiness-channel error branch.
- Existing behavior and contract basis: Snap owns service readiness and failure handling, as recorded in `docs/reviews/browser-hmr.md:14-15`. At prior revision `9389e43`, `process.rs:74-85` wrapped the entire readiness future, including the stderr-EOF wait, in a 20-second timeout. Startup therefore remained bounded when the host closed stderr without announcing readiness. This finding preserves that existing failure behavior; it does not add a new protocol or timeout requirement.
- Supported trigger: A configured native host closes or redirects stderr before announcing readiness and then remains alive. A trusted process can change its logging destination; no detached process or hostile input is required.
- Observed evidence: A real browser-enabled CLI fixture execs Python, records that it has started, closes descriptor 2, and sleeps. At the repaired revision, Snap was still running **25.77 seconds after host startup**, beyond the existing 20-second deadline. The probe stopped Snap at its own 26-second bound; Snap then returned 143. No readiness-timeout diagnostic was emitted.
- Cause: `timeout(..., receiving)` completes as soon as the log task drops the sender on stderr EOF. The selected branch then awaits `service.wait(self)` outside that timeout. The host's eventual exit or a CLI interrupt can end this wait, but the startup deadline no longer can. At the prior revision, that same wait was inside the timed readiness future.
- Bounded proposed remedy: Keep the overall startup deadline active while waiting after readiness-channel closure, while retaining the new direct-child-exit race and group-before-reader cleanup. A real-CLI case with early stderr closure should prove bounded startup failure and process cleanup. This is a proposed remedy for human consideration, not authorization for another autonomous repair batch.

There are no partial prior-finding resolutions. No other repair-induced standards findings were confirmed. SPEC-1, SPEC-2, and SPEC-4 remain assigned to the Spec validator; this report does not substitute for their verdict.

## Independent checks

- Inspected the five-file repair diff, the review ledger, the prior Standards report, the current process implementation, and the prior revision's readiness implementation.
- `git diff --check 9389e4367d13d5d9fa09b41867a4af4b0629d4a3...da14da4e0fbcbda99da3d8b312913a8a7c17e714` passed.
- Ran `mise exec -- python3 /tmp/opencode/hmr-standards-r2-probe.py` once. Its two bounded cases establish STD-1 resolution and reproduce STD-2 through the real CLI.
- Probe sources: `/tmp/opencode/hmr-standards-r2-probe.py`.
- Complete results: `/tmp/opencode/hmr-standards-r2-probe.json`.
- Fixtures lived in ignored `.tmp`, requested OS-assigned development ports, owned their process sessions, and were cleaned afterward. STD-1 descendant state was checked before fixture fallback cleanup.

## Supplied evidence and limits

The coordinator reports a passing post-repair `mise exec -- ./bin/check`, including 9 dev, 7 build, 4 check, 5 architecture CLI tests, three Chromium scenarios, all previous SDK/protocol/journey/structural/doc/dependency/lifecycle gates, and no skips. The ledger also records a passing whitespace check. These full-suite results were supplied, not independently repeated.

The extended early-exit fixture covers STD-1 but does not cover a still-running host whose stderr closes before readiness. STD-2's head behavior was reproduced; its prior bounded behavior was established from the pinned implementation rather than a separate baseline build. The probe's sleep is a deliberately stalled host; the consumer-visible failure is Snap outliving its established startup deadline.

No product edits, commits, publication, or delegation occurred. Final validation is complete. Further action requires human intervention under the recorded two-round limit.

## Complete round 2 Spec report

# Slice 05 Spec fix validation, final round

Axis result: **BLOCKED**. All four original Spec findings are resolved. The readiness repair introduces one confirmed startup-timeout regression, SPEC-5.

## Revisions and scope

- Prior reviewed revision: `9389e4367d13d5d9fa09b41867a4af4b0629d4a3`
- Repaired revision: `da14da4e0fbcbda99da3d8b312913a8a7c17e714`
- Repair commit: `da14da4 Preserve native proxy behavior and early service failure status`
- Original milestone base: `629c46944d1dd79ded7c272b2574798202e77ba5`

Validated the repair diff, the accepted ledger in `docs/reviews/browser-hmr.md`, the four assigned findings, and directly affected process/proxy behavior. No unaffected whole-diff audit. Linux, trusted projects, Healthy HTTP-only scope. No new authentication, application WebSocket, or Rust-watch requirements.

HEAD matched the repaired revision. Tracked working tree was clean. No product edits, commits, publication, or delegation.

## Original findings

| Finding | Status | Validation |
| --- | --- | --- |
| SPEC-1, Vite intercepts OPTIONS | Resolved | `cors: false` preserves native handling. Through the real CLI, public and private `OPTIONS /health/up` both returned 405, the same error body and native cache/Allow headers. This also held with Origin and Access-Control-Request-Method headers. The previous Vite CORS approval headers were absent. |
| SPEC-2, custom public Host rejected | Resolved | `allowedHosts: true` implements the ledger's accepted native hostname policy for trusted local development. The real public and private health endpoints both returned 200 with `Host: healthy.test`, its matching Origin, and a probe Cookie. The added browser-launch regression also checks `/@vite/client` with that Host; its passing execution is supplied evidence. |
| SPEC-3 / STD-1, early failure loses status when descendants retain stderr | Resolved | Readiness now races direct `child.wait()`. The exit branch calls `finish`, which drops the process-group guard before joining logs, then preserves the exit status. The original real-CLI descendant probe now returned 37 in 0.28 seconds, versus status 1 after 20.26 seconds in round 1. The extracted `successful` helper preserves the previous success/code/signal mapping. See SPEC-5 for a separate regression in this repair. |
| SPEC-4, configured port 80 becomes ephemeral | Resolved | `Number(listen.port || "80")` retains HTTP's normalized default port. Independent Bun evaluation produced 80 for IPv4/IPv6 port-80 addresses, 0 for explicit IPv4/IPv6 port zero, and 3846 for the ordinary development port. No port-80 listener was bound or replaced. |

No original finding remains partial or unresolved.

## Repair-induced finding

### SPEC-5: Closed stderr removes the startup readiness deadline

Disposition: **BLOCKER**. Severity: medium. Status: unresolved.

Location: `tools/cli/src/process.rs:75-80` at `da14da4e0fbcbda99da3d8b312913a8a7c17e714`.

Existing behavior and requirement: resident service startup must either observe readiness or fail and clean the owned service. The prior reviewed implementation bounded the entire readiness operation, including its fallback wait after log-channel closure, with a 20-second timeout. The repair moves the timeout to only the readiness receiver. This regresses an existing observable startup-failure bound within the changed code.

Trigger: the configured native host redirects or closes stderr, remains alive, and never emits readiness. Closing stderr completes the log reader and drops the readiness sender. The `Err(_)` arm then awaits `service.wait(self)` outside any timeout. A host that remains alive can now hold startup indefinitely.

Observed evidence: `/tmp/opencode/hmr-spec-closed-stderr.py` launches the real `snap dev` against an owned independent web-enabled fixture. Its native executable uses safe `CommandExt::exec` to run `sleep 60` with stderr redirected to `/dev/null`. Compilation finished in about 0.1 seconds. Snap still had not exited or reported its readiness timeout when the probe's **25-second deadline** fired. The probe then terminated its owned CLI; cleanup completed and reported child status 143. The fixture sources were removed.

Prior-revision comparison: at `9389e43`, `timeout(Duration::from_secs(20), readiness)` surrounds the future containing both `receiving.await` and `service.wait(self).await`. Thus the same closed-stderr condition remains bounded. At `da14da4`, only `receiving` is timed, and the fallback wait occurs in the selected branch after that timeout has completed. This is introduced by the repair, not a newly audited pre-existing concern. The prior executable was not rebuilt for this probe; the baseline comparison is a direct control-flow comparison.

Bounded remedy: retain a deadline across the entire pre-readiness phase while also racing direct child exit. An early closed readiness channel must not enter an unbounded wait. Preserve the repaired exit-status and process-group cleanup behavior. Verify the closed-stderr/live-child case at the CLI boundary.

This is the final authorized review round. Further repair or validation requires human adjudication under the recorded two-round limit. No autonomous third round was started.

## Checks and limits

Independently performed:

- Resolved both immutable revisions and HEAD; inspected the repair commit/diff and ledger.
- `mise exec -- bun /tmp/opencode/hmr-spec-probe.ts`: real CLI and owned Healthy copy on ephemeral ports; normal health and Build responses remained successful; OPTIONS and custom-Host comparisons confirmed SPEC-1 and SPEC-2 repairs.
- `mise exec -- python3 /tmp/opencode/hmr-spec-early-exit.py`: inherited-stderr descendant case confirmed SPEC-3 repair.
- Bun evaluation of repaired port handling confirmed SPEC-4, including explicit port zero.
- `mise exec -- python3 /tmp/opencode/hmr-spec-closed-stderr.py`: reproduced SPEC-5; owned timeout cleanup completed.
- `git diff --check 9389e4367d13d5d9fa09b41867a4af4b0629d4a3...da14da4e0fbcbda99da3d8b312913a8a7c17e714` passed. Tracked-tree check was clean.

Supplied evidence, not independently repeated: full `mise exec -- ./bin/check` passed after all repairs, including 9 dev, 7 build, 4 check, 5 architecture CLI cases, three Chromium cases, and all previous SDK/protocol/journey, structural, documentation, dependency-tool and lifecycle gates, with no skips. Inspected the extended early-failure fixture and public/private/custom-Host regression assertions.

No full-suite repeat, fresh Chromium run, live port-80 bind, or broader security audit. The passing supplied suite does not cover the closed-stderr/live-child readiness case. No independent follow-ups or optional advisories are raised.
