# Theming Hydra

Twelve themes ship in the box. Adding another is two small edits, because the
stylesheet names no colours of its own.

## How it works

Every colour, radius and shadow in the interface is a CSS custom property.
`ui/styles/app.css` — the whole layout and every component — contains not one
literal colour. They all live in `ui/styles/themes.css`, one block per theme:

```css
[data-theme='nord'] {
  color-scheme: dark;
  --bg: #2e3440;
  --surface: #3b4252;
  --text: #eceff4;
  --accent: #88c0d0;
  /* …fourteen roles in total */
}
```

Switching themes sets one attribute on `<body>`. No reflow, no reload, no
rebuild.

## Adding one

**1. Define it** in `ui/styles/themes.css`. Copy an existing block and change
the values. All fourteen roles must be present — a missing one falls through to
whatever the previous theme set, which looks like a bug rather than a default.

| Role | Used for |
| --- | --- |
| `--bg` | The window behind everything |
| `--bg-elevated` | Toolbar, sidebar, status bar |
| `--bg-inset` | Input fields, empty progress track |
| `--surface`, `--surface-hover` | Cards and rows |
| `--border`, `--border-strong` | Dividers; focused and hovered edges |
| `--text`, `--text-dim`, `--text-faint` | Primary, secondary, tertiary text |
| `--accent`, `--accent-hover`, `--accent-contrast` | The accent, and text on it |
| `--success`, `--warning`, `--danger`, `--info` | Status |

Set `color-scheme` to `light` or `dark` so form controls and scrollbars match.

**2. List it** in `THEMES` in `ui/src/theme.ts`:

```ts
{ id: 'nord', name: 'Nord', dark: true },
```

**3. Rebuild** with `npx tsc -p ui/tsconfig.json`.

The picker builds each swatch from that theme's own variables, so the preview
is a real sample rather than a hand-maintained approximation that drifts.

## Accent colours

The accent can be overridden independently of the theme, which is why
`--accent` is read through `--accent-user`:

```css
[data-accent] { --accent: var(--accent-user, var(--accent)); }
```

Add to `ACCENTS` in `ui/src/theme.ts` to offer another.

## Right to left

The interface is written with CSS logical properties throughout —
`inline-start` rather than `left`, `margin-inline-end` rather than
`margin-right`. Selecting فارسی sets `dir="rtl"` and the whole layout flips
correctly, rather than needing a mirrored stylesheet.

Two details matter when adding to the interface:

- **Filenames and URLs are neutral-direction strings.** They carry
  `direction: ltr` so an English filename stays readable inside a right-to-left
  page. Anything that displays one should do the same.
- **Numbers go through `toLocaleString`.** That is what produces Persian digits
  and separators rather than a half-translated screen.

## Adding a language

`ui/src/i18n.ts` holds the catalogues. Add one, add its code to `Locale`, and
add it to `RTL` if its script runs right to left. Missing keys fall back to
English rather than showing the key, so a partial translation is usable
immediately.

## Checking your work

With a daemon running and a download or two in flight:

```sh
node ui/verify.mjs
```

It screenshots every theme in both text directions into `docs/screenshots/` and
fails on any console error — so a theme that breaks the layout shows up as a
picture rather than a bug report.
