/** The extension's settings page. */

import './compat.js';
import { DEFAULT_SETTINGS, getConnection, isAvailable, loadSettings, saveSettings } from './hydra.js';

const $ = (id) => document.getElementById(id);
const LISTS = ['includedExtensions', 'excludedExtensions', 'excludedHosts'];
const NUMBERS = ['minimumSize', 'connections'];
const FLAGS = ['captureEnabled', 'sniffMedia'];
const TEXTS = ['manualUrl', 'manualToken'];

/** Turns "iso, zip" into ["iso", "zip"], tolerating spacing and stray dots. */
function parseList(value) {
  return value
    .split(',')
    .map((item) => item.trim().replace(/^\./, '').toLowerCase())
    .filter(Boolean);
}

async function showStatus() {
  const element = $('status');
  const link = await getConnection({ force: true });
  if (!link) {
    element.textContent =
      'Hydra was not found. Start the hdmd daemon, or fill in the pairing fields below.';
    return;
  }
  const reachable = await isAvailable();
  element.textContent = reachable
    ? `Connected to ${link.url} (${link.source === 'native' ? 'found automatically' : 'configured by hand'}).`
    : `Found ${link.url}, but it did not answer. Is Hydra still running?`;
}

async function load() {
  const settings = await loadSettings();
  for (const id of FLAGS) $(id).checked = settings[id];
  for (const id of NUMBERS) $(id).value = String(settings[id]);
  for (const id of LISTS) $(id).value = settings[id].join(', ');
  for (const id of TEXTS) $(id).value = settings[id] ?? '';
}

async function save() {
  const settings = { ...DEFAULT_SETTINGS };
  for (const id of FLAGS) settings[id] = $(id).checked;
  for (const id of NUMBERS) settings[id] = Math.max(0, Number($(id).value) || 0);
  for (const id of LISTS) settings[id] = parseList($(id).value);
  for (const id of TEXTS) settings[id] = $(id).value.trim();

  await saveSettings(settings);
  const saved = $('saved');
  saved.hidden = false;
  setTimeout(() => (saved.hidden = true), 1800);
  await showStatus();
}

$('save').addEventListener('click', save);
load();
showStatus();
