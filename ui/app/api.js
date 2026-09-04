/**
 * The daemon client: REST plus the WebSocket event stream.
 *
 * The page is served by the daemon itself, so the base URL is wherever this
 * page came from. The token is injected by the daemon into the served page, or
 * supplied in the query string when the UI is opened from another origin
 * during development.
 */
export class ApiError extends Error {
    status;
    constructor(message, status) {
        super(message);
        this.status = status;
        this.name = 'ApiError';
    }
}
export class Api {
    base;
    token;
    constructor(base, token) {
        this.base = (base ?? window.location.origin).replace(/\/$/, '');
        this.token = token ?? readToken();
    }
    async call(method, path, body) {
        const response = await fetch(`${this.base}/api/v1${path}`, {
            method,
            headers: {
                Authorization: `Bearer ${this.token}`,
                ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
            },
            body: body === undefined ? undefined : JSON.stringify(body),
        });
        const text = await response.text();
        let parsed = null;
        if (text) {
            try {
                parsed = JSON.parse(text);
            }
            catch {
                throw new ApiError(`the daemon sent a malformed reply`, response.status);
            }
        }
        if (!response.ok) {
            const message = parsed?.error ?? `request failed (${response.status})`;
            throw new ApiError(message, response.status);
        }
        return parsed;
    }
    listDownloads() {
        return this.call('GET', '/downloads');
    }
    addDownload(request) {
        return this.call('POST', '/downloads', request);
    }
    action(id, action) {
        return this.call('POST', `/downloads/${encodeURIComponent(id)}/${action}`);
    }
    remove(id, deleteFiles = false) {
        const query = deleteFiles ? '?deleteFiles=true' : '';
        return this.call('DELETE', `/downloads/${encodeURIComponent(id)}${query}`);
    }
    pauseAll() {
        return this.call('POST', '/downloads-pause-all');
    }
    resumeAll() {
        return this.call('POST', '/downloads-resume-all');
    }
    clearCompleted() {
        return this.call('POST', '/downloads-clear-completed');
    }
    queues() {
        return this.call('GET', '/queues');
    }
    saveQueue(queue) {
        return this.call('PUT', `/queues/${encodeURIComponent(queue.id)}`, queue);
    }
    queueAction(id, action) {
        return this.call('POST', `/queues/${encodeURIComponent(id)}/${action}`);
    }
    setDownloadQueue(id, queue) {
        return this.call('POST', `/downloads/${encodeURIComponent(id)}/queue`, { queue });
    }
    expandPattern(pattern) {
        return this.call('POST', '/expand', { pattern });
    }
    crawl(request) {
        return this.call('POST', '/crawl', request);
    }
    addBatch(request) {
        return this.call('POST', '/downloads-batch', request);
    }
    plugins() {
        return this.call('GET', '/plugins');
    }
    settings() {
        return this.call('GET', '/settings');
    }
    saveSettings(settings) {
        return this.call('PUT', '/settings', settings);
    }
    /**
     * Subscribes to live updates.
     *
     * Reconnects with a backoff rather than giving up, so restarting the daemon
     * does not leave the page permanently stale.
     */
    subscribe(handlers) {
        let socket = null;
        let closed = false;
        let attempt = 0;
        let timer;
        const connect = () => {
            if (closed)
                return;
            const scheme = this.base.startsWith('https') ? 'wss' : 'ws';
            const host = this.base.replace(/^https?:\/\//, '');
            // The token travels in the query string because a browser cannot set
            // headers on a WebSocket handshake.
            const url = `${scheme}://${host}/api/v1/events?token=${encodeURIComponent(this.token)}`;
            socket = new WebSocket(url);
            socket.addEventListener('open', () => {
                attempt = 0;
                handlers.onOpen?.();
            });
            socket.addEventListener('message', (event) => {
                try {
                    const payload = JSON.parse(event.data);
                    if (payload.type === 'snapshot')
                        handlers.onSnapshot(payload);
                }
                catch {
                    // A malformed frame is not worth tearing the connection down for.
                }
            });
            socket.addEventListener('close', () => {
                handlers.onClose?.();
                if (closed)
                    return;
                attempt += 1;
                const delay = Math.min(500 * 2 ** Math.min(attempt, 5), 10_000);
                timer = window.setTimeout(connect, delay);
            });
            socket.addEventListener('error', () => socket?.close());
        };
        connect();
        return () => {
            closed = true;
            if (timer)
                window.clearTimeout(timer);
            socket?.close();
        };
    }
}
/**
 * Finds the API token.
 *
 * The daemon stamps it into the served page. A `?token=` in the URL is the
 * development path, where the UI is opened from a separate dev server.
 */
function readToken() {
    const fromQuery = new URLSearchParams(window.location.search).get('token');
    if (fromQuery)
        return fromQuery;
    const meta = document.querySelector('meta[name="hydra-token"]');
    return meta?.content ?? '';
}
//# sourceMappingURL=api.js.map