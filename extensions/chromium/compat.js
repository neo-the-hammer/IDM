/**
 * One extension API across browsers.
 *
 * Firefox exposes the promise-based API as `browser` and keeps `chrome`
 * callback-based; Chromium exposes only `chrome`, promise-based since MV3.
 * Aliasing `chrome` to `browser` where the latter exists means the rest of the
 * extension can be written once, against promises, and behave identically in
 * both. Importing this module for its side effect is enough.
 */
if (typeof globalThis.browser !== 'undefined' && globalThis.browser?.runtime) {
  globalThis.chrome = globalThis.browser;
}
export {};
