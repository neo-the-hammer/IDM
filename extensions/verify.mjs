/**
 * Loads the extension in a real Chromium and checks it does its one job:
 * taking a download away from the browser and handing it to Hydra, with the
 * session context that made the link work.
 *
 * Usage: node extensions/verify.mjs [--url http://127.0.0.1:PORT/file]
 *
 * Requires a running daemon whose daemon.json is readable, and a URL that
 * serves a file. Playwright must be installed (globally is fine).
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

async function loadPlaywright() {
  const candidates = ['playwright', 'playwright-core'];
  try {
    const root = execSync('npm root -g', { encoding: 'utf8' }).trim();
    for (const name of ['playwright', 'playwright-core']) {
      candidates.push(pathToFileURL(path.join(root, name, 'index.mjs')).href);
    }
  } catch {
    /* npm not on PATH */
  }
  for (const specifier of candidates) {
    try {
      return await import(specifier);
    } catch {
      /* try the next */
    }
  }
  throw new Error('Playwright not found. Install it with `npm install -g playwright`.');
}

const args = process.argv.slice(2);
const flag = (name, fallback) => {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? args[index + 1] : fallback;
};

const EXT = path.resolve(path.dirname(new URL(import.meta.url).pathname), 'chromium');
const TARGET = flag('url', 'http://127.0.0.1:38090/report.pdf');
const DATA_DIR =
  flag('data-dir', process.env.HYDRA_DATA_DIR) ??
  path.join(os.homedir(), '.local/share/hydra');

const daemon = JSON.parse(fs.readFileSync(path.join(DATA_DIR, 'daemon.json'), 'utf8'));
const authorized = { Authorization: `Bearer ${daemon.token}` };
const listDownloads = () =>
  fetch(`${daemon.url}/api/v1/downloads`, { headers: authorized }).then((r) => r.json());

let failures = 0;
const check = (label, ok, detail = '') => {
  console.log(`${ok ? 'ok  ' : 'FAIL'}  ${label}${detail ? `  (${detail})` : ''}`);
  if (!ok) failures += 1;
};

const { chromium } = await loadPlaywright();
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'hydra-ext-'));
const context = await chromium.launchPersistentContext(profile, {
  channel: 'chromium',
  args: [`--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`, '--no-first-run'],
});

const webErrors = [];
context.on('weberror', (e) => webErrors.push(String(e.error())));

let worker =
  context.serviceWorkers()[0] ?? (await context.waitForEvent('serviceworker', { timeout: 20000 }));
// An MV3 service worker can be mid-start when its handle first appears.
for (let i = 0; i < 40; i += 1) {
  const ready = await worker
    .evaluate(() => typeof chrome !== 'undefined' && Boolean(chrome.runtime?.id))
    .catch(() => false);
  if (ready) break;
  await new Promise((r) => setTimeout(r, 250));
  worker = context.serviceWorkers()[0] ?? worker;
}
const extensionId = new URL(worker.url()).host;
check('the extension loads', Boolean(extensionId), extensionId);

const manifest = await worker.evaluate(() => chrome.runtime.getManifest());
check('it is a Manifest V3 extension', manifest.manifest_version === 3);
for (const permission of ['downloads', 'cookies', 'nativeMessaging']) {
  check(`it requests the ${permission} permission`, manifest.permissions.includes(permission));
}

// Pair by hand: a throwaway profile has no native host registered.
await worker.evaluate(
  async (d) =>
    chrome.storage.sync.set({
      manualUrl: d.url,
      manualToken: d.token,
      captureEnabled: true,
      minimumSize: 0,
    }),
  daemon,
);

// Seed a cookie for the origin, since forwarding it is the whole reason
// capture works for links that are only valid within a session.
const origin = new URL(TARGET).origin;
await worker.evaluate(
  async (o) =>
    chrome.cookies.set({ url: o, name: 'hydra_session', value: 'proof-of-forwarding' }),
  origin,
);

const options = await context.newPage();
await options.goto(`chrome-extension://${extensionId}/options.html`);
const status = await options.evaluate(() =>
  chrome.runtime.sendMessage({ type: 'status', force: true }),
);
check('it reaches the daemon', status?.connected === true, status?.error ?? '');
await options.close();

// The real test.
const before = await listDownloads();
const page = await context.newPage();
await page.goto(origin);
await page.evaluate((url) => {
  const link = document.createElement('a');
  link.href = url;
  link.download = '';
  document.body.append(link);
  link.click();
}, TARGET);

let after = before;
for (let i = 0; i < 40; i += 1) {
  await page.waitForTimeout(500);
  after = await listDownloads();
  if (after.downloads.length > before.downloads.length) break;
}

const captured = after.downloads.find(
  (d) => d.spec.url === TARGET && !before.downloads.some((b) => b.id === d.id),
);
check('a browser download is handed to Hydra', Boolean(captured));

if (captured) {
  const headers = Object.fromEntries(
    (captured.spec.headers ?? []).map((h) => [h.name.toLowerCase(), h.value]),
  );
  check('the session cookie is forwarded', (headers.cookie ?? '').includes('hydra_session'),
    headers.cookie ?? 'no Cookie header');
  check('the user agent is forwarded', Boolean(headers['user-agent']));
  check('the referring page is forwarded', Boolean(headers.referer), headers.referer ?? '');
  // Clean up so repeated runs do not pile up entries.
  await fetch(`${daemon.url}/api/v1/downloads/${captured.id}?deleteFiles=true`, {
    method: 'DELETE',
    headers: authorized,
  });
}

// A streaming manifest must reach the media grabber rather than the plain
// download route, or the "video" that arrives is a few kilobytes of text.
const STREAM = new URL('hi.m3u8', origin).href;
const beforeStream = await listDownloads();

// Load the playlist the way a player would, so the extension sniffs it from
// the response's content type rather than from the address bar.
await page.evaluate((url) => fetch(url).then((r) => r.text()), STREAM);
await page.waitForTimeout(600);

// These have to be sent from an extension *page*: a runtime message sent from
// the service worker is not delivered to the service worker's own listener.
const media = await context.newPage();
await media.goto(`chrome-extension://${extensionId}/options.html`);

const sniffed = await media.evaluate(
  (pageOrigin) =>
    new Promise((resolve) => {
      chrome.tabs.query({ url: `${pageOrigin}/*` }, ([tab]) => {
        chrome.runtime.sendMessage({ type: 'media', tabId: tab?.id }, (reply) =>
          resolve(reply?.media ?? []),
        );
      });
    }),
  origin,
);
const playlist = sniffed.find((item) => item.url === STREAM);
check('the playlist is noticed as a stream', playlist?.streaming === true, JSON.stringify(sniffed));

// This is what the popup's "Get" button sends.
const sent = await media.evaluate(
  (url) =>
    new Promise((resolve) => {
      chrome.runtime.sendMessage({ type: 'download', request: { url, streaming: true } }, resolve);
    }),
  STREAM,
);
check('the stream is accepted', sent?.ok === true, sent?.error ?? '');
await media.close();

let afterStream = beforeStream;
for (let i = 0; i < 40; i += 1) {
  await page.waitForTimeout(500);
  afterStream = await listDownloads();
  if (afterStream.downloads.length > beforeStream.downloads.length) break;
}
const stream = afterStream.downloads.find(
  (d) => !beforeStream.downloads.some((b) => b.id === d.id),
);
check(
  'the stream goes to the media grabber, not the file route',
  Boolean(stream?.spec?.media),
  stream ? JSON.stringify(stream.spec.media ?? null) : 'nothing was added',
);
if (stream) {
  await fetch(`${daemon.url}/api/v1/downloads/${stream.id}?deleteFiles=true`, {
    method: 'DELETE',
    headers: authorized,
  });
}

const leftBehind = await worker.evaluate(
  () => new Promise((r) => chrome.downloads.search({}, (items) => r(items.length))),
);
check('the browser keeps no copy of its own', leftBehind === 0, `${leftBehind} left`);
check('no uncaught errors', webErrors.length === 0, webErrors.join('; '));

await context.close();
fs.rmSync(profile, { recursive: true, force: true });
process.exit(failures === 0 ? 0 : 1);
