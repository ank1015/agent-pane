# Cursor Dashboard Design System

Reverse-engineered visual specification for recreating the signed-in Cursor web dashboard's dark theme in future applications.

## Audit scope and confidence

This document is based on direct inspection of the live Cursor web application in Google Chrome on August 20, 2026, at a 1,512 × 806 CSS-pixel viewport and 2× device pixel ratio. It covers:

- Agent home and composer
- Automations and automation templates
- Codebase and Codebase Settings
- Dashboard overview
- Settings
- Cloud Agents
- Plugins
- Integrations
- API Keys
- Shared Canvases
- Members
- Usage
- Spending
- Billing & Invoices

All values marked **measured** came from computed browser styles or element bounds. Responsive recommendations are marked **inferred** because the audit was performed at one desktop viewport. Personal account values and identifiers are intentionally omitted.

Companion implementation file: [`cursor-dashboard-theme.css`](./cursor-dashboard-theme.css).

## 1. Visual thesis

Cursor's dashboard is a dense, low-chroma workbench. Its character comes from restraint rather than ornament:

- Nearly black chrome (`#141414`) with a slightly lifted sidebar/card surface (`#181818`).
- Warm off-white (`#f0f0f0`) reused at controlled opacity for text, borders, selection, hover, and pressed states.
- Small, regular-weight typography. Most UI text is 13 px, not 14–16 px.
- Almost no conventional drop shadow on ordinary cards. Separation comes from 4–8% white hairlines and one-step background elevation.
- Tight controls: 28–30 px high, 6 px radius, 8–10 px horizontal padding.
- Cards are 12 px radius. Pills are fully rounded. The radius hierarchy is consistent.
- Color is semantic and sparse. Blue is the principal accent; green, amber, red, purple, cyan, orange, and magenta are status/data colors.
- Motion is fast and quiet: 150 ms is the dominant transition duration.

If a recreation feels too glossy, too blue, too rounded, too bold, or too spacious, it has moved away from the Cursor theme.

## 2. Foundations

### 2.1 Core surfaces

| Token | Exact value | Use |
|---|---:|---|
| `chrome` | `#141414` | Page background and deepest application plane |
| `surface` | `#181818` | Sidebar, cards, inputs, elevated workbench areas |
| `surface-card` | `color-mix(in oklab, #f0f0f0 6%, transparent)` | Translucent card/active-row overlay |
| `surface-input` | `#181818` | Search, text inputs, composer controls |
| `text-primary` | `#f0f0f0` | Titles, key labels, selected states |
| `text-secondary` | `color-mix(in oklab, #f0f0f0 74%, transparent)` | Supporting text and icons |
| `text-tertiary` | `color-mix(in oklab, #f0f0f0 60%, transparent)` | Captions, metadata, unselected tabs |
| `text-quaternary` | `color-mix(in oklab, #f0f0f0 36%, transparent)` | Placeholders and disabled detail |
| `text-inverse` | `#181818` | Text on white primary buttons |

The system's neutral ladder is an off-white overlay, not a sequence of unrelated grays:

```css
--neutral-04: color-mix(in oklab, #f0f0f0 4%, transparent);
--neutral-06: color-mix(in oklab, #f0f0f0 6%, transparent);
--neutral-08: color-mix(in oklab, #f0f0f0 8%, transparent);
--neutral-12: color-mix(in oklab, #f0f0f0 12%, transparent);
--neutral-14: color-mix(in oklab, #f0f0f0 14%, transparent);
--neutral-16: color-mix(in oklab, #f0f0f0 16%, transparent);
--neutral-20: color-mix(in oklab, #f0f0f0 20%, transparent);
--neutral-22: color-mix(in oklab, #f0f0f0 22%, transparent);
```

Use these overlays over `#141414` or `#181818`. Do not replace them with opaque gray swatches if visual fidelity matters; the overlay model is why elevation stays subtle and consistent.

### 2.2 Semantic colors

| Role | Exact value | Typical use |
|---|---:|---|
| Accent | `#599ce7` | Focus, progress, selected/high-emphasis interactive state |
| Link / blue | `#7bafe9` | Links and secondary blue data |
| Success | `#3fa266` | Success state and active confirmation |
| Added | `#70b489` | Git additions and positive counts |
| Warning | `#f1b467` | Warning, modified files, caution |
| Danger / removed | `#fc6b83` | Errors, destructive state, Git removals |
| Cyan | `#81a1c1` | Data series and informational state |
| Purple | `#9386f2` | Model/data differentiation |
| Magenta | `#b48ead` | Data series and syntax/category state |
| Orange | `#d08770` | Data series and secondary warning |
| Untracked | `#88c0d0` | Git untracked state |

Tint formula: use 8% for a very quiet background, 12% for a standard badge background, 24% for a selected/stronger tint, 56–70% for borders/icons, and 78% for secondary colored text.

```css
.status-success {
  color: #70b489;
  background: color-mix(in oklab, #70b489 12%, transparent);
  border-color: color-mix(in oklab, #70b489 32%, transparent);
}
```

### 2.3 Borders

| Level | Exact value | Use |
|---|---|---|
| Faint | `color-mix(in oklab, #f0f0f0 4%, transparent)` | Card outline, row separators |
| Subtle | `color-mix(in oklab, #f0f0f0 8%, transparent)` | Panel border, selected outline |
| Control | `color-mix(in oklab, #f0f0f0 12%, transparent)` | Inputs and secondary buttons |
| Strong | `color-mix(in oklab, #f0f0f0 20%, transparent)` | Focused/active neutral border |
| Neutral | `color-mix(in oklab, #f0f0f0 80%, transparent)` | Rare high-contrast hairline |

Prefer a 1 px inset outline or border. Ordinary cards do not use bright strokes.

## 3. Typography

### 3.1 Font families

Measured body stack:

```css
font-family: system-ui, -apple-system, "Segoe UI", Roboto,
  "Helvetica Neue", Arial, sans-serif,
  "Apple Color Emoji", "Segoe UI Emoji";
```

Measured monospaced stack:

```css
font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas,
  "Liberation Mono", "Courier New", monospace;
```

Cursor uses a custom `cursor-icons` icon font. For independent products, use 14–16 px outline SVG icons with approximately 1.5 px strokes (Lucide is a close substitute) rather than copying the proprietary font.

### 3.2 Type scale

| Style | Size / line-height | Weight | Tracking | Use |
|---|---|---:|---:|---|
| Page title | `20px / 28px` | 400 | `-0.26px` | Primary page heading |
| Large UI | `14px / 22px` | 400 | `-0.15px` | Emphasized labels; use sparingly |
| Base UI | `13px / 18px` | 400 | `-0.08px` | Navigation, buttons, body, tables |
| Caption | `12px / 16px` | 400 | `0` | Descriptions, helper text |
| Micro | `11px / 14px` | 400 | `0.07px` | Badges, timestamps, compact metadata |
| Composer/body-large | `16px / 24px` | 400 | near `0` | Agent prompt and occasional content |
| Emphasis | same size as context | 600 | inherit | Rare labels or critical numerals |

The dashboard intentionally uses weight 400 even for many headings. Do not default every heading or selected item to 600. Hierarchy is produced with size, spacing, color, and placement.

## 4. Spacing and density

Cursor exposes a 1 px spacing ladder, but actual layouts cluster around these anchors:

| Token | Value | Common use |
|---|---:|---|
| `space-1` | 1 px | Adjacent nav-row rhythm, hairline offsets |
| `space-4` | 4 px | Tight icon/text or stacked metadata |
| `space-6` | 6 px | Button icon gap |
| `space-8` | 8 px | Nav icon gap, compact padding |
| `space-10` | 10 px | Nav/input horizontal padding |
| `space-12` | 12 px | Compact row gap and table padding |
| `space-16` | 16 px | Card padding, row padding, content gap |
| `space-24` | 24 px | Standard card padding and section separation |
| `space-32` | 32 px | Empty-state padding, large subsection gap |
| `space-40` | 40 px | Major grouping |
| `space-64` | 64 px | Desktop page top offset |
| `space-80` | 80 px | Desktop main-content gutter/bottom breathing room |

Dominant measured CSS gaps across the Agent surface were 4, 6, 8, 12, 16, and 24 px. Use those first.

## 5. Radius system

| Token | Value | Use |
|---|---:|---|
| `radius-xs` | 2 px | Tiny code/status treatments |
| `radius-sm` | 4 px | Segmented tabs and micro controls |
| `radius-base` | 6 px | Buttons, inputs, nav rows, icon buttons |
| `radius-lg` | 8 px | Empty states and compact panels |
| `radius-xl` | 12 px | Primary cards and tables |
| `radius-2xl` | 14 px | Elevated/special panels |
| `radius-3xl` | 16 px | Composer and large feature surfaces |
| `radius-4xl` | 18 px | Rare oversized shell surfaces |
| `radius-full` | 9999 px | Pills, avatars, status dots, switches |

Do not use 10 px. Cursor's most visible pairing is 6 px controls inside 12 px cards.

## 6. Effects and motion

Ordinary card:

```css
background: #181818;
border: 1px solid color-mix(in oklab, #f0f0f0 4%, transparent);
border-radius: 12px;
box-shadow: 0 0 0 1px color-mix(in oklab, #f0f0f0 4%, transparent);
```

Popover/elevated workbench:

```css
box-shadow:
  0 0 0 1px color-mix(in oklab, #f0f0f0 8%, transparent),
  0 0 4px #0000001f,
  0 8px 24px -2px #0000001f;
```

The dominant motion rule is:

```css
transition-duration: 150ms;
transition-timing-function: cubic-bezier(0.4, 0, 0.2, 1);
```

Transition color, background-color, border-color, fill, and stroke. Use 400 ms only for soft fades; do not animate layout extensively. Honor `prefers-reduced-motion`.

## 7. Application shell

### 7.1 Desktop geometry (measured)

| Element | Measurement |
|---|---:|
| Sidebar | 280 px wide, full viewport height |
| Sidebar surface | `#181818` |
| Main background | `#141414` |
| Main content start | x ≈ 359–376 px at a 1,512 px viewport |
| Common content width | 1,056–1,072 px |
| Page top offset | 64 px |
| Main horizontal breathing room | ≈ 80–96 px before the content column |
| Sidebar outer padding | 8 px |
| Sidebar nav row | 263 × 30 px |

Recommended shell:

```css
.shell { display: grid; grid-template-columns: 280px minmax(0, 1fr); }
.sidebar { position: sticky; top: 0; height: 100dvh; padding: 8px; }
.main { padding: 64px 80px 80px; }
.container { width: min(100%, 1072px); margin-inline: auto; }
```

### 7.2 Sidebar construction

- Background `#181818`; 1 px 8%-white separator on the right.
- Nav items are 30 px high, 6 px radius, 10 px horizontal padding, 8 px icon gap.
- Selected and hover rows use a 6% off-white overlay. No accent-blue selection rail.
- Icons are 14–16 px, normally secondary text color.
- Group separation is achieved with 16–18 px vertical gaps, not headings in boxes.
- User/profile control is pinned to the lower edge.
- Search/toggle icon buttons are 24–28 px squares.

## 8. Components

### 8.1 Buttons

#### Primary

- Height: 28 px.
- Padding: 0 8 px.
- Radius: 6 px.
- Gap: 6 px.
- Font: 13/18, weight 400.
- Background: `#f0f0f0`.
- Text: `#181818`.
- Hover background: `color-mix(in oklab, #141414 10%, #f0f0f0)`.

#### Secondary outline

- Same geometry as primary.
- Transparent background.
- Text `#f0f0f0`.
- 1 px inset outline at 8% white.
- Hover: 6% white overlay.

#### Ghost

- Transparent, usually secondary text.
- Hover: primary text plus 6% overlay.
- Use for quiet actions such as Skip or utility controls.

#### Icon button

- 24 × 24 px for the tight sidebar/header utility.
- 28 × 28 px for standard control rows.
- 6 px radius; no default fill.

#### Destructive

- Keep the surface dark; use red for text/border rather than a large red fill.
- Destructive action still requires explicit confirmation in application behavior.

### 8.2 Inputs and search

- Height: 30 px standard; 28 px in extra-compact toolbars.
- Background: `#181818` or a 6% white overlay over `#141414`.
- Border: 1 px at 12% white.
- Radius: 6 px.
- Padding: 0 10 px.
- Placeholder: 36% white.
- Focus: accent border at 56% blue plus a restrained 2 px, 12%-blue outer ring.
- Search icon: 14 px; 8 px gap from text.

Avoid floating labels and oversized 44–48 px fields on desktop; they break the workbench density.

### 8.3 Tabs

Two patterns are used:

1. **Pill tabs** for high-level binary/group filters: 28 px high, `5px 10px`, fully rounded. Selected background is an 8% off-white overlay.
2. **Segmented micro tabs** for compact chart/filter controls: 22 px high, `2px 6px`, 4 px radius. Selected background is an 8% overlay.

Unselected tabs use secondary text and no container border.

### 8.4 Cards

Base card:

- Background `#181818`.
- Radius 12 px.
- 1 px 4%-white outline, often implemented as an inset shadow.
- Standard padding 24 px; compact padding 16 px.
- No default elevation shadow.

Measured card widths were commonly 1,072 px. The Automations content cards measured 1,056 px because their wrapper uses slightly different page gutters.

### 8.5 Settings rows

- Group related rows inside one 12 px card.
- Typical row height: 64 px.
- Internal padding: about 14 px vertical, 16 px horizontal.
- Label + helper text on the left; control on the right.
- Use a 4% white separator between rows.
- Longer/complex rows expand naturally to 86–232 px rather than compressing their contents.

### 8.6 Tables

- Table sits inside a 12 px card with overflow clipped.
- Header is quiet: 13 px or 12 px, regular weight, tertiary text.
- Header row ≈ 36 px; body rows at least 44 px.
- Horizontal cell padding: 12 px.
- Row separators: 4% white.
- Hover: 4% white overlay.
- Actions align right and use ghost or secondary buttons.
- Empty table state remains inside the table card rather than replacing the entire page.

### 8.7 Badges and status chips

- 11/14 text.
- 1–2 px vertical and 6 px horizontal padding.
- Full radius.
- Neutral default: 8% off-white background and secondary text.
- Semantic background: 12% of the status hue; text uses the full or 78% hue.
- Keep labels short: `Current`, `Early Beta`, `Merged`, `Open`, `Setup`.

### 8.8 Switches

- Compact pill geometry, approximately 28 × 16 px.
- Off track: 14% white.
- On track: accent blue.
- Knob: 12 px, 2 px inset.
- Transition: 150 ms.

### 8.9 Empty states

Observed small empty state: 1,072 × 102 px with 32 px vertical padding and 8 px radius. Larger table empty states use a 12 px card and center the content inside the table area.

Pattern:

- One short label in primary text.
- One sentence in secondary/tertiary text.
- At most two compact actions.
- Do not use oversized illustrations unless the page is an onboarding/feature showcase.

### 8.10 Feature and onboarding panels

The Automations page uses a 12 px split panel: image/demo on one side, dark content on the other. Inside, selectable feature rows use 8% overlays and 12 px radii. A primary 28 px button spans most of the content column.

Keep special media panels visually exceptional. The rest of the dashboard should remain flat and data-first.

### 8.11 Agent composer

- Main prompt control is the most rounded recurring surface: approximately 16 px radius.
- Composer content uses 16/24 text.
- Tool/model/voice controls stay compact and sit within or directly below the prompt surface.
- Supporting controls and recent-agent cards return to 13/18 text and 6–12 px radii.
- The page should feel centered and calm, with more empty space than analytics/settings pages.

## 9. Page archetypes

Use these templates to reproduce Cursor's information architecture.

### Overview / account home

- Credit/stat banner at the top.
- Setup checklist/accordion.
- Plan cards and usage summary.
- Analytics section.
- Integration rows.
- Strong content hierarchy comes from vertical rhythm and cards, not large headings.

### Settings

- One 1,072 px column of stacked 12 px cards.
- Each card contains compact horizontal settings rows.
- The dangerous account actions remain at the bottom.

### Integration catalog

- Group integrations into large 12 px cards.
- Each integration is a 64–67 px row with icon, name, description, and right-aligned 28 px action.
- Disabled/admin-required states use muted copy instead of extra color.

### Data/API table

- Short page introduction.
- One dominant 12 px table card.
- Add action in the page toolbar.
- Tokens/identifiers use mono type and truncation.

### Analytics / usage

- Date control + compact range segments at the top.
- KPI strip.
- Chart card.
- Filter/group button and export action.
- Table or centered no-results state below.

### Spending/billing

- Two-up plan cards when comparison is useful; each card measured about 528 px inside a 1,072 px row.
- Full-width 1,072 px cards for credit, usage, payment, invoices, and cancellation.
- Plan cards use 16 px padding; account/billing cards typically use 24 px.
- Reserve tinted blue/cyan backgrounds for a single recommended/upgrade card.

### Membership upsell

- Centered headline and one-sentence description.
- 2 × 2 feature grid with small icons.
- One primary 28 px action and one lower-emphasis sales action.
- Keep the surrounding surface empty.

## 10. Interaction states

| State | Treatment |
|---|---|
| Default | Transparent or `#181818`; primary/secondary text |
| Hover | Add 6% white overlay; raise text toward primary |
| Selected nav | 6% white overlay, primary text; no blue bar |
| Selected tab | 8% white overlay |
| Focus | Accent border/ring, never a heavy glow |
| Pressed | 14–16% white overlay or slight transform; keep under 100 ms if transforming |
| Disabled | Quaternary text / ≈ 36% opacity; remove hover response |
| Loading | Keep layout dimensions stable; use quiet progress/skeleton treatment |
| Error | Red text/border/tint; do not turn the whole page red |

## 11. Responsive adaptation

The desktop measurements above are exact. The following is an inferred implementation that preserves the theme without claiming to be Cursor's exact internal breakpoints:

- **≥ 1,200 px:** 280 px sidebar, centered max-width 1,072 px content, 64/80 px page padding.
- **960–1,199 px:** Collapse sidebar to a 64 px icon rail; reduce main horizontal padding to 32 px.
- **640–959 px:** Keep single-column cards; allow dense tables to scroll horizontally; stack two-up plan cards.
- **< 640 px:** Move primary navigation to a 56 px bottom rail or off-canvas sheet; page padding 16 px; stack setting-row controls below labels; preserve 28–32 px control height unless touch requirements demand a larger invisible hit target.

For touch accessibility, keep the visible control at Cursor's compact size but wrap it in a 40–44 px hit area when possible.

## 12. Accessibility guardrails

- Primary text on `#141414` and `#181818` is high contrast. Secondary and tertiary text should not carry essential information alone.
- Use the accent focus ring on every keyboard-focusable control.
- Pair status colors with labels/icons; never rely on hue alone.
- Preserve actual button, link, tab, switch, heading, table, and region semantics.
- Keep destructive actions visually quiet but behaviorally explicit with confirmation.
- Provide tooltips for icon-only controls.
- Use `prefers-reduced-motion` and maintain stable layouts during loading.

## 13. Fidelity checklist

Before shipping a Cursor-themed application, verify:

- [ ] Page background is `#141414`; sidebar/cards are `#181818`.
- [ ] Main UI copy is primarily 13/18 at weight 400.
- [ ] Page titles are 20/28 at weight 400, not oversized/bold.
- [ ] Desktop shell uses a 280 px sidebar and ≈ 1,072 px content column.
- [ ] Nav rows are 30 px high with 6 px radius.
- [ ] Standard controls are 28–30 px high with 6 px radius.
- [ ] Primary cards are 12 px radius with 4–8% white outlines.
- [ ] Spacing concentrates around 4/6/8/12/16/24 px.
- [ ] Hover/selected neutrals use white overlays, not unrelated gray fills.
- [ ] Blue is used sparingly for focus/progress, not every active surface.
- [ ] Shadows are reserved for popovers and true elevation.
- [ ] Transitions are mostly 150 ms with `cubic-bezier(0.4, 0, 0.2, 1)`.
- [ ] Icons are small outline glyphs and visually secondary to text.
- [ ] Status colors are paired with labels and low-opacity tints.
- [ ] Empty states are compact and useful, not decorative splash screens.

## 14. Common mistakes to avoid

- Using pure black (`#000`) instead of the warmer `#141414` foundation.
- Using pure white (`#fff`) for all text instead of `#f0f0f0` plus opacity levels.
- Making every card `#202020` with a visible gray border.
- Defaulting to 14–16 px body copy and 600-weight headings.
- Using 36–44 px desktop controls.
- Applying 12–16 px radius to buttons; Cursor's standard control radius is 6 px.
- Adding blue glow, gradients, glass blur, or prominent drop shadows to ordinary panels.
- Overusing cards where a simple row, separator, or empty space would suffice.
- Using accent blue for selected sidebar rows. Cursor uses a neutral overlay there.

## 15. Minimal starter markup

```html
<div class="cursor-shell">
  <aside class="cursor-sidebar">
    <nav class="cursor-nav" aria-label="Primary">
      <a class="cursor-nav-item" aria-current="page" href="#">
        <svg aria-hidden="true"><!-- outline icon --></svg>
        <span>Overview</span>
      </a>
      <a class="cursor-nav-item" href="#">
        <svg aria-hidden="true"><!-- outline icon --></svg>
        <span>Settings</span>
      </a>
    </nav>
  </aside>

  <main class="cursor-main">
    <div class="cursor-container">
      <header style="display:flex;align-items:center;justify-content:space-between;margin-bottom:24px">
        <div>
          <h1 class="cursor-page-title">Automations</h1>
          <p class="cursor-muted cursor-body">Automate repetitive work with always-on agents.</p>
        </div>
        <button class="cursor-button">New Automation</button>
      </header>

      <section class="cursor-card cursor-card--padded">
        <div class="cursor-setting-row">
          <div>
            <div class="cursor-body">Network access</div>
            <div class="cursor-caption">Control allowed destinations.</div>
          </div>
          <button class="cursor-switch" role="switch" aria-checked="false" aria-label="Network access"></button>
        </div>
      </section>
    </div>
  </main>
</div>
```

Use the companion CSS as the canonical token and component source. The document explains how to compose those tokens so the result feels like Cursor rather than merely sharing a dark palette.
