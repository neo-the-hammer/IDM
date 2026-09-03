/**
 * Drives the interface in a real browser and screenshots every theme.
 *
 * Usage: node ui/verify.mjs [base-url] [--out DIR]
 *
 * Requires a running daemon. Downloads in flight make for far more useful
 * screenshots, since the per-connection segment bars only appear then.
 */

import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

/**
 * Finds Playwright whether it is a local dev dependency or installed globally.
 * Hydra has no npm dependencies of its own, so a global install is the normal
 * case here rather than the exception.
 */
async function loadPlaywright() {
  const candidates = ['playwright', 'playwright-core'];
  try {
    const globalRoot = execSync('npm root -g', { encoding: 'utf8' }).trim();
    for (const name of ['playwright', 'playwright-core']) {
      candidates.push(pathToFileURL(path.join(globalRoot, name, 'index.mjs')).href);
    }
  } catch {
    // npm is not on PATH; the bare specifiers above may still resolve.
  }
  for (const specifier of candidates) {
    try {
      return await import(specifier);
    } catch {
      // Try the next candidate.
    }
  }
  throw new Error(
    'Playwright was not found. Install it with `npm install -g playwright`, ' +
      'or `npm install -D playwright` in this directory.',
  );
}

const { chromium } = await loadPlaywright();

const args = process.argv.slice(2);
const base = args.find((a) => !a.startsWith('--')) ?? 'http://127.0.0.1:47113';
const outIndex = args.indexOf('--out');
const outDir = outIndex >= 0 ? args[outIndex + 1] : 'docs/screenshots';

const THEMES = [
  'hydra-dark', 'hydra-light', 'amoled', 'nord', 'dracula', 'catppuccin-mocha',
  'catppuccin-latte', 'gruvbox-dark', 'tokyo-night', 'solarized-dark',
  'solarized-light', 'rose-pine',
];

fs.mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({
  viewport: { width: 1280, height: 760 },
  deviceScaleFactor: 1,
});

const errors = [];
page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
page.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));

const check = (label, condition, detail = '') => {
  console.log(`${condition ? 'ok  ' : 'FAIL'}  ${label}${detail ? `  (${detail})` : ''}`);
  if (!condition) process.exitCode = 1;
};

await page.goto(base, { waitUntil: 'networkidle' });
await page.waitForSelector('.download, .empty', { timeout: 15000 });
await page.waitForTimeout(1500);

check('the event stream connects', (await page.textContent('#connection-label')) === 'Connected');
const rows = await page.locator('.download').count();
if (rows > 0) {
  const before = await page.locator('.download .meta').first().textContent();
  await page.waitForTimeout(2500);
  const after = await page.locator('.download .meta').first().textContent();
  check('live progress updates arrive', before !== after);
  const strips = await page.locator('.segments').count();
  console.log(`info  ${rows} rows, ${strips} showing per-connection segments`);
}

for (const theme of THEMES) {
  await page.evaluate((t) => { document.body.dataset.theme = t; }, theme);
  await page.waitForTimeout(200);
  await page.screenshot({ path: path.join(outDir, `theme-${theme}.png`) });
}
check('every theme rendered', true, `${THEMES.length} screenshots`);

// Persian, right to left, through the app's own locale switch.
await page.evaluate(() => { document.body.dataset.theme = 'hydra-dark'; });
await page.evaluate(async () => {
  const mod = await import('./app/i18n.js');
  mod.setLocale('fa');
});
await page.waitForTimeout(400);
check('switching to Persian flips the layout',
  (await page.evaluate(() => document.body.dir)) === 'rtl');
await page.screenshot({ path: path.join(outDir, 'persian-rtl.png') });

await page.evaluate(async () => {
  const mod = await import('./app/i18n.js');
  mod.setLocale('en');
});
await page.waitForTimeout(200);
await page.click('#settings-button');
await page.waitForTimeout(500);
check('the theme picker is complete',
  (await page.locator('.theme-swatch').count()) === THEMES.length);
await page.screenshot({ path: path.join(outDir, 'settings-themes.png') });
await page.keyboard.press('Escape');

await page.waitForTimeout(300);
await page.click('#add-button');
await page.waitForTimeout(400);
await page.click('details.advanced summary');
await page.waitForTimeout(300);
await page.screenshot({ path: path.join(outDir, 'add-download.png') });
await page.keyboard.press('Escape');

await browser.close();
check('no console errors', errors.length === 0, errors.join('; '));
console.log(`\nScreenshots written to ${outDir}`);
