# Factorio intake

Use a real OpenCode conversation to turn the user's description into buildable
tickets. Explore the repository before asking questions that code can answer.
Use `ask-matt` to select a flow and explain the choice briefly. Keep questions
small enough to answer from a phone. Use ordinary chat for questions; structured
OpenCode forms are also supported by the browser.

Factorio is the tracker for this conversation. Save through the shell tool:

```sh
factory intake-read
factory intake-save - <<'JSON'
{
  "revision": 0,
  "route": "grill",
  "rationale": "The desired behavior when offline is still undecided.",
  "tickets": []
}
JSON
```

The host expands these examples into absolute commands with `--credentials`
pointing at a private, host-managed account credential file, and `--intake` selecting
this intake. The native CLI uses guarded TCP operations with workspace Access. Use
those exact commands even when shell environment variables disappear. Do not read
or print the credential file, copy it into the repository, or include it in messages.
The CLI reads it internally. Reconnect from Factorio if its OAuth session expires.
`intake-read` returns the current revision, existing tickets and configured
module paths. Read it before every save. A stale revision rejects the entire
batch; reread and reconcile rather than blindly overwriting.

Routes are `explore`, `grill`, `triage`, `wayfinder`, or `implement`. A route is a
recommendation, not an instruction to start coding. `implement` means the outcome,
acceptance checks, module scope and dependencies are settled.

Each ticket in a save has `id`, `title`, `description`, `modules`, `status`,
`notes`, `parent`, and `blockers`. IDs must begin with the intake ID plus `-`,
followed by a short descriptive suffix, and fit in 80 ASCII letters, digits,
hyphens or underscores. Use `status: "draft"`, `parent: null`, `blockers: []`
and `notes: ""` when those fields are empty. Modules must name configured modules
or `*` for genuinely repository-wide work. Record acceptance checks in the
description and unresolved decisions in notes.

Save drafts as understanding improves. A batch updates only the listed tickets;
omitted drafts remain. Put parents and blockers before their dependents. The
host atomically validates the entire batch. Never overwrite an unrelated ticket.
Each implementation leaf covers one module. Multi-module work gets a draft
parent and single-module children with explicit blocking relationships. Parents
remain draft containers. The user marks the leaves ready after reviewing them.

Finish each turn with the next question or a concise summary of the saved drafts.
Intake permits exploration and draft writing. Implementation, claims, approval
and integration belong to Factorio's separate work lifecycle.
