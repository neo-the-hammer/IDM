/**
 * The capture service worker.
 *
 * This is what makes Hydra behave like IDM: a download the browser starts is
 * taken over and handed to the daemon, complete with the session context that
 * made the link work in the first place.
 */

import './compat.js';
import {
  cookieHeaderFor,
  getConnection,
  loadSettings,
  openInterface,
  sendDownload,
  totals,
} from './hydra.js';

/** Downloads this extension has already claimed, so a retry is not captured twice. */
const claimed = new Set();

/** Media seen per tab, offered in the popup. */
const mediaByTab = new Map();
const MAX_MEDIA_PER_TAB = 40;

/** Extensions a browser handles inline, which capturing would only annoy. */
const NEVER_CAPTURE = new Set([
  'html', 'htm', 'xhtml', 'php', 'asp', 'aspx', 'jsp',
  'css', 'js', 'mjs', 'json', 'xml', 'svg', 'ico', 'txt',
]);

// ------------------------------------------------------------------ capture

chrome.downloads.onCreated.addListener(async (item) => {
  try {
    const settings = await loadSettings();
    if (!settings.captureEnabled) return;
    if (claimed.has(item.id)) return;
    if (!(await shouldCapture(item, settings))) return;

    // Confirm the daemon is reachable *before* cancelling, or a capture
    // against a stopped daemon would lose the download entirely.
    const link = await getConnection();
    if (!link) {
      notify('Hydra is not running', 'The browser will download this file itself.');
      return;
    }

    claimed.add(item.id);
    await takeOver(item, settings);
  } catch (error) {
    notify('Hydra could not take over the download', String(error?.message ?? error));
  }
});

async function takeOver(item, settings) {
  const url = item.finalUrl || item.url;
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const cookies = await cookieHeaderFor(url);

  try {
    await sendDownload({
      url,
      filename,
      referer: item.referrer || undefined,
      cookies: cookies || undefined,
      connections: settings.connections || undefined,
    });
  } catch (error) {
    // Handing off failed, so leave the browser's own download alone rather
    // than cancelling it and leaving the user with nothing.
    claimed.delete(item.id);
    notify('Hydra could not take the download', String(error?.message ?? error));
    return;
  }

  // Only now stop the browser's copy. removeFile clears the partial bytes it
  // already wrote; erase takes the cancelled entry out of its download list.
  await chrome.downloads.cancel(item.id).catch(() => {});
  await chrome.downloads.removeFile(item.id).catch(() => {});
  await chrome.downloads.erase({ id: item.id }).catch(() => {});
  await updateBadge();
}

async function shouldCapture(item, settings) {
  const url = item.finalUrl || item.url || '';
  // blob: and data: URLs exist only inside the page; there is nothing for a
  // separate process to fetch.
  if (!/^(https?|ftps?):\/\//i.test(url)) return false;

  let host = '';
  try {
    host = new URL(url).hostname.toLowerCase();
  } catch {
    return false;
  }
  if (settings.excludedHosts.some((h) => host === h || host.endsWith(`.${h}`))) return false;

  const extension = extensionOf(item.filename || url);
  if (extension && NEVER_CAPTURE.has(extension)) return false;
  if (settings.excludedExtensions.includes(extension)) return false;
  if (settings.includedExtensions.length > 0 && !settings.includedExtensions.includes(extension)) {
    return false;
  }

  // A size of -1 means the server did not say; capture it rather than guess.
  if (settings.minimumSize > 0 && item.fileSize > 0 && item.fileSize < settings.minimumSize) {
    return false;
  }
  return true;
}

function extensionOf(name) {
  const clean = String(name).split(/[?#]/)[0].split(/[\\/]/).pop() ?? '';
  const dot = clean.lastIndexOf('.');
  if (dot <= 0) return '';
  const extension = clean.slice(dot + 1).toLowerCase();
  return extension.length <= 6 ? extension : '';
}

// ------------------------------------------------------------ context menus

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: 'hydra-link',
      title: 'Download with Hydra',
      contexts: ['link'],
    });
    chrome.contextMenus.create({
      id: 'hydra-media',
      title: 'Download this media with Hydra',
      contexts: ['image', 'video', 'audio'],
    });
    chrome.contextMenus.create({
      id: 'hydra-open',
      title: 'Open Hydra',
      contexts: ['action'],
    });
  });
  updateBadge();
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === 'hydra-open') {
    openInterface();
    return;
  }
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  try {
    const settings = await loadSettings();
    await sendDownload({
      url,
      referer: info.pageUrl || tab?.url,
      cookies: (await cookieHeaderFor(url)) || undefined,
      connections: settings.connections || undefined,
    });
    notify('Sent to Hydra', decodeURIComponent(url.split('/').pop() ?? url));
    await updateBadge();
  } catch (error) {
    notify('Hydra could not take the link', String(error?.message ?? error));
  }
});

// ------------------------------------------------------------ media sniffing

const MEDIA_TYPES = /^(video|audio)\//i;
const MEDIA_MANIFESTS = /(mpegurl|dash\+xml|x-mpegurl)/i;

chrome.webRequest?.onHeadersReceived.addListener(
  (details) => {
    if (details.tabId < 0) return;
    const headers = details.responseHeaders ?? [];
    const header = (name) =>
      headers.find((h) => h.name.toLowerCase() === name)?.value ?? '';
    const type = header('content-type');
    if (!MEDIA_TYPES.test(type) && !MEDIA_MANIFESTS.test(type)) return;

    loadSettings().then((settings) => {
      if (!settings.sniffMedia) return;
      const list = mediaByTab.get(details.tabId) ?? [];
      if (list.some((m) => m.url === details.url)) return;
      list.unshift({
        url: details.url,
        type,
        size: Number(header('content-length')) || 0,
        // A manifest means the media is segmented, which is a different job
        // from a plain file and is flagged so the popup can say so.
        streaming: MEDIA_MANIFESTS.test(type),
        seenAt: Date.now(),
      });
      mediaByTab.set(details.tabId, list.slice(0, MAX_MEDIA_PER_TAB));
    });
  },
  { urls: ['<all_urls>'] },
  ['responseHeaders'],
);

chrome.tabs.onRemoved.addListener((tabId) => mediaByTab.delete(tabId));
chrome.tabs.onUpdated.addListener((tabId, changes) => {
  // A new page means the old page's media is no longer on offer.
  if (changes.url) mediaByTab.delete(tabId);
});

// ------------------------------------------------------- popup conversation

chrome.runtime.onMessage.addListener((message, _sender, respond) => {
  (async () => {
    switch (message?.type) {
      case 'media':
        respond({ media: mediaByTab.get(message.tabId) ?? [] });
        break;
      case 'status': {
        const link = await getConnection({ force: message.force === true });
        if (!link) {
          respond({ connected: false });
          break;
        }
        try {
          respond({ connected: true, source: link.source, url: link.url, totals: await totals() });
        } catch (error) {
          respond({ connected: false, error: String(error?.message ?? error) });
        }
        break;
      }
      case 'download':
        try {
          await sendDownload(message.request);
          await updateBadge();
          respond({ ok: true });
        } catch (error) {
          respond({ ok: false, error: String(error?.message ?? error) });
        }
        break;
      case 'open':
        openInterface();
        respond({ ok: true });
        break;
      default:
        respond({ ok: false, error: 'unknown message' });
    }
  })();
  // Keep the message channel open for the async work above.
  return true;
});

// ------------------------------------------------------------------- badge

/** Shows how many downloads are active, so the toolbar icon is worth glancing at. */
async function updateBadge() {
  try {
    const counts = await totals();
    const active = counts?.active ?? 0;
    await chrome.action.setBadgeText({ text: active > 0 ? String(active) : '' });
    await chrome.action.setBadgeBackgroundColor({ color: '#5b8cff' });
  } catch {
    await chrome.action.setBadgeText({ text: '' });
  }
}

chrome.alarms?.create('hydra-poll', { periodInMinutes: 0.25 });
chrome.alarms?.onAlarm.addListener((alarm) => {
  if (alarm.name === 'hydra-poll') updateBadge();
});

function notify(title, message) {
  chrome.notifications?.create({
    type: 'basic',
    iconUrl: 'icons/icon-128.png',
    title,
    message: message.slice(0, 300),
  });
}
