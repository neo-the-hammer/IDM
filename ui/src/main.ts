/** Bootstraps the interface and wires it to the daemon. */

import { Api, ApiError } from './api.js';
import { bytes, duration, parseRate, percent, rate, timestamp } from './format.js';
import { applyStaticStrings, locale, setLocale, t, type Locale } from './i18n.js';
import { invalidateRows, renderList, type RowAction } from './render.js';
import { ACCENTS, applyAccent, applyTheme, currentAccent, currentTheme, restoreAppearance, THEMES } from './theme.js';
import type { Download, Settings, Snapshot, Status, Totals } from './types.js';

type Filter = 'all' | 'downloading' | 'queued' | 'paused' | 'completed' | 'failed';

interface State {
  downloads: Download[];
  totals: Totals | null;
  settings: Settings | null;
  filter: Filter;
  search: string;
  connected: boolean;
}

const state: State = {
  downloads: [],
  totals: null,
  settings: null,
  filter: 'all',
  search: '',
  connected: false,
};

const api = new Api();
const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T;

/** Downloads that were finished last time we looked, so a toast fires once. */
const alreadyFinished = new Set<string>();
let firstSnapshot = true;

// ------------------------------------------------------------------ startup

function boot(): void {
  restoreAppearance();
  setLocale(detectLocale());
  buildThemePicker();
  buildAccentPicker();
  wireChrome();
  renderSidebar();
  renderStatusBar();

  api
    .settings()
    .then((settings) => {
      state.settings = settings;
      // The daemon remembers the user's choice across machines and browsers,
      // so it wins over whatever this browser happened to store.
      if (settings.theme) applyTheme(settings.theme);
      if (settings.language) setLocale(settings.language as Locale);
      redrawAll();
    })
    .catch(reportError);

  api.subscribe({
    onSnapshot: (snapshot) => applySnapshot(snapshot),
    onOpen: () => {
      state.connected = true;
      renderStatusBar();
    },
    onClose: () => {
      state.connected = false;
      renderStatusBar();
    },
  });

  // The stream carries everything, but fetch once so the list is populated
  // before the first push arrives.
  api
    .listDownloads()
    .then(({ downloads, totals }) => applySnapshot({ type: 'snapshot', downloads, totals }))
    .catch(reportError);
}

function detectLocale(): Locale {
  const stored = (() => {
    try {
      return localStorage.getItem('hydra.language');
    } catch {
      return null;
    }
  })();
  if (stored === 'fa' || stored === 'en') return stored;
  return navigator.language.startsWith('fa') ? 'fa' : 'en';
}

// ------------------------------------------------------------------ updates

function applySnapshot(snapshot: Snapshot): void {
  const previous = state.downloads;
  state.downloads = snapshot.downloads;
  state.totals = snapshot.totals;

  notifyNewlyFinished(previous, snapshot.downloads);
  renderMain();
  renderSidebar();
  renderStatusBar();
}

/** Announces downloads that finished since the last snapshot. */
function notifyNewlyFinished(previous: Download[], current: Download[]): void {
  if (firstSnapshot) {
    // Everything already complete on first load is history, not news.
    current.filter((d) => d.status === 'completed').forEach((d) => alreadyFinished.add(d.id));
    firstSnapshot = false;
    return;
  }
  const before = new Map(previous.map((d) => [d.id, d.status]));
  for (const download of current) {
    if (
      download.status === 'completed' &&
      before.get(download.id) !== 'completed' &&
      !alreadyFinished.has(download.id)
    ) {
      alreadyFinished.add(download.id);
      toast(t('toast.finished', { name: download.filename || download.spec.url }));
    }
  }
}

function redrawAll(): void {
  invalidateRows();
  applyStaticStrings();
  renderSidebar();
  renderMain();
  renderStatusBar();
  buildThemePicker();
  buildAccentPicker();
}

// ------------------------------------------------------------------ filters

const FILTERS: { id: Filter; key: string; match: (d: Download) => boolean }[] = [
  { id: 'all', key: 'nav.all', match: () => true },
  {
    id: 'downloading',
    key: 'nav.downloading',
    match: (d) => d.status === 'downloading' || d.status === 'probing' || d.status === 'verifying',
  },
  { id: 'queued', key: 'nav.queued', match: (d) => d.status === 'queued' },
  { id: 'paused', key: 'nav.paused', match: (d) => d.status === 'paused' },
  { id: 'completed', key: 'nav.completed', match: (d) => d.status === 'completed' },
  {
    id: 'failed',
    key: 'nav.failed',
    match: (d) => d.status === 'failed' || d.status === 'cancelled',
  },
];

function visibleDownloads(): Download[] {
  const filter = FILTERS.find((f) => f.id === state.filter) ?? FILTERS[0]!;
  const needle = state.search.trim().toLowerCase();
  return state.downloads.filter((download) => {
    if (!filter.match(download)) return false;
    if (!needle) return true;
    return (
      download.filename.toLowerCase().includes(needle) ||
      download.spec.url.toLowerCase().includes(needle)
    );
  });
}

// ------------------------------------------------------------------ rendering

function renderSidebar(): void {
  const sidebar = $('sidebar');
  const counts = new Map<Filter, number>();
  for (const filter of FILTERS) {
    counts.set(filter.id, state.downloads.filter(filter.match).length);
  }

  sidebar.replaceChildren();
  const group = document.createElement('div');
  group.className = 'nav-group';
  const heading = document.createElement('h3');
  heading.textContent = t('nav.status');
  group.append(heading);

  for (const filter of FILTERS) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'nav-item';
    button.setAttribute('aria-current', String(state.filter === filter.id));
    button.innerHTML = `<span>${t(filter.key)}</span><span class="count">${counts.get(filter.id) ?? 0}</span>`;
    button.addEventListener('click', () => {
      state.filter = filter.id;
      renderSidebar();
      renderMain();
    });
    group.append(button);
  }
  sidebar.append(group);

  const actions = document.createElement('div');
  actions.className = 'nav-group';
  const clear = document.createElement('button');
  clear.type = 'button';
  clear.className = 'nav-item';
  clear.textContent = t('action.clearCompleted');
  clear.addEventListener('click', () => {
    api
      .clearCompleted()
      .then(({ removed }) => toast(t('toast.cleared', { n: removed })))
      .catch(reportError);
  });
  actions.append(clear);
  sidebar.append(actions);
}

function renderMain(): void {
  const main = $('main');
  const downloads = visibleDownloads();
  main.dataset.filtered = String(state.filter !== 'all' || state.search.trim() !== '');
  renderList(main, downloads, {
    onAction: performAction,
    onOpen: openDetail,
  });
}

function renderStatusBar(): void {
  const lang = locale();
  $('connection-dot').className = `dot${state.connected ? ' live' : ''}`;
  $('connection-label').textContent = state.connected ? t('status.connected') : t('status.offline');

  const totals = state.totals;
  $('status-speed').textContent = totals && totals.speed > 0 ? rate(totals.speed, lang) : '';
  $('status-counts').textContent = totals
    ? `${totals.active} ${t('nav.downloading').toLowerCase()} · ${totals.total} ${t('nav.all').toLowerCase()}`
    : '';
  $('status-limit').textContent =
    totals && totals.speedLimit > 0 ? `${t('settings.speedLimit')}: ${rate(totals.speedLimit, lang)}` : '';
}

// ------------------------------------------------------------------ actions

function performAction(id: string, action: RowAction): void {
  const request =
    action === 'remove' ? api.remove(id) : api.action(id, action === 'resume' ? 'resume' : action);
  request
    .then(() => {
      if (action === 'remove') toast(t('toast.removed'));
    })
    .catch(reportError);
}

function openDetail(id: string): void {
  const download = state.downloads.find((d) => d.id === id);
  if (!download) return;
  const lang = locale();
  const dialog = $<HTMLDialogElement>('detail-dialog');

  $('detail-title').textContent = download.filename || download.spec.url;

  const rows: [string, string, boolean?][] = [
    [t('detail.url'), download.spec.url, true],
    [t('status.connected'), t(`status.${download.status}`)],
    [t('detail.size'), download.total ? bytes(download.total, lang) : '—'],
    [
      t('detail.progress'),
      `${bytes(download.downloaded, lang)} (${percent(download.downloaded, download.total, lang)})`,
    ],
    [t('detail.speed'), rate(download.speed, lang)],
    [t('detail.eta'), duration(download.eta, lang)],
    [t('detail.connections'), String(download.spec.connections)],
    [t('detail.type'), download.contentType ?? '—'],
    [t('detail.added'), timestamp(download.createdAt, lang)],
  ];
  if (download.outputPath) rows.push([t('detail.savedTo'), download.outputPath, true]);
  if (download.completedAt) rows.push([t('detail.finished'), timestamp(download.completedAt, lang)]);
  if (download.segments?.length) {
    rows.push([t('detail.segments'), String(download.segments.length)]);
  }
  if (download.error) rows.push([t('detail.error'), download.error]);

  const list = document.createElement('dl');
  for (const [term, value, ltr] of rows) {
    const dt = document.createElement('dt');
    dt.textContent = term;
    const dd = document.createElement('dd');
    dd.textContent = value;
    if (ltr) dd.className = 'ltr';
    list.append(dt, dd);
  }
  $('detail-body').replaceChildren(list);
  dialog.showModal();
}

// ------------------------------------------------------------------ chrome

function wireChrome(): void {
  // Row action buttons are delegated, so rows stay cheap to build.
  $('main').addEventListener('click', (event) => {
    const button = (event.target as HTMLElement).closest<HTMLElement>('button[data-action]');
    if (!button?.dataset.id) return;
    event.stopPropagation();
    performAction(button.dataset.id, button.dataset.action as RowAction);
  });

  $<HTMLInputElement>('search').addEventListener('input', (event) => {
    state.search = (event.target as HTMLInputElement).value;
    renderMain();
  });

  $('add-button').addEventListener('click', () => openAddDialog());
  $('settings-button').addEventListener('click', () => openSettingsDialog());
  $('pause-all').addEventListener('click', () => api.pauseAll().catch(reportError));
  $('resume-all').addEventListener('click', () => api.resumeAll().catch(reportError));
  $('settings-save').addEventListener('click', saveSettings);

  document.querySelectorAll<HTMLElement>('[data-close]').forEach((button) => {
    button.addEventListener('click', () => button.closest('dialog')?.close());
  });

  $<HTMLFormElement>('add-form').addEventListener('submit', (event) => {
    event.preventDefault();
    submitAdd();
  });

  document.addEventListener('keydown', (event) => {
    // Ctrl/Cmd+N to add, matching what a download manager's users expect.
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'n') {
      event.preventDefault();
      openAddDialog();
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
      event.preventDefault();
      $<HTMLInputElement>('search').focus();
    }
  });
}

function openAddDialog(): void {
  const dialog = $<HTMLDialogElement>('add-dialog');
  const url = $<HTMLInputElement>('add-url');
  if (state.settings) {
    $<HTMLInputElement>('add-connections').value = String(state.settings.connections);
  }
  dialog.showModal();
  // Offer whatever link is on the clipboard, the way IDM does. It needs
  // permission and silently does nothing when denied, which is fine.
  navigator.clipboard
    ?.readText()
    .then((text) => {
      if (!url.value && /^(https?|ftps?):\/\/\S+$/i.test(text.trim())) {
        url.value = text.trim();
      }
      url.focus();
    })
    .catch(() => url.focus());
}

function submitAdd(): void {
  const value = (id: string) => $<HTMLInputElement>(id).value.trim();
  const url = value('add-url');
  if (!url) return;

  const limitText = value('add-limit');
  const limit = parseRate(limitText);
  if (limit === null) {
    reportError(new Error(`"${limitText}" is not a speed; try 500k or 2M`));
    return;
  }

  const request: Record<string, unknown> = {
    url,
    connections: Number($<HTMLInputElement>('add-connections').value) || undefined,
    autostart: !$<HTMLInputElement>('add-paused').checked,
  };
  if (value('add-filename')) request.filename = value('add-filename');
  if (value('add-directory')) request.directory = value('add-directory');
  if (limit > 0) request.speedLimit = limit;
  if (value('add-username')) request.username = value('add-username');
  if (value('add-password')) request.password = value('add-password');
  if (value('add-referer')) request.referer = value('add-referer');

  const checksum = value('add-checksum');
  if (checksum) {
    const [maybeAlgo, maybeDigest] = checksum.split(':');
    if (maybeDigest) {
      request.checksumAlgo = maybeAlgo;
      request.checksum = maybeDigest;
    } else {
      request.checksum = checksum;
    }
  }

  api
    .addDownload(request)
    .then(() => {
      $<HTMLDialogElement>('add-dialog').close();
      $<HTMLFormElement>('add-form').reset();
      toast(t('toast.added'));
    })
    .catch(reportError);
}

function openSettingsDialog(): void {
  const settings = state.settings;
  if (settings) {
    $<HTMLSelectElement>('settings-language').value = locale();
    $<HTMLInputElement>('settings-connections').value = String(settings.connections);
    $<HTMLInputElement>('settings-concurrent').value = String(settings.maxConcurrent);
    $<HTMLInputElement>('settings-limit').value =
      settings.speedLimit > 0 ? String(settings.speedLimit) : '';
    $<HTMLInputElement>('settings-notifications').checked = settings.notifications;
  }
  $<HTMLDialogElement>('settings-dialog').showModal();
}

function saveSettings(): void {
  if (!state.settings) return;
  const limitText = $<HTMLInputElement>('settings-limit').value;
  const limit = parseRate(limitText);
  if (limit === null) {
    reportError(new Error(`"${limitText}" is not a speed; try 500k or 2M`));
    return;
  }

  const language = $<HTMLSelectElement>('settings-language').value as Locale;
  const updated: Settings = {
    ...state.settings,
    speedLimit: limit,
    connections: Number($<HTMLInputElement>('settings-connections').value) || 8,
    maxConcurrent: Number($<HTMLInputElement>('settings-concurrent').value) || 4,
    notifications: $<HTMLInputElement>('settings-notifications').checked,
    language,
    theme: currentTheme(),
  };

  api
    .saveSettings(updated)
    .then((saved) => {
      state.settings = saved;
      setLocale(language);
      try {
        localStorage.setItem('hydra.language', language);
      } catch {
        /* storage unavailable */
      }
      redrawAll();
      $<HTMLDialogElement>('settings-dialog').close();
      toast(t('settings.saved'));
    })
    .catch(reportError);
}

// ------------------------------------------------------------- appearance

function buildThemePicker(): void {
  const grid = $('theme-grid');
  grid.replaceChildren(
    ...THEMES.map((theme) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'theme-swatch';
      // The preview paints itself from the theme's own variables, so each
      // swatch is a genuine sample rather than a hand-kept approximation.
      button.dataset.theme = theme.id;
      button.setAttribute('aria-pressed', String(currentTheme() === theme.id));
      button.innerHTML = `<span class="preview"><i></i><i></i><i></i></span><span class="label"></span>`;
      button.querySelector('.label')!.textContent = theme.name;
      button.addEventListener('click', () => {
        applyTheme(theme.id);
        buildThemePicker();
      });
      return button;
    }),
  );
}

function buildAccentPicker(): void {
  const row = $('accent-row');
  row.replaceChildren(
    ...ACCENTS.map((accent) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'accent-dot';
      button.style.background = accent.colour || 'var(--accent)';
      button.title = accent.id;
      button.setAttribute('aria-label', accent.id);
      button.setAttribute('aria-pressed', String(currentAccent() === accent.id));
      button.addEventListener('click', () => {
        applyAccent(accent.id);
        buildAccentPicker();
      });
      return button;
    }),
  );
}

// ---------------------------------------------------------------- feedback

function toast(message: string, isError = false): void {
  const element = document.createElement('div');
  element.className = `toast${isError ? ' error' : ''}`;
  element.textContent = message;
  $('toasts').append(element);
  window.setTimeout(() => element.remove(), isError ? 7000 : 3500);
}

function reportError(error: unknown): void {
  const message =
    error instanceof ApiError
      ? error.message
      : error instanceof Error
        ? error.message
        : String(error);
  toast(message, true);
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}
