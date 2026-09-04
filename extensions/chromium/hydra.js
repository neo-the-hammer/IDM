/**
 * Talking to the Hydra daemon.
 *
 * The connection details are discovered rather than configured: the daemon
 * publishes its port and API token to a file on startup, and the native
 * messaging host reads that file on the extension's behalf. Asking a user to
 * locate daemon.json and paste a token is a miserable first run, so that path
 * exists only as a fallback for setups where the host is not registered.
 */

import './compat.js';

/** Registered name of the native messaging host. */
const NATIVE_HOST = 'com.hydradm.host';

/** Cached connection, so every capture does not re-launch the host. */
let connection = null;
let connectionCheckedAt = 0;
const CONNECTION_TTL = 30_000;

export const DEFAULT_SETTINGS = {
  /** Take over downloads the browser starts. */
  captureEnabled: true,
  /** Only capture files at least this many bytes; 0 captures everything. */
  minimumSize: 0,
  /** Never capture these extensions, whatever their size. */
  excludedExtensions: [],
  /** Never capture from these hosts. */
  excludedHosts: [],
  /** Only capture these extensions, when the list is not empty. */
  includedExtensions: [],
  /** Connections per download; 0 uses the daemon's default. */
  connections: 0,
  /** Manually supplied connection, for when native messaging is unavailable. */
  manualUrl: '',
  manualToken: '',
  /** Watch responses for media so the popup can offer them. */
  sniffMedia: true,
};

export async function loadSettings() {
  const stored = await chrome.storage.sync.get(DEFAULT_SETTINGS);
  return { ...DEFAULT_SETTINGS, ...stored };
}

export async function saveSettings(settings) {
  await chrome.storage.sync.set(settings);
  // A changed manual connection must take effect at once.
  connection = null;
}

/**
 * Finds the daemon: the native host first, then anything configured by hand.
 */
export async function getConnection({ force = false } = {}) {
  if (!force && connection && Date.now() - connectionCheckedAt < CONNECTION_TTL) {
    return connection;
  }

  const viaHost = await askNativeHost();
  if (viaHost) {
    connection = viaHost;
    connectionCheckedAt = Date.now();
    return connection;
  }

  const settings = await loadSettings();
  if (settings.manualUrl && settings.manualToken) {
    connection = {
      url: settings.manualUrl.replace(/\/$/, ''),
      token: settings.manualToken,
      source: 'manual',
    };
    connectionCheckedAt = Date.now();
    return connection;
  }

  connection = null;
  return null;
}

async function askNativeHost() {
  try {
    const reply = await chrome.runtime.sendNativeMessage(NATIVE_HOST, {
      type: 'getConnection',
    });
    if (reply?.ok && reply.url && reply.token) {
      return { url: reply.url, token: reply.token, source: 'native' };
    }
  } catch {
    // The host is not registered, or Hydra is not running. Either way the
    // manual path below is the answer, not an error the user needs to see.
  }
  return null;
}

/** Calls the daemon's REST API. */
async function call(method, path, body) {
  const link = await getConnection();
  if (!link) {
    throw new Error('Hydra is not running, or the extension is not paired with it.');
  }
  const response = await fetch(`${link.url}/api/v1${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${link.token}`,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  const parsed = text ? JSON.parse(text) : null;
  if (!response.ok) {
    // A stale token means the daemon restarted; drop the cache so the next
    // attempt re-pairs rather than failing forever.
    if (response.status === 401) connection = null;
    throw new Error(parsed?.error ?? `Hydra returned ${response.status}`);
  }
  return parsed;
}

export async function isAvailable() {
  try {
    await call('GET', '/health');
    return true;
  } catch {
    return false;
  }
}

export async function totals() {
  return call('GET', '/totals');
}

/**
 * Hands a download to Hydra.
 *
 * The cookies, referer and user-agent are replayed deliberately: a great many
 * links are only valid for the session that produced them, and without them
 * Hydra would fetch a login page instead of a file.
 */
export async function sendDownload({
  url,
  filename,
  referer,
  cookies,
  connections,
  queue,
  streaming,
}) {
  const request = { url };
  if (filename) request.filename = filename;
  if (referer) request.referer = referer;
  if (cookies) request.cookies = cookies;
  if (connections) request.connections = connections;
  if (queue) request.queue = queue;
  request.userAgent = navigator.userAgent;
  // An .m3u8 or .mpd is an index, not a video. Sending one to the plain
  // download route saves a few kilobytes of text with a film's name on it,
  // which is the whole reason the media route exists.
  return call('POST', streaming ? '/media/download' : '/downloads', request);
}

/** Collects the cookies a request to `url` would have carried. */
export async function cookieHeaderFor(url) {
  try {
    const jar = await chrome.cookies.getAll({ url });
    if (!jar.length) return '';
    return jar.map((c) => `${c.name}=${c.value}`).join('; ');
  } catch {
    // The cookies permission may have been declined; the download may still
    // work, so this is not worth failing over.
    return '';
  }
}

export function openInterface() {
  getConnection().then((link) => {
    if (link) chrome.tabs.create({ url: link.url });
    else chrome.runtime.openOptionsPage();
  });
}
