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

/** A file the site grabber found. */
export interface FoundFile {
  url: string;
  filename: string;
  extension: string;
  /** The page it was linked from, which becomes the download's Referer. */
  foundOn: string;
  text: string;
}

export interface CrawlResult {
  files: FoundFile[];
  pagesVisited: number;
  errors: string[];
  /** True when a limit stopped the crawl before it ran out of links. */
  truncated: boolean;
}

export interface PluginStatus {
  available: boolean;
  error?: string;
  python?: string;
  packageRoot?: string;
  capabilities?: {
    version: string;
    ytdlp: { available: boolean; version?: string; reason?: string };
  };
}

/** One quality a streaming manifest offers. */
export interface MediaStream {
  id: string;
  /** The playlist this stream is downloaded from. */
  url: string;
  /** `video`, `audio` or `text`. */
  kind: string;
  /** A ready-made description such as `1080p · 4.2 Mbit/s`. */
  label: string;
  width: number | null;
  height: number | null;
  bandwidth: number | null;
  codecs: string;
  language: string;
  /** Zero when the segment list has not been fetched yet. */
  segments: number;
  encrypted: boolean;
}

/** What a manifest turned out to contain. */
export interface MediaProbe {
  url: string;
  /** `hls` or `dash`. */
  format: string;
  live: boolean;
  /** Seconds; zero when the manifest does not say. */
  duration: number;
  streams: MediaStream[];
  /** True when video and audio are separate and both are needed. */
  separateAudio: boolean;
  warnings: string[];
  /** Whether the daemon found ffmpeg, which combining and remuxing need. */
  ffmpeg: boolean;
}
