/** Shapes returned by the daemon's REST API. */

export interface Segment {
  start: number;
  end: number;
  done: number;
}

export interface DownloadSpec {
  url: string;
  directory: string;
  filename: string | null;
  connections: number;
  speedLimit: number;
  checksum: string | null;
  checksumAlgo: string | null;
  proxy: string | null;
  mirrors: string[];
}

export type Status =
  | 'queued'
  | 'probing'
  | 'downloading'
  | 'verifying'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface Download {
  id: string;
  queue: string | null;
  spec: DownloadSpec;
  status: Status;
  filename: string;
  outputPath: string | null;
  total: number | null;
  downloaded: number;
  speed: number;
  eta: number | null;
  error: string | null;
  createdAt: number;
  completedAt: number | null;
  category: string | null;
  contentType: string | null;
  segments?: Segment[];
}

export interface Totals {
  speed: number;
  active: number;
  queued: number;
  paused: number;
  completed: number;
  failed: number;
  total: number;
  speedLimit: number;
}

export interface Schedule {
  enabled: boolean;
  /** Minutes since local midnight. */
  start: number;
  stop: number | null;
  /** Bitmask of weekdays; bit 0 is Sunday. */
  days: number;
}

export interface Queue {
  id: string;
  name: string;
  concurrency: number;
  speedLimit: number;
  schedule: Schedule;
  completion: { kind: string; command?: string };
  paused: boolean;
}

export interface Settings {
  speedLimit: number;
  connections: number;
  maxConcurrent: number;
  maxRetries: number;
  notifications: boolean;
  clipboardMonitor: boolean;
  language: string;
  theme: string;
  proxy: string | null;
  queues: Queue[];
  categories: {
    enabled: boolean;
    root: string;
    categories: { id: string; name: string; extensions: string[] }[];
  };
}

/** What the event stream pushes. */
export interface Snapshot {
  type: 'snapshot';
  downloads: Download[];
  totals: Totals;
}
