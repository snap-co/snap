---
version: 1
slug: "web-pages-workspace-tsx"
primary_target: "web/pages/workspace.tsx"
related_targets: ["web/style.css","web/routes.tsx","web/pages/intake.tsx"]
---

# Factorio workspace shell

Mode: Operate. Preserve ticket/session commands, human approval, onboarding, Authy, intake conversations and existing light green visual identity. The approved mockup is `/home/cc444/code/snapco/.impeccable/mocks/factorio-shell-proposal.svg`, a layout reference with illustrative data, not a pixel-measured image comp.

## Direction contract

THESIS: Select work from a compact list and act on one detail, replacing the all-records workspace page.

OWN-WORLD: Existing system sans, white detail canvas, #f6f7f4 chrome, #23332d text, #286044 actions, #dce2dc separators. Green selected rows; restrained rounding.

STORY: Navigate Intakes, Tickets or Sessions, select work, inspect dependencies or review evidence, then use existing guarded actions.

FIRST VIEWPORT: Desktop full-viewport top navigation, 280px left list, fluid detail. Mobile detail-first with bottom navigation and a list strip opening a bottom drawer. Account/token actions leave the main navigation. Primary actions remain within the detail.

FORM: User-pinned list/detail topology, explicitly approved. No concept seed applies to this precisely specified extension. Code-led.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

Constraints: Open/all filter, newest-created first with ID tie-break and undated legacy records last. Preserve direct links and selections across section navigation. Drawer uses native dialog focus containment, Escape, close button and focus return. Mobile safe areas and visual keyboard viewport must not hide the composer. No raster assets ship.
