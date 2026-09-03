/**
 * The daemon client: REST plus the WebSocket event stream.
 *
 * The page is served by the daemon itself, so the base URL is wherever this
 * page came from. The token is injected by the daemon into the served page, or
 * supplied in the query string when the UI is opened from another origin
 * during development.
 */

import type { Download, Queue, Settings, Snapshot, Totals } from './types.js';

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

export class Api {
  private readonly base: string;
  private readonly token: string;

  constructor(base?: string, token?: string) {
    this.base = (base ?? window.location.origin).replace(/\/$/, '');
    this.token = token ?? readToken();
  }

  private async call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await fetch(`${this.base}/api/v1${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });

    const text = await response.text();
    let parsed: unknown = null;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new ApiError(`the daemon sent a malformed reply`, response.status);
      }
    }
    if (!response.ok) {
      const message =
        (parsed as { error?: string } | null)?.error ?? `request failed (${response.status})`;
      throw new ApiError(message, response.status);
    }
    return parsed as T;
  }

  listDownloads(): Promise<{ downloads: Download[]; totals: Totals }> {
    return this.call('GET', '/downloads');
  }

  addDownload(request: Record<string, unknown>): Promise<Download> {
    return this.call('POST', '/downloads', request);
  }

  action(id: string, action: string): Promise<Download> {
    return this.call('POST', `/downloads/${encodeURIComponent(id)}/${action}`);
  }

  remove(id: string, deleteFiles = false): Promise<unknown> {
    const query = deleteFiles ? '?deleteFiles=true' : '';
    return this.call('DELETE', `/downloads/${encodeURIComponent(id)}${query}`);
  }

  pauseAll(): Promise<unknown> {
    return this.call('POST', '/downloads-pause-all');
  }

  resumeAll(): Promise<unknown> {
    return this.call('POST', '/downloads-resume-all');
  }

  clearCompleted(): Promise<{ removed: number }> {
    return this.call('POST', '/downloads-clear-completed');
  }

  queues(): Promise<{ queues: Queue[] }> {
    return this.call('GET', '/queues');
  }

  saveQueue(queue: Queue): Promise<{ queues: Queue[] }> {
    return this.call('PUT', `/queues/${encodeURIComponent(queue.id)}`, queue);
  }

  queueAction(id: string, action: 'pause' | 'resume'): Promise<{ queues: Queue[] }> {
    return this.call('POST', `/queues/${encodeURIComponent(id)}/${action}`);
  }

  setDownloadQueue(id: string, queue: string | null): Promise<Download> {
    return this.call('POST', `/downloads/${encodeURIComponent(id)}/queue`, { queue });
  }

  settings(): Promise<Settings> {
    return this.call('GET', '/settings');
  }

  saveSettings(settings: Settings): Promise<Settings> {
    return this.call('PUT', '/settings', settings);
  }

  /**
   * Subscribes to live updates.
   *
   * Reconnects with a backoff rather than giving up, so restarting the daemon
   * does not leave the page permanently stale.
   */
  subscribe(handlers: {
    onSnapshot: (snapshot: Snapshot) => void;
    onOpen?: () => void;
    onClose?: () => void;
  }): () => void {
    let socket: WebSocket | null = null;
    let closed = false;
    let attempt = 0;
    let timer: number | undefined;

    const connect = () => {
      if (closed) return;
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
          const payload = JSON.parse(event.data as string) as Snapshot;
          if (payload.type === 'snapshot') handlers.onSnapshot(payload);
        } catch {
          // A malformed frame is not worth tearing the connection down for.
        }
      });
      socket.addEventListener('close', () => {
        handlers.onClose?.();
        if (closed) return;
        attempt += 1;
        const delay = Math.min(500 * 2 ** Math.min(attempt, 5), 10_000);
        timer = window.setTimeout(connect, delay);
      });
      socket.addEventListener('error', () => socket?.close());
    };

    connect();
    return () => {
      closed = true;
      if (timer) window.clearTimeout(timer);
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
function readToken(): string {
  const fromQuery = new URLSearchParams(window.location.search).get('token');
  if (fromQuery) return fromQuery;
  const meta = document.querySelector<HTMLMetaElement>('meta[name="hydra-token"]');
  return meta?.content ?? '';
}
