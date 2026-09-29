---
name: Factorio
description: Existing light green workspace shell with system sans typography.
colors:
  primary: "#286044"
  primary-hover: "#1f4d36"
  primary-active: "#19442e"
  canvas: "#f6f7f4"
  surface: "#fff"
  text: "#23332d"
  muted: "#657169"
  separator: "#dce2dc"
  field-border: "#73877a"
  button-text: "#31483a"
  button-border: "#c2cdc4"
  button-hover: "#edf2ed"
  list-hover: "#edf0eb"
  secondary-text: "#536159"
  selected: "#e2eee5"
  selected-meta: "#35674a"
  conversation: "#edf3ed"
  done-background: "#e2ecf3"
  done-text: "#365d78"
  danger: "#9c3735"
  alert-background: "#fff0ee"
  alert-border: "#efcfca"
  scroll-thumb: "#bbc8be"
typography:
  body:
    fontFamily: "system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.5
  detail-title:
    fontFamily: "system-ui, sans-serif"
    fontSize: "30px"
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: "-.04em"
  detail-title-mobile:
    fontFamily: "system-ui, sans-serif"
    fontSize: "26px"
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: "-.04em"
  section-title:
    fontFamily: "system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 700
    lineHeight: 1.5
    letterSpacing: "0"
  button:
    fontFamily: "system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 550
    lineHeight: 1.4
  navigation:
    fontFamily: "system-ui, sans-serif"
    fontSize: "14px"
    fontWeight: 550
    lineHeight: 1.5
  metadata:
    fontFamily: "system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.5
  badge:
    fontFamily: "system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 600
    lineHeight: 1.5
  module-list:
    fontFamily: "ui-monospace, monospace"
    fontSize: "13px"
    lineHeight: 1.8
rounded:
  flat: "0"
  field: "6px"
  button: "7px"
  panel: "8px"
  thread-link: "10px"
  card: "12px"
  drawer: "16px 16px 0 0"
  badge: "20px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "12px"
  lg: "16px"
  xl: "20px"
  xxl: "24px"
  section: "32px"
  detail: "40px"
components:
  button:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.button-text}"
    typography: "{typography.button}"
    rounded: "{rounded.button}"
    padding: "9px 14px"
  button-hover:
    backgroundColor: "{colors.button-hover}"
  button-active:
    backgroundColor: "{colors.selected}"
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.surface}"
    typography: "{typography.button}"
    rounded: "{rounded.button}"
    padding: "9px 14px"
  button-primary-hover:
    backgroundColor: "{colors.primary-hover}"
  button-primary-active:
    backgroundColor: "{colors.primary-active}"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.button-text}"
    typography: "{typography.button}"
    rounded: "{rounded.button}"
    padding: "9px 14px"
  button-danger-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.danger}"
    typography: "{typography.button}"
    rounded: "{rounded.button}"
    padding: "9px 14px"
  field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.field}"
    padding: "10px 12px"
  navigation-desktop:
    textColor: "{colors.secondary-text}"
    typography: "{typography.navigation}"
    rounded: "{rounded.field}"
    padding: "10px 18px"
  navigation-desktop-current:
    backgroundColor: "{colors.selected}"
    textColor: "{colors.primary}"
  navigation-mobile-current:
    backgroundColor: "{colors.conversation}"
    textColor: "{colors.primary}"
    rounded: "{rounded.button}"
  work-list-row:
    textColor: "{colors.text}"
    rounded: "{rounded.field}"
    padding: "14px 10px"
  work-list-row-current:
    backgroundColor: "{colors.selected}"
    textColor: "{colors.primary}"
  badge:
    backgroundColor: "{colors.list-hover}"
    textColor: "{colors.secondary-text}"
    typography: "{typography.badge}"
    rounded: "{rounded.badge}"
    padding: "3px 9px"
  badge-ready:
    backgroundColor: "{colors.selected}"
    textColor: "{colors.primary}"
  badge-done:
    backgroundColor: "{colors.done-background}"
    textColor: "{colors.done-text}"
  card:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.card}"
    padding: "24px"
  work-detail:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.flat}"
    padding: "32px 40px"
  work-detail-mobile:
    padding: "24px 20px"
---

# Design system: Factorio

## Overview

Preserve the approved incumbent light green identity, system sans typography and white detail canvas. The full-width shell separates navigation, record selection and the selected document. Green marks current selections and primary actions; borders and background changes separate regions.

This document records the implementation in `web/style.css`, `web/shell.tsx` and `web/pages/workspace.tsx`. `CONTRACT.md` owns product behavior. No new brand metaphor or product positioning is implied.

Key characteristics:
- Full-width desktop shell with a fixed-width record sidebar and fluid detail.
- Mobile bottom navigation and a modal bottom drawer for the same record list.
- Flat document areas, restrained rounding and visible keyboard focus.
- System fonts and inline SVG icons. No raster assets ship; review screenshots are evidence, not assets.

## Colors

The frontmatter is normative. The palette uses a green accent with pale green-gray chrome and white documents.

### Primary

Use `primary` for primary buttons, links, current navigation, selected record text, ready status, input carets and keyboard focus. Primary buttons use the darker hover and active tokens. Selected desktop navigation and record rows use `selected`; mobile current navigation and conversation summaries use `conversation`.

### Neutral

Use `canvas` for header/sidebar chrome, `surface` for documents and controls, `text` for body copy, and `muted` for secondary copy. `separator` divides structural regions. Fields use the stronger `field-border`, not the separator token. Standard buttons retain their separate text, border and hover tokens. Selected record metadata uses `selected-meta`.

Status badges use their existing ready and done pairs. Danger text and alerts use the danger/alert tokens. Status always remains readable as text, not color alone.

## Typography

Use system sans throughout, with system monospace for identifiers, commands and evidence. Do not introduce a display font or downloadable font asset.

The body is the base role. Detail titles use `detail-title`, then `detail-title-mobile` at the shell breakpoint. Shell section headings stay at the compact `section-title` size. Desktop navigation and buttons use distinct line heights from their respective roles. List metadata and mobile labels are compact; badges use the badge role. Record titles in lists remain bold rather than growing in size.

The desktop brand is 20px, weight 700, with -.025em tracking; mobile reduces it to 17px. Session identifiers in the detail header are 26px. Thread titles remain 16px with 1.4 line height and no tracking. General headings outside the detail retain the stylesheet's existing hierarchy.

## Layout

The shell fills the viewport without a centered page-width cap. Its fixed frame uses the visual viewport height and top offset, with `100dvh` as the initial height. The desktop header spans the full width, has a 70px minimum height and 12px 24px padding. Tickets and Sessions have a 280px sidebar; Intakes does not inherit that sidebar. List and detail scroll independently inside flex regions with zero minimum height.

Desktop detail uses the frontmatter padding. Prose is capped at 72ch, properties and additional ticket actions at 880px, and the ticket editor at 760px. Properties use two equal columns with a 24px gap. Setup and intake content remain centered within 820px rather than constraining the whole shell.

At widths of 800px or less:
- Hide desktop navigation and the sidebar. Stack brand/workspace labels and use the compact 64px-minimum header.
- Show bottom icon-and-label navigation with minimum height `calc(68px + env(safe-area-inset-bottom))`. Navigation links and the list trigger have a 54px minimum height.
- Keep the selected detail visible. The list strip opens a bottom drawer above navigation, using the same list renderer as the sidebar.
- Drawer width is `min(100%, 560px)` and height is `min(72dvh, 660px)`, capped by the shell height minus navigation and 16px.
- Use mobile detail padding, one property column and full-width stacked detail actions.
- Respect safe-area insets in the header and bottom navigation. When an editable input or textarea has focus and the visual viewport shrinks by more than 120px, hide bottom navigation and the list trigger.

The older 700px breakpoint still reduces general page/card padding and collapses general two-column forms. It does not replace the 800px shell breakpoint. Thread layout keeps its header and composer outside the scrolling conversation; composer textareas have a 64px minimum height and a 25dvh maximum height.

## Elevation & Depth

The shell, sidebar and detail are flat. Background tones and single-pixel separators establish structure; document cards do not acquire shadows. Shadows are limited to the account popover, list drawer and latest-message control. Their exact values live in the sidecar.

The drawer has a translucent backdrop and enters with a 24px vertical translation over 180ms. Reduced-motion preference removes that animation. There is no general transition system in the current stylesheet.

## Shapes

Fields and desktop navigation share modest rounding. Buttons, small panels, thread links and cards use their separate frontmatter radii. Badges are rounded capsules. The selected document and its editor are borderless and square, not nested cards. The bottom drawer rounds only its top corners.

## Components

### Buttons

Standard buttons are white with a fine border; primary buttons use green with white text. Quiet buttons remove the resting background and border, and danger changes text color. Buttons have a 44px minimum height, hover/active background states, and weight 550. Disabled buttons use opacity .5 and a wait cursor. Detail actions stack on mobile; primary actions stay with the document.

### Inputs / Fields

Fields have a white background, the functional field border, base-size text and a 44px minimum height. Placeholder text uses `muted`. The global focus-visible treatment is a 3px solid primary outline with a 3px offset. Keep this outline on all keyboard-focusable controls and links. Generic textareas resize vertically; the conversation composer does not.

### Navigation

Desktop navigation is text-only; its SVG icons are hidden. Mobile navigation shows the same section labels below 22px inline stroke icons. Current links use `aria-current` and the appropriate desktop/mobile selected colors. Navigation hover uses `list-hover`.

Account actions remain in the header's native disclosure popover. Mobile keeps a 44px icon-only account summary with an accessible label.

### Record lists and drawer

Rows contain a bold title, compact metadata and a text status. Metadata truncates to two lines. Selected rows replace the bottom divider with a green background and use the selected metadata color. Open/All filtering stays above the scrollable list and its action stays below it.

The mobile drawer is a native modal dialog with a handle and close button. It contains focus, accepts Escape, closes after selection and returns focus to the trigger. Returning to desktop at 801px closes it. Do not substitute a second, differently styled list.

### Badges and containers

Badges use the existing neutral, ready and done variants. General forms/cards use bordered white containers; shell documents override that treatment with a flat white canvas. Conversation user messages and draft summaries use `conversation`. Alerts use the existing danger text and tinted bordered background.

## Do's and Don'ts

- Do preserve the incumbent light green palette and system fonts.
- Do use the field-border token for fields and the primary outline for keyboard focus.
- Do keep desktop list and detail scrolling independent, and preserve the detail-first mobile layout.
- Do reuse the sidebar list inside the native modal drawer.
- Don't apply the old centered main-page width cap to the workspace shell.
- Don't turn the selected document into a rounded, shadowed card.
- Don't treat review screenshots as shipping imagery or add raster assets to this system.
