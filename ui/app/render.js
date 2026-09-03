/**
 * Rendering the download list.
 *
 * Rows are reconciled by id and updated in place rather than rebuilt, so a
 * snapshot arriving twice a second does not destroy hover state, keyboard
 * focus, or the text selection someone is in the middle of making.
 */
import { bytes, duration, extension, percent, rate } from './format.js';
import { locale, t } from './i18n.js';
const ICONS = {
    pause: '<svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M6 5h4v14H6zM14 5h4v14h-4z"/></svg>',
    play: '<svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z"/></svg>',
    folder: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>',
    remove: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 6l12 12M18 6L6 18"/></svg>',
    retry: '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M3 12a9 9 0 1 0 3-6.7M3 4v5h5"/></svg>',
};
const rows = new Map();
export function renderList(container, downloads, callbacks) {
    if (downloads.length === 0) {
        rows.clear();
        container.replaceChildren(emptyState(container.dataset.filtered === 'true'));
        return;
    }
    let list = container.querySelector('.download-list');
    if (!list) {
        rows.clear();
        list = document.createElement('div');
        list.className = 'download-list';
        container.replaceChildren(list);
    }
    const seen = new Set();
    let previous = null;
    for (const download of downloads) {
        seen.add(download.id);
        let row = rows.get(download.id);
        if (!row) {
            row = createRow(download, callbacks);
            rows.set(download.id, row);
        }
        updateRow(row, download);
        // Keep DOM order in step with the data without rebuilding.
        const expected = previous
            ? previous.nextElementSibling
            : list.firstElementChild;
        if (expected !== row) {
            list.insertBefore(row, expected);
        }
        previous = row;
    }
    for (const [id, row] of rows) {
        if (!seen.has(id)) {
            row.remove();
            rows.delete(id);
        }
    }
}
function createRow(download, callbacks) {
    const row = document.createElement('article');
    row.className = 'download';
    row.tabIndex = 0;
    row.dataset.id = download.id;
    row.innerHTML = `
    <div class="filetype"></div>
    <div class="body">
      <span class="name"></span>
      <div class="meta"></div>
      <div class="bars"></div>
    </div>
    <div class="actions"></div>`;
    const open = () => callbacks.onOpen(download.id);
    row.addEventListener('click', (event) => {
        if (!event.target.closest('button'))
            open();
    });
    row.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            open();
        }
    });
    return row;
}
function updateRow(row, download) {
    const id = download.id;
    const name = download.filename || download.spec.url;
    const lang = locale();
    const badge = row.querySelector('.filetype');
    const ext = extension(name);
    if (badge.textContent !== ext)
        badge.textContent = ext;
    const label = row.querySelector('.name');
    if (label.textContent !== name) {
        label.textContent = name;
        label.title = name;
    }
    // Meta line: status, progress, speed, time left.
    const meta = row.querySelector('.meta');
    const parts = [
        `<span class="status-pill ${download.status}">${t(`status.${download.status}`)}</span>`,
    ];
    if (download.total) {
        parts.push(`<span>${bytes(download.downloaded, lang)} ${t('unit.of')} ${bytes(download.total, lang)}</span>`, `<span>${percent(download.downloaded, download.total, lang)}</span>`);
    }
    else if (download.downloaded > 0) {
        parts.push(`<span>${bytes(download.downloaded, lang)}</span>`);
    }
    if (download.speed > 0)
        parts.push(`<span>${rate(download.speed, lang)}</span>`);
    if (download.status === 'downloading' && download.eta !== null) {
        parts.push(`<span>${duration(download.eta, lang)}</span>`);
    }
    if (download.status === 'failed' && download.error) {
        parts.push(`<span style="color: var(--danger)">${escapeHtml(download.error)}</span>`);
    }
    const metaHtml = parts.join('');
    if (meta.innerHTML !== metaHtml)
        meta.innerHTML = metaHtml;
    renderBars(row.querySelector('.bars'), download);
    renderActions(row.querySelector('.actions'), download, id);
}
/**
 * Draws progress: one bar per connection while transferring, a single bar
 * otherwise. Seeing the segments fill in is what makes multi-connection
 * downloading visible rather than a claim on a feature list.
 */
function renderBars(container, download) {
    const segments = download.segments ?? [];
    const useSegments = download.status === 'downloading' && segments.length > 1;
    if (useSegments) {
        let strip = container.querySelector('.segments');
        if (!strip || strip.children.length !== segments.length) {
            strip = document.createElement('div');
            strip.className = 'segments';
            strip.innerHTML = segments.map(() => '<div class="segment"><i></i></div>').join('');
            container.replaceChildren(strip);
        }
        segments.forEach((segment, index) => {
            const length = Math.max(1, segment.end - segment.start + 1);
            const fraction = Math.min(1, segment.done / length);
            const fill = strip.children[index]?.firstElementChild;
            if (fill)
                fill.style.inlineSize = `${(fraction * 100).toFixed(1)}%`;
        });
        return;
    }
    let bar = container.querySelector('.progress');
    if (!bar) {
        bar = document.createElement('div');
        bar.className = 'progress';
        bar.innerHTML = '<div class="fill"></div>';
        container.replaceChildren(bar);
    }
    const fraction = download.status === 'completed'
        ? 1
        : download.total
            ? Math.min(1, download.downloaded / download.total)
            : 0;
    const fill = bar.firstElementChild;
    fill.style.inlineSize = `${(fraction * 100).toFixed(1)}%`;
    bar.className = `progress ${progressClass(download.status)}`;
}
function progressClass(status) {
    if (status === 'completed')
        return 'done';
    if (status === 'failed' || status === 'cancelled')
        return 'failed';
    if (status === 'paused' || status === 'queued')
        return 'paused';
    return '';
}
function renderActions(container, download, id) {
    const buttons = [];
    if (download.status === 'downloading' || download.status === 'probing') {
        buttons.push({ action: 'pause', icon: ICONS.pause, label: t('action.pause') });
    }
    else if (download.status === 'paused' || download.status === 'queued') {
        buttons.push({ action: 'resume', icon: ICONS.play, label: t('action.resume') });
    }
    else if (download.status === 'failed' || download.status === 'cancelled') {
        buttons.push({ action: 'resume', icon: ICONS.retry, label: t('action.retry') });
    }
    if (download.status === 'completed') {
        buttons.push({ action: 'reveal', icon: ICONS.folder, label: t('action.openFolder') });
    }
    buttons.push({ action: 'remove', icon: ICONS.remove, label: t('action.remove'), danger: true });
    const signature = buttons.map((b) => b.action).join(',');
    if (container.dataset.signature === signature)
        return;
    container.dataset.signature = signature;
    container.replaceChildren(...buttons.map(({ action, icon, label, danger }) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = `btn ghost icon${danger ? ' danger' : ''}`;
        button.innerHTML = icon;
        button.title = label;
        button.setAttribute('aria-label', label);
        button.dataset.action = action;
        button.dataset.id = id;
        return button;
    }));
}
function emptyState(filtered) {
    const element = document.createElement('div');
    element.className = 'empty';
    element.innerHTML = `
    <svg width="46" height="46" viewBox="0 0 24 24" fill="none" stroke="currentColor"
         stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M4 20h16"/>
    </svg>
    <h2></h2>
    <p></p>`;
    element.querySelector('h2').textContent = t('empty.title');
    element.querySelector('p').textContent = filtered ? t('empty.filtered') : t('empty.body');
    return element;
}
export function escapeHtml(value) {
    return value
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;');
}
/** Discards cached rows, for a language or theme change that alters every row. */
export function invalidateRows() {
    rows.clear();
}
//# sourceMappingURL=render.js.map