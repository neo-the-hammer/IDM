/** The toolbar popup: status at a glance, plus anything worth grabbing here. */

import './compat.js';
const $ = (id) => document.getElementById(id);
const ask = (message) => chrome.runtime.sendMessage(message);

function humanBytes(value) {
  if (!value || value < 0) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? size : size.toFixed(1)} ${units[unit]}`;
}

async function refresh(force = false) {
  const status = await ask({ type: 'status', force });
  const state = $('state');

  if (status?.connected) {
    state.textContent = status.source === 'native' ? 'Connected' : 'Connected (manual)';
    state.className = 'state ok';
    $('stats').hidden = false;
    $('disconnected').hidden = true;
    $('stat-active').textContent = String(status.totals?.active ?? 0);
    $('stat-speed').textContent = humanBytes(status.totals?.speed ?? 0) + '/s';
    $('stat-total').textContent = String(status.totals?.total ?? 0);
  } else {
    state.textContent = 'Not running';
    state.className = 'state bad';
    $('stats').hidden = true;
    $('disconnected').hidden = false;
  }
}

async function loadMedia() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id) return;
  const { media } = await ask({ type: 'media', tabId: tab.id });
  if (!media?.length) return;

  $('media-section').hidden = false;
  const list = $('media');
  list.replaceChildren(
    ...media.map((item) => {
      const row = document.createElement('li');
      const name = document.createElement('span');
      name.className = 'name';
      let label = item.url;
      try {
        label = decodeURIComponent(new URL(item.url).pathname.split('/').pop() || item.url);
      } catch {
        // Keep the raw URL if it will not parse.
      }
      name.textContent = label;
      name.title = item.url;

      const tag = document.createElement('span');
      tag.className = 'tag';
      // A manifest means segmented media, which needs the media grabber
      // rather than a plain transfer, so say so instead of pretending.
      tag.textContent = item.streaming ? 'stream' : humanBytes(item.size);

      const button = document.createElement('button');
      button.className = 'btn';
      button.textContent = 'Get';
      button.addEventListener('click', async () => {
        button.disabled = true;
        const result = await ask({
          type: 'download',
          request: { url: item.url, referer: tab.url },
        });
        button.textContent = result?.ok ? 'Sent' : 'Failed';
        if (!result?.ok) button.title = result?.error ?? '';
      });

      row.append(name, tag, button);
      return row;
    }),
  );
}

async function init() {
  const settings = await chrome.storage.sync.get({ captureEnabled: true });
  $('capture').checked = settings.captureEnabled;
  $('capture').addEventListener('change', (event) => {
    chrome.storage.sync.set({ captureEnabled: event.target.checked });
  });

  $('open').addEventListener('click', () => ask({ type: 'open' }));
  $('options').addEventListener('click', () => chrome.runtime.openOptionsPage());
  $('pair').addEventListener('click', () => chrome.runtime.openOptionsPage());
  $('retry').addEventListener('click', () => refresh(true));

  await refresh();
  await loadMedia();
}

init();
