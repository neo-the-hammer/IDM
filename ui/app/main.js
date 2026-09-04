/** Bootstraps the interface and wires it to the daemon. */
import { Api, ApiError } from './api.js';
import { bytes, duration, parseRate, percent, rate, timestamp } from './format.js';
import { applyStaticStrings, locale, setLocale, t } from './i18n.js';
import { invalidateRows, renderList } from './render.js';
import { ACCENTS, applyAccent, applyTheme, currentAccent, currentTheme, restoreAppearance, THEMES } from './theme.js';
const state = {
    downloads: [],
    totals: null,
    settings: null,
    queues: [],
    filter: 'all',
    queueFilter: null,
    search: '',
    connected: false,
};
const api = new Api();
const $ = (id) => document.getElementById(id);
/** Downloads that were finished last time we looked, so a toast fires once. */
const alreadyFinished = new Set();
let firstSnapshot = true;
// ------------------------------------------------------------------ startup
function boot() {
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
        if (settings.theme)
            applyTheme(settings.theme);
        if (settings.language)
            setLocale(settings.language);
        state.queues = settings.queues ?? [];
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
function detectLocale() {
    const stored = (() => {
        try {
            return localStorage.getItem('hydra.language');
        }
        catch {
            return null;
        }
    })();
    if (stored === 'fa' || stored === 'en')
        return stored;
    return navigator.language.startsWith('fa') ? 'fa' : 'en';
}
// ------------------------------------------------------------------ updates
function applySnapshot(snapshot) {
    const previous = state.downloads;
    state.downloads = snapshot.downloads;
    state.totals = snapshot.totals;
    notifyNewlyFinished(previous, snapshot.downloads);
    renderMain();
    renderSidebar();
    renderStatusBar();
}
/** Announces downloads that finished since the last snapshot. */
function notifyNewlyFinished(previous, current) {
    if (firstSnapshot) {
        // Everything already complete on first load is history, not news.
        current.filter((d) => d.status === 'completed').forEach((d) => alreadyFinished.add(d.id));
        firstSnapshot = false;
        return;
    }
    const before = new Map(previous.map((d) => [d.id, d.status]));
    for (const download of current) {
        if (download.status === 'completed' &&
            before.get(download.id) !== 'completed' &&
            !alreadyFinished.has(download.id)) {
            alreadyFinished.add(download.id);
            toast(t('toast.finished', { name: download.filename || download.spec.url }));
        }
    }
}
function redrawAll() {
    invalidateRows();
    applyStaticStrings();
    renderSidebar();
    renderMain();
    renderStatusBar();
    buildThemePicker();
    buildAccentPicker();
}
// ------------------------------------------------------------------ filters
const FILTERS = [
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
function visibleDownloads() {
    const filter = FILTERS.find((f) => f.id === state.filter) ?? FILTERS[0];
    const needle = state.search.trim().toLowerCase();
    return state.downloads.filter((download) => {
        if (state.queueFilter !== null) {
            // An unassigned download belongs to the main queue.
            const queue = download.queue ?? 'main';
            if (queue !== state.queueFilter)
                return false;
        }
        else if (!filter.match(download)) {
            return false;
        }
        if (!needle)
            return true;
        return (download.filename.toLowerCase().includes(needle) ||
            download.spec.url.toLowerCase().includes(needle));
    });
}
// ------------------------------------------------------------------ rendering
function renderSidebar() {
    const sidebar = $('sidebar');
    const counts = new Map();
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
            state.queueFilter = null;
            renderSidebar();
            renderMain();
        });
        group.append(button);
    }
    sidebar.append(group);
    if (state.queues.length > 0) {
        sidebar.append(renderQueueGroup());
    }
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
/**
 * The queue list, with each queue's schedule and a pause toggle.
 *
 * Showing the window inline matters: a queue that is quietly waiting for 1am
 * looks identical to a broken one unless the interface says why.
 */
function renderQueueGroup() {
    const group = document.createElement('div');
    group.className = 'nav-group';
    const heading = document.createElement('h3');
    heading.textContent = t('nav.queues');
    group.append(heading);
    for (const queue of state.queues) {
        const count = state.downloads.filter((d) => (d.queue ?? 'main') === queue.id).length;
        const row = document.createElement('div');
        row.style.display = 'flex';
        row.style.alignItems = 'center';
        row.style.gap = '2px';
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'nav-item';
        button.setAttribute('aria-current', String(state.queueFilter === queue.id));
        button.innerHTML =
            `<span>${escapeText(queue.name)}</span><span class="count">${count}</span>`;
        button.title = describeSchedule(queue);
        button.addEventListener('click', () => {
            state.queueFilter = state.queueFilter === queue.id ? null : queue.id;
            renderSidebar();
            renderMain();
        });
        const toggle = document.createElement('button');
        toggle.type = 'button';
        toggle.className = 'btn ghost icon';
        toggle.style.flex = 'none';
        toggle.title = queue.paused ? t('action.resume') : t('action.pause');
        toggle.innerHTML = queue.paused
            ? '<svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>'
            : '<svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>';
        toggle.addEventListener('click', (event) => {
            event.stopPropagation();
            api
                .queueAction(queue.id, queue.paused ? 'resume' : 'pause')
                .then(({ queues }) => {
                state.queues = queues;
                renderSidebar();
            })
                .catch(reportError);
        });
        row.append(button, toggle);
        group.append(row);
    }
    return group;
}
/** A one-line description of when a queue runs, for the tooltip. */
function describeSchedule(queue) {
    if (queue.paused)
        return t('queue.paused');
    if (!queue.schedule.enabled)
        return t('queue.running');
    const clock = (minutes) => {
        const h = String(Math.floor(minutes / 60)).padStart(2, '0');
        const m = String(minutes % 60).padStart(2, '0');
        return `${h}:${m}`;
    };
    return queue.schedule.stop === null
        ? t('queue.windowOpen', { from: clock(queue.schedule.start) })
        : t('queue.window', { from: clock(queue.schedule.start), to: clock(queue.schedule.stop) });
}
function escapeText(value) {
    return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}
function renderMain() {
    const main = $('main');
    const downloads = visibleDownloads();
    main.dataset.filtered = String(state.filter !== 'all' || state.queueFilter !== null || state.search.trim() !== '');
    renderList(main, downloads, {
        onAction: performAction,
        onOpen: openDetail,
    });
}
function renderStatusBar() {
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
function performAction(id, action) {
    const request = action === 'remove' ? api.remove(id) : api.action(id, action === 'resume' ? 'resume' : action);
    request
        .then(() => {
        if (action === 'remove')
            toast(t('toast.removed'));
    })
        .catch(reportError);
}
function openDetail(id) {
    const download = state.downloads.find((d) => d.id === id);
    if (!download)
        return;
    const lang = locale();
    const dialog = $('detail-dialog');
    $('detail-title').textContent = download.filename || download.spec.url;
    const rows = [
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
    if (download.outputPath)
        rows.push([t('detail.savedTo'), download.outputPath, true]);
    if (download.completedAt)
        rows.push([t('detail.finished'), timestamp(download.completedAt, lang)]);
    if (download.segments?.length) {
        rows.push([t('detail.segments'), String(download.segments.length)]);
    }
    if (download.error)
        rows.push([t('detail.error'), download.error]);
    const list = document.createElement('dl');
    for (const [term, value, ltr] of rows) {
        const dt = document.createElement('dt');
        dt.textContent = term;
        const dd = document.createElement('dd');
        dd.textContent = value;
        if (ltr)
            dd.className = 'ltr';
        list.append(dt, dd);
    }
    $('detail-body').replaceChildren(list);
    dialog.showModal();
}
// ------------------------------------------------------------------ chrome
function wireChrome() {
    // Row action buttons are delegated, so rows stay cheap to build.
    $('main').addEventListener('click', (event) => {
        const button = event.target.closest('button[data-action]');
        if (!button?.dataset.id)
            return;
        event.stopPropagation();
        performAction(button.dataset.id, button.dataset.action);
    });
    $('search').addEventListener('input', (event) => {
        state.search = event.target.value;
        renderMain();
    });
    $('add-button').addEventListener('click', () => openAddDialog());
    $('grab-button').addEventListener('click', () => openGrabDialog());
    wireGrabDialog();
    $('media-button').addEventListener('click', () => openMediaDialog());
    wireMediaDialog();
    $('settings-button').addEventListener('click', () => openSettingsDialog());
    $('pause-all').addEventListener('click', () => api.pauseAll().catch(reportError));
    $('resume-all').addEventListener('click', () => api.resumeAll().catch(reportError));
    $('settings-save').addEventListener('click', saveSettings);
    document.querySelectorAll('[data-close]').forEach((button) => {
        button.addEventListener('click', () => button.closest('dialog')?.close());
    });
    $('add-form').addEventListener('submit', (event) => {
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
            $('search').focus();
        }
    });
}
function openAddDialog() {
    const dialog = $('add-dialog');
    const url = $('add-url');
    if (state.settings) {
        $('add-connections').value = String(state.settings.connections);
    }
    const queueSelect = $('add-queue');
    queueSelect.replaceChildren(...state.queues.map((queue) => {
        const option = document.createElement('option');
        option.value = queue.id;
        option.textContent = queue.name;
        return option;
    }));
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
function submitAdd() {
    const value = (id) => $(id).value.trim();
    const url = value('add-url');
    if (!url)
        return;
    const limitText = value('add-limit');
    const limit = parseRate(limitText);
    if (limit === null) {
        reportError(new Error(`"${limitText}" is not a speed; try 500k or 2M`));
        return;
    }
    const request = {
        url,
        connections: Number($('add-connections').value) || undefined,
        autostart: !$('add-paused').checked,
    };
    if (value('add-filename'))
        request.filename = value('add-filename');
    if (value('add-directory'))
        request.directory = value('add-directory');
    if (limit > 0)
        request.speedLimit = limit;
    if (value('add-username'))
        request.username = value('add-username');
    if (value('add-password'))
        request.password = value('add-password');
    if (value('add-referer'))
        request.referer = value('add-referer');
    const queue = $('add-queue').value;
    if (queue)
        request.queue = queue;
    const checksum = value('add-checksum');
    if (checksum) {
        const [maybeAlgo, maybeDigest] = checksum.split(':');
        if (maybeDigest) {
            request.checksumAlgo = maybeAlgo;
            request.checksum = maybeDigest;
        }
        else {
            request.checksum = checksum;
        }
    }
    api
        .addDownload(request)
        .then(() => {
        $('add-dialog').close();
        $('add-form').reset();
        toast(t('toast.added'));
    })
        .catch(reportError);
}
function openSettingsDialog() {
    const settings = state.settings;
    if (settings) {
        $('settings-language').value = locale();
        $('settings-connections').value = String(settings.connections);
        $('settings-concurrent').value = String(settings.maxConcurrent);
        $('settings-limit').value =
            settings.speedLimit > 0 ? String(settings.speedLimit) : '';
        $('settings-notifications').checked = settings.notifications;
    }
    $('settings-dialog').showModal();
}
function saveSettings() {
    if (!state.settings)
        return;
    const limitText = $('settings-limit').value;
    const limit = parseRate(limitText);
    if (limit === null) {
        reportError(new Error(`"${limitText}" is not a speed; try 500k or 2M`));
        return;
    }
    const language = $('settings-language').value;
    const updated = {
        ...state.settings,
        speedLimit: limit,
        connections: Number($('settings-connections').value) || 8,
        maxConcurrent: Number($('settings-concurrent').value) || 4,
        notifications: $('settings-notifications').checked,
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
        }
        catch {
            /* storage unavailable */
        }
        redrawAll();
        $('settings-dialog').close();
        toast(t('settings.saved'));
    })
        .catch(reportError);
}
// ------------------------------------------------- batch and site grabber
/** What the last search turned up, and which of it is selected. */
let grabCandidates = [];
function openGrabDialog() {
    $('grab-results').hidden = true;
    $('grab-add').disabled = true;
    $('grab-status').textContent = '';
    grabCandidates = [];
    $('grab-dialog').showModal();
    $('batch-pattern').focus();
}
function wireGrabDialog() {
    const selectTab = (batch) => {
        $('tab-batch').setAttribute('aria-selected', String(batch));
        $('tab-crawl').setAttribute('aria-selected', String(!batch));
        $('panel-batch').hidden = !batch;
        $('panel-crawl').hidden = batch;
        // The two modes find different things; keeping stale results would invite
        // adding the wrong list.
        $('grab-results').hidden = true;
        $('grab-add').disabled = true;
        $('grab-status').textContent = '';
    };
    $('tab-batch').addEventListener('click', () => selectTab(true));
    $('tab-crawl').addEventListener('click', () => selectTab(false));
    $('grab-preview').addEventListener('click', runGrabSearch);
    $('grab-all').addEventListener('click', () => setAllSelected(true));
    $('grab-none').addEventListener('click', () => setAllSelected(false));
    $('grab-add').addEventListener('click', addSelected);
    // Enter in either input runs the search, which is what a user expects.
    for (const id of ['batch-pattern', 'crawl-url']) {
        $(id).addEventListener('keydown', (event) => {
            if (event.key === 'Enter') {
                event.preventDefault();
                runGrabSearch();
            }
        });
    }
}
function batchMode() {
    return $('tab-batch').getAttribute('aria-selected') === 'true';
}
async function runGrabSearch() {
    const button = $('grab-preview');
    const status = $('grab-status');
    button.disabled = true;
    status.textContent = t('grab.searching');
    $('grab-results').hidden = true;
    try {
        if (batchMode()) {
            const pattern = $('batch-pattern').value.trim();
            if (!pattern)
                return;
            const { urls } = await api.expandPattern(pattern);
            grabCandidates = urls.map((url) => ({
                url,
                filename: url.split('/').pop() ?? url,
                extension: (url.split('.').pop() ?? '').toLowerCase(),
                foundOn: '',
                text: '',
            }));
            status.textContent = t('grab.found', { n: grabCandidates.length });
        }
        else {
            const url = $('crawl-url').value.trim();
            if (!url)
                return;
            const include = $('crawl-include')
                .value.split(',')
                .map((item) => item.trim())
                .filter(Boolean);
            const result = await api.crawl({
                url,
                depth: Number($('crawl-depth').value) || 0,
                include,
                respectRobots: $('crawl-robots').checked,
            });
            grabCandidates = result.files;
            status.textContent =
                t('grab.foundPages', { n: result.files.length, pages: result.pagesVisited }) +
                    (result.truncated ? ` (${t('grab.truncated')})` : '');
        }
        renderGrabResults();
    }
    catch (error) {
        grabCandidates = [];
        status.textContent = '';
        reportError(error);
    }
    finally {
        button.disabled = false;
    }
}
function renderGrabResults() {
    const list = $('grab-list');
    if (grabCandidates.length === 0) {
        $('grab-results').hidden = false;
        $('grab-count').textContent = t('grab.nothing');
        list.replaceChildren();
        $('grab-add').disabled = true;
        return;
    }
    list.replaceChildren(...grabCandidates.map((file, index) => {
        const row = document.createElement('li');
        const box = document.createElement('input');
        box.type = 'checkbox';
        box.checked = true;
        box.dataset.index = String(index);
        box.addEventListener('change', updateAddButton);
        const url = document.createElement('span');
        url.className = 'url';
        url.textContent = file.url;
        url.title = file.foundOn ? `${file.url}\n\nfound on ${file.foundOn}` : file.url;
        const ext = document.createElement('span');
        ext.className = 'ext';
        ext.textContent = file.extension;
        row.append(box, url, ext);
        return row;
    }));
    $('grab-results').hidden = false;
    $('grab-count').textContent = t('grab.found', { n: grabCandidates.length });
    updateAddButton();
}
function selectedIndices() {
    return [...$('grab-list').querySelectorAll('input:checked')].map((box) => Number(box.dataset.index));
}
function setAllSelected(checked) {
    $('grab-list')
        .querySelectorAll('input[type=checkbox]')
        .forEach((box) => (box.checked = checked));
    updateAddButton();
}
function updateAddButton() {
    const count = selectedIndices().length;
    const button = $('grab-add');
    button.disabled = count === 0;
    button.textContent = count > 0 ? `${t('grab.add')} (${count})` : t('grab.add');
}
async function addSelected() {
    const chosen = selectedIndices()
        .map((index) => grabCandidates[index])
        .filter((file) => file !== undefined);
    if (chosen.length === 0)
        return;
    const button = $('grab-add');
    button.disabled = true;
    try {
        // Each entry carries the page it was found on, which becomes its Referer --
        // many servers refuse a download without one.
        const { added } = await api.addBatch({
            urls: chosen.map((file) => ({ url: file.url, foundOn: file.foundOn || undefined })),
            autostart: true,
        });
        $('grab-dialog').close();
        toast(t('grab.added', { n: added }));
    }
    catch (error) {
        reportError(error);
    }
    finally {
        button.disabled = false;
    }
}
// ------------------------------------------------------------- appearance
function buildThemePicker() {
    const grid = $('theme-grid');
    grid.replaceChildren(...THEMES.map((theme) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'theme-swatch';
        // The preview paints itself from the theme's own variables, so each
        // swatch is a genuine sample rather than a hand-kept approximation.
        button.dataset.theme = theme.id;
        button.setAttribute('aria-pressed', String(currentTheme() === theme.id));
        button.innerHTML = `<span class="preview"><i></i><i></i><i></i></span><span class="label"></span>`;
        button.querySelector('.label').textContent = theme.name;
        button.addEventListener('click', () => {
            applyTheme(theme.id);
            buildThemePicker();
        });
        return button;
    }));
}
function buildAccentPicker() {
    const row = $('accent-row');
    row.replaceChildren(...ACCENTS.map((accent) => {
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
    }));
}
// ------------------------------------------------- video and audio grabber
/** The manifest last examined, so the chosen stream can be looked up by id. */
let mediaProbe = null;
function openMediaDialog() {
    $('media-results').hidden = true;
    $('media-add').disabled = true;
    $('media-status').textContent = '';
    mediaProbe = null;
    $('media-dialog').showModal();
    $('media-url').focus();
}
function wireMediaDialog() {
    $('media-examine').addEventListener('click', examineMedia);
    $('media-add').addEventListener('click', addChosenStream);
    $('media-url').addEventListener('keydown', (event) => {
        if (event.key === 'Enter') {
            event.preventDefault();
            examineMedia();
        }
    });
}
async function examineMedia() {
    const url = $('media-url').value.trim();
    if (!url)
        return;
    const button = $('media-examine');
    const status = $('media-status');
    button.disabled = true;
    status.textContent = t('media.examining');
    $('media-results').hidden = true;
    try {
        mediaProbe = await api.probeMedia({ url });
        status.textContent = '';
        renderMediaStreams(mediaProbe);
    }
    catch (error) {
        mediaProbe = null;
        status.textContent = '';
        reportError(error);
    }
    finally {
        button.disabled = false;
    }
}
function renderMediaStreams(probe) {
    const list = $('media-streams');
    const summary = [probe.format.toUpperCase()];
    if (probe.duration > 0)
        summary.push(duration(Math.round(probe.duration)));
    if (probe.live)
        summary.push(t('media.live'));
    $('media-summary').textContent = summary.join(' · ');
    $('media-warnings').replaceChildren(...probe.warnings.map((warning) => {
        const item = document.createElement('li');
        item.textContent = warning;
        return item;
    }));
    // ffmpeg is what remuxing needs; offering the option without it would be a
    // promise the daemon cannot keep.
    const remux = $('media-remux');
    remux.disabled = !probe.ffmpeg;
    if (!probe.ffmpeg)
        remux.checked = false;
    $('media-ffmpeg-note').hidden = probe.ffmpeg;
    if (probe.streams.length === 0) {
        list.replaceChildren();
        $('media-results').hidden = false;
        $('media-summary').textContent = t('media.nothing');
        $('media-add').disabled = true;
        return;
    }
    const rows = [];
    let lastKind = '';
    probe.streams.forEach((stream, index) => {
        // Group video and audio under headings: on a DASH manifest the two lists
        // run together otherwise, and picking an audio track by mistake is easy.
        if (stream.kind !== lastKind) {
            lastKind = stream.kind;
            const heading = document.createElement('li');
            heading.className = 'group-heading';
            heading.textContent = stream.kind === 'audio' ? t('media.audio') : t('media.video');
            rows.push(heading);
        }
        const row = document.createElement('li');
        const radio = document.createElement('input');
        radio.type = 'radio';
        // Video and audio are chosen independently when the manifest keeps them
        // apart, so they are separate radio groups.
        radio.name = `media-${stream.kind}`;
        radio.value = String(index);
        radio.id = `media-stream-${index}`;
        // The first of each kind is the best of it, and the sensible default.
        radio.checked = probe.streams.findIndex((s) => s.kind === stream.kind) === index;
        const label = document.createElement('label');
        label.className = 'stream-label';
        label.htmlFor = radio.id;
        // With no resolution or bitrate to describe it, the daemon's label is just
        // the kind — which is an untranslated English word, so say it properly.
        label.textContent =
            stream.label === stream.kind && (stream.kind === 'video' || stream.kind === 'audio')
                ? t(`media.${stream.kind}`)
                : stream.label;
        const note = document.createElement('span');
        note.className = 'stream-note';
        const parts = [];
        if (stream.segments > 0)
            parts.push(t('media.segments', { n: stream.segments }));
        if (stream.encrypted)
            parts.push(t('media.encrypted'));
        note.textContent = parts.join(' · ');
        row.append(radio, label, note);
        rows.push(row);
    });
    list.replaceChildren(...rows);
    $('media-results').hidden = false;
    $('media-add').disabled = false;
}
function chosenStream(kind) {
    const selected = $('media-streams').querySelector(`input[name="media-${kind}"]:checked`);
    return selected ? mediaProbe?.streams[Number(selected.value)] : undefined;
}
async function addChosenStream() {
    const probe = mediaProbe;
    const video = chosenStream('video') ?? chosenStream('audio');
    if (!probe || !video)
        return;
    const request = {
        url: video.url,
        format: probe.format,
        streamId: video.id,
        remux: $('media-remux').checked,
    };
    // Only pair an audio track when the video genuinely lacks one; an HLS
    // variant already carries its audio, and adding a second track there would
    // duplicate it.
    if (probe.separateAudio && video.kind === 'video') {
        const audio = chosenStream('audio');
        if (audio) {
            request.audioUrl = audio.url;
            request.audioStreamId = audio.id;
        }
    }
    const button = $('media-add');
    button.disabled = true;
    try {
        const created = await api.addMedia(request);
        $('media-dialog').close();
        toast(t('media.added', { name: created.filename || video.label }));
    }
    catch (error) {
        reportError(error);
    }
    finally {
        button.disabled = false;
    }
}
// ---------------------------------------------------------------- feedback
function toast(message, isError = false) {
    const element = document.createElement('div');
    element.className = `toast${isError ? ' error' : ''}`;
    element.textContent = message;
    $('toasts').append(element);
    window.setTimeout(() => element.remove(), isError ? 7000 : 3500);
}
function reportError(error) {
    const message = error instanceof ApiError
        ? error.message
        : error instanceof Error
            ? error.message
            : String(error);
    toast(message, true);
}
if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
}
else {
    boot();
}
//# sourceMappingURL=main.js.map