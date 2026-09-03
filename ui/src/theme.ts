/** Theme and accent selection. */

export interface ThemeInfo {
  id: string;
  name: string;
  dark: boolean;
}

/** Kept in step with styles/themes.css. */
export const THEMES: ThemeInfo[] = [
  { id: 'hydra-dark', name: 'Hydra Dark', dark: true },
  { id: 'hydra-light', name: 'Hydra Light', dark: false },
  { id: 'amoled', name: 'AMOLED', dark: true },
  { id: 'nord', name: 'Nord', dark: true },
  { id: 'dracula', name: 'Dracula', dark: true },
  { id: 'catppuccin-mocha', name: 'Catppuccin Mocha', dark: true },
  { id: 'catppuccin-latte', name: 'Catppuccin Latte', dark: false },
  { id: 'gruvbox-dark', name: 'Gruvbox', dark: true },
  { id: 'tokyo-night', name: 'Tokyo Night', dark: true },
  { id: 'solarized-dark', name: 'Solarized Dark', dark: true },
  { id: 'solarized-light', name: 'Solarized Light', dark: false },
  { id: 'rose-pine', name: 'Rosé Pine', dark: true },
];

export const ACCENTS = [
  { id: 'default', colour: '' },
  { id: 'blue', colour: '#4c8dff' },
  { id: 'violet', colour: '#a78bfa' },
  { id: 'pink', colour: '#f472b6' },
  { id: 'red', colour: '#f87171' },
  { id: 'orange', colour: '#fb923c' },
  { id: 'amber', colour: '#fbbf24' },
  { id: 'green', colour: '#4ade80' },
  { id: 'teal', colour: '#2dd4bf' },
  { id: 'cyan', colour: '#22d3ee' },
];

const THEME_KEY = 'hydra.theme';
const ACCENT_KEY = 'hydra.accent';

export function applyTheme(id: string): void {
  const theme = THEMES.find((t) => t.id === id) ?? THEMES[0]!;
  document.body.dataset.theme = theme.id;
  try {
    localStorage.setItem(THEME_KEY, theme.id);
  } catch {
    // Private browsing, or site data blocked. The theme still applies for
    // this session; only remembering it is lost.
  }
}

export function currentTheme(): string {
  return document.body.dataset.theme ?? 'hydra-dark';
}

export function applyAccent(id: string): void {
  const accent = ACCENTS.find((a) => a.id === id) ?? ACCENTS[0]!;
  if (accent.colour) {
    document.body.dataset.accent = accent.id;
    document.body.style.setProperty('--accent-user', accent.colour);
  } else {
    delete document.body.dataset.accent;
    document.body.style.removeProperty('--accent-user');
  }
  try {
    localStorage.setItem(ACCENT_KEY, accent.id);
  } catch {
    /* see applyTheme */
  }
}

export function currentAccent(): string {
  return document.body.dataset.accent ?? 'default';
}

/**
 * Restores the remembered look, falling back to the system's light/dark
 * preference so a first run already matches the desktop.
 */
export function restoreAppearance(): void {
  let theme: string | null = null;
  let accent: string | null = null;
  try {
    theme = localStorage.getItem(THEME_KEY);
    accent = localStorage.getItem(ACCENT_KEY);
  } catch {
    /* storage unavailable */
  }
  if (!theme) {
    const prefersLight = window.matchMedia?.('(prefers-color-scheme: light)').matches;
    theme = prefersLight ? 'hydra-light' : 'hydra-dark';
  }
  applyTheme(theme);
  if (accent) applyAccent(accent);
}
