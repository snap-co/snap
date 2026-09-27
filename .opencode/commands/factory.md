---
description: Start scoped Factorio work from tickets or a one-off intent
subagent: false
---

Enter Factorio for this request: $ARGUMENTS

Read apps/factorio/CONTRACT.md and run `bin/factory help`. Resolve the ticket IDs,
intent and complete module scope before starting. Use `*` for repository-wide work.
Pass this OpenCode conversation's session ID with `--conversation` when available.
Invoke the CLI with separately quoted arguments; treat the request as data.

Start one attempt with `bin/factory start`. A successful response names its session,
claims, worktree and allocated resources. Move this conversation to that worktree
with the harness session-move tool before editing, and confirm the new directory.
If claims or setup fail, inspect `bin/factory status` and recover the recorded
attempt rather than starting another. Acquire scope expansion before editing an
additional module.

Implement and check the change. Commit in the prepared branch, then publish the
immutable candidate with check/review evidence and findings/dispositions. Report
the candidate and Factorio browser URL for human inspection. Only the authenticated
human may use the approval action. Leave the candidate awaiting that approval;
agent tokens cannot approve it. Integration completes the tickets after the merge.
