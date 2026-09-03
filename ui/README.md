# The Hydra web interface

A single-page app in plain TypeScript and CSS. There is nothing to
`npm install` — no framework, no bundler, no `node_modules`.

## Building

```bash
npx tsc -p ui/tsconfig.json     # or: tsc -p ui/tsconfig.json
```

That compiles `ui/src/*.ts` to ES modules in `ui/app/`, which `index.html`
loads directly. The build output is committed so that anyone cloning the
repository can run the daemon without a TypeScript toolchain at all.

The daemon serves this directory and finds it automatically next to the
binary, under `share/hydra/ui`, or in the source tree. Point it elsewhere
with `hdmd --ui <dir>`.

## How it is put together

| File | Role |
| --- | --- |
| `index.html` | The shell. The daemon injects the API token into it as a `<meta>` tag. |
| `styles/themes.css` | Every theme, as a block of custom properties. Nothing else names a colour. |
| `styles/app.css` | Layout and components, entirely in terms of those properties. |
| `src/api.ts` | REST client and the reconnecting WebSocket event stream. |
| `src/render.ts` | The download list, reconciled by id rather than rebuilt. |
| `src/i18n.ts` | English and Persian, and the text direction that comes with them. |
| `src/theme.ts` | Theme and accent selection, remembered in `localStorage`. |
| `src/main.ts` | State, dialogs, and the wiring between them. |

### Adding a theme

Add one block to `styles/themes.css` defining the same fourteen properties as
the others, then one entry to `THEMES` in `src/theme.ts`. Nothing else needs to
change: the picker builds its own preview from the theme's variables, so the
swatch is a genuine sample rather than a hand-maintained approximation.

### Right-to-left

Persian is a first-class locale, not a mirrored afterthought. The layout is
written with CSS logical properties throughout (`inline-start`, not `left`), so
selecting فارسی flips the entire interface. Filenames are forced to `direction:
ltr` so an English filename stays readable inside a right-to-left page, and
numbers go through `toLocaleString`, which is what produces Persian digits and
separators rather than a half-translated interface.

## Verifying it

With a daemon running and some downloads in flight:

```bash
node ui/verify.mjs http://127.0.0.1:47113
```

It drives Chromium through Playwright: asserts the event stream connects and
progress advances, screenshots every theme in both text directions, and fails
on any console error.
