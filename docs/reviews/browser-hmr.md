# Browser HMR review

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
earlier milestone reviews are closed. Status: awaiting implementation SHA and reports.
