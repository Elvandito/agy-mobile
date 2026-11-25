# DESIGN.md: AGY Mobile Visual and Interaction Direction

> Reading this as: native Android developer CLI and agent workbench for mobile engineers on Android 5.0+ and low-RAM hardware, in a utilitarian hardware terminal style (matte charcoal + warm amber accent + crisp light-paper toggle), dial ENERGY 1 / RHYTHM 1 / MOTION 1.

Dial: ENERGY 1 / RHYTHM 1 / MOTION 1

## 1. Identity and Character

AGY Mobile treats a phone or compact tablet as a dedicated field terminal rather than a marketing showcase. Every pixel serves fast command entry, legible streaming output, explicit runtime honesty (`Native CLI` vs `Compatible API`), and safe workspace inspection on constrained screens down to Android 5.0 (API 21) and 1 GB RAM.

- **Identity motif**: crisp `1dp` structural hairlines paired with a solid `2dp` warm amber status/focus rule (`#F2A93B` in dark mode, `#8A4D00` in light mode) and tabular monospace metadata headers (`[RUNTIME: API-COMPAT]`, `[MODE: PLAN]`).
- **Focal point per screen**:
  - **Terminal screen**: the live streaming terminal buffer and docked command prompt bar.
  - **Workspace screen**: the active project tree and streaming file/diff inspector.
  - **Config screen**: the runtime & authentication status card with explicit capability breakdown.

## 2. Color System (2 Core Neutrals + 1 Deliberate Accent)

All pairings were verified mathematically with `contrast-check.py` against WCAG 2.2 AA (`>= 4.5:1` normal text, `>= 3.0:1` non-text UI boundaries and focus indicators).

### Dark Theme (Default: Matte Charcoal Hardware)
- **Canvas background**: `#121417`
- **Elevated surface / bar**: `#1B1F24`
- **Primary foreground text**: `#EDEAE2` (`15.35:1` on canvas, `13.78:1` on surface)
- **Secondary foreground text**: `#B2B9C2` (`9.32:1` on canvas, `8.37:1` on surface)
- **Deliberate accent (Amber)**: `#F2A93B` (`9.24:1` on canvas, `8.29:1` on surface; `#121417` button ink on `#F2A93B` = `9.24:1`)
- **Component & focus boundary**: `#646E7C` (`3.20:1` on surface, `#F2A93B` `2dp` ring when keyboard-focused)
- **Semantic error / approval warning**: `#FF8A80` (`7.25:1` on surface), always paired with explicit text labels (`[ERROR]`, `[APPROVAL REQUIRED]`)

### Light Theme (Toggle: Warm Engineering Paper)
- **Canvas background**: `#F5F2EB`
- **Elevated surface / bar**: `#EAE5DA`
- **Primary ink**: `#16191D` (`15.77:1` on canvas, `14.04:1` on surface)
- **Secondary ink**: `#454D57` (`7.66:1` on canvas, `6.82:1` on surface)
- **Deliberate accent (Amber-Ochre)**: `#8A4D00` (`5.98:1` on canvas, `5.32:1` on surface; `#FFFFFF` button ink on `#8A4D00` = `6.68:1`)
- **Component & focus boundary**: `#68707B` (`3.99:1` on surface, `#8A4D00` `2dp` ring when keyboard-focused)
- **Semantic error / approval warning**: `#A61B1B` (`5.98:1` on surface), always paired with explicit text labels

## 3. Typography, Geometry, Spacing, and Motion Rationale (R-31)

- **Why this color palette**: Matte charcoal (`#121417`) and warm phosphor amber (`#F2A93B`) reduce glare during extended terminal sessions while warm paper (`#F5F2EB`) preserves outdoor daylight legibility without resorting to generic blue-purple gradients.
- **Why this layout**: A compact 3-destination bar (`Terminal`, `Workspace`, `Config`) keeps primary workflows one thumb tap apart on narrow phones and expands into a side-by-side split pane on landscape/tablet widths (`sw600dp` / `w720dp`) without wasting vertical space above the on-screen keyboard.
- **Why this typography**: System `monospace` (`Typeface.MONOSPACE`, scaled in `sp` so 200% OS font zoom works cleanly) is restricted to terminal cells, code, paths, and status telemetry where fixed character pitch aids scanning, while system `sans-serif` (`Typeface.SANS_SERIF`) handles UI labels, dialogs, and prose explanations.
- **Why this spacing and border radius**: A compact `4dp / 8dp / 12dp / 16dp` spacing scale maximizes visible terminal rows on small screens while holding interactive controls at `>= 48dp` height with `8dp` separation; `4dp` surface corners and `6dp` primary action corners distinguish interactive buttons from static containers without pill-everything uniformity.
- **Why flat bordered surfaces instead of shadow-heavy cards**: `1dp` high-contrast borders (`>= 3:1`) communicate grouping at zero GPU overdraw cost on 2014 ARMv7 GPUs, eliminating floating card shadows, blur, and glow.
- **Why the typographic `AGY` launcher mark**: Confirmed with the product owner (R-23), a geometric `AGY` monogram over `#121417` with a `2dp` `#F2A93B` baseline bar identifies the tool cleanly at `48px` launcher density without stock robot or sparkle tropes.
- **Why MOTION 1 (instant state transitions)**: Low-RAM Android 5.0 devices suffer frame drops from decorative animations; state transitions are immediate and respect system animator scale settings.
