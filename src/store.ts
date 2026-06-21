// Global UI state. Server data (feeds, articles…) lives in React Query;
// this store holds view selection plus the appearance preferences the
// design's settings / tweaks controls drive.

import { create } from "zustand";
import i18n from "./i18n";
import * as api from "./api";
import type { ArticleQuery } from "./types";

export type Theme = "light" | "dark";
/** Background depth for the dark theme. Only meaningful while `theme` is
 *  "dark"; lets users pick a darker paper than the default warm charcoal. */
export type DarkShade = "default" | "dimmer" | "black";
export type Accent = "clay" | "pine" | "indigo" | "ink";
export type Density = "compact" | "cozy" | "spacious";
export type ViewMode = "list" | "card";
export type StartupView = "all" | "unread" | "starred" | "last";
export type ReaderFont = "serif" | "sans" | "hyperlegible";

/** Reader title/body typeface options. `stack` feeds the `--reader-font` CSS
 *  variable; `adjust` nudges the body font-size — sans and Hyperlegible read
 *  visually larger than the serif at the same pixel size, so they shrink a
 *  touch to keep the optical size even across choices. */
export const READER_FONTS: Record<ReaderFont, { stack: string; adjust: string }> = {
  serif: { stack: "var(--serif)", adjust: "0px" },
  sans: { stack: "var(--ui)", adjust: "-1.5px" },
  hyperlegible: { stack: "'Atkinson Hyperlegible', var(--ui)", adjust: "-1.5px" },
};

/** Valid ranges for the reader appearance sliders — the single source of
 *  truth shared by the Settings sliders, persistence validation, and the
 *  `setReader` write guard, so all three stay in lockstep. */
export const READER_BOUNDS = {
  size: { min: 14, max: 22 },
  leading: { min: 130, max: 200 },
  width: { min: 520, max: 840 },
} as const;

/** Clamp `n` into `[min, max]`. */
function clamp(n: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, n));
}

/** Behavioural preferences driven by the Settings panel. */
export interface Prefs {
  showSidebarCounts: boolean;
  showCardThumbs: boolean;
  reduceMotion: boolean;
  showReadingTime: boolean;
  markReadOnOpen: boolean;
  markReadOnScroll: boolean;
  autoExtract: boolean;
  startupView: StartupView;
  hideReadOnStartup: boolean;
  /** Sidebar "unread only" mode — hide feeds with no unread articles. */
  sidebarUnreadOnly: boolean;
}

const ls = {
  get: (k: string, fallback: string) => localStorage.getItem(k) ?? fallback,
  /** A persisted enum value, validated against `allowed` — a corrupt or
   *  stale value falls back instead of flowing through an unchecked cast. */
  oneOf: <T extends string>(k: string, allowed: readonly T[], fallback: T): T => {
    const v = localStorage.getItem(k);
    return v != null && (allowed as readonly string[]).includes(v)
      ? (v as T)
      : fallback;
  },
  /** A persisted number, clamped to `[min, max]`. localStorage is
   *  webview-writable and may hold a corrupt non-numeric value (NaN would
   *  reach a CSS variable like `--reader-size: NaNpx` and break the layout)
   *  or a stale out-of-range value from an older build with different
   *  slider limits — both would distort the reader. NaN falls back; an
   *  in-band-but-out-of-range value is clamped into range. */
  num: (k: string, fallback: number, min: number, max: number) => {
    const v = localStorage.getItem(k);
    if (v == null) return fallback;
    const n = Number(v);
    if (!Number.isFinite(n)) return fallback;
    return clamp(n, min, max);
  },
  bool: (k: string, fallback: boolean) => {
    const v = localStorage.getItem(k);
    return v == null ? fallback : v === "1";
  },
  set: (k: string, v: string | number | boolean) =>
    localStorage.setItem(k, typeof v === "boolean" ? (v ? "1" : "0") : String(v)),
};

interface UiState {
  /** The active sidebar selection driving the article list. */
  query: ArticleQuery;
  /** Human-readable label of the current selection (list header). */
  queryLabel: string;
  /** Currently open article, or null. */
  selectedArticleId: number | null;
  /** Hide already-read articles in the list. */
  unreadOnly: boolean;
  /** Sort the list oldest-first instead of newest-first. */
  sortOldest: boolean;

  // appearance preferences
  theme: Theme;
  darkShade: DarkShade;
  accent: Accent;
  density: Density;
  viewMode: ViewMode;
  readerFont: ReaderFont;
  readerSize: number;
  readerLeading: number;
  readerWidth: number;

  // behavioural preferences
  prefs: Prefs;

  // transient view modes
  focusMode: boolean;
  aiOpen: boolean;

  select: (query: ArticleQuery, label: string) => void;
  openArticle: (id: number | null) => void;
  toggleUnreadOnly: () => void;
  toggleSort: () => void;

  setTheme: (t: Theme) => void;
  setDarkShade: (s: DarkShade) => void;
  setAccent: (a: Accent) => void;
  setDensity: (d: Density) => void;
  setViewMode: (v: ViewMode) => void;
  setReaderFont: (v: ReaderFont) => void;
  setReader: (p: Partial<Pick<UiState, "readerSize" | "readerLeading" | "readerWidth">>) => void;

  setPref: (patch: Partial<Prefs>) => void;

  setFocusMode: (v: boolean) => void;
  setAiOpen: (v: boolean) => void;
}

const PREF_KEYS: (keyof Prefs)[] = [
  "showSidebarCounts",
  "showCardThumbs",
  "reduceMotion",
  "showReadingTime",
  "markReadOnOpen",
  "markReadOnScroll",
  "autoExtract",
  "startupView",
  "hideReadOnStartup",
  "sidebarUnreadOnly",
];

/** Mirror the active theme into the backend settings table so the Rust side
 *  can paint the native window in the matching colour *before* the webview
 *  loads on the next launch — without this, a dark-theme user sees a brief
 *  light flash at window-create time (the `tauri.conf.json` background is a
 *  fixed light colour the backend has no other way to override). Mirrors the
 *  way `i18n.ts` persists the language for backend-localised text. */
function mirrorTheme(theme: Theme): void {
  api.setSetting("theme", theme).catch(() => {});
}

/** Mirror the dark-shade choice to the backend so the native window can be
 *  painted in the matching colour before the first webview frame (lib.rs). */
function mirrorDarkShade(shade: DarkShade): void {
  api.setSetting("dark_shade", shade).catch(() => {});
}

/** Resolve the persisted reader font, migrating the pre-0.2 boolean
 *  `useSerif` toggle (serif on/off) to the named-typeface preference. */
function loadReaderFont(): ReaderFont {
  const v = localStorage.getItem("readerFont");
  if (v === "serif" || v === "sans" || v === "hyperlegible") return v;
  return localStorage.getItem("useSerif") === "0" ? "sans" : "serif";
}

function loadPrefs(): Prefs {
  return {
    showSidebarCounts: ls.bool("pref.showSidebarCounts", true),
    showCardThumbs: ls.bool("pref.showCardThumbs", true),
    reduceMotion: ls.bool("pref.reduceMotion", false),
    showReadingTime: ls.bool("pref.showReadingTime", true),
    markReadOnOpen: ls.bool("pref.markReadOnOpen", true),
    markReadOnScroll: ls.bool("pref.markReadOnScroll", false),
    autoExtract: ls.bool("pref.autoExtract", false),
    startupView: ls.oneOf<StartupView>(
      "pref.startupView",
      ["all", "unread", "starred", "last"],
      "unread",
    ),
    hideReadOnStartup: ls.bool("pref.hideReadOnStartup", false),
    sidebarUnreadOnly: ls.bool("pref.sidebarUnreadOnly", false),
  };
}

export const useUi = create<UiState>((set) => ({
  query: { kind: "all" },
  queryLabel: i18n.t("smart.all"),
  selectedArticleId: null,
  unreadOnly: false,
  sortOldest: false,

  theme: ls.oneOf<Theme>("theme", ["light", "dark"], "light"),
  darkShade: ls.oneOf<DarkShade>(
    "darkShade",
    ["default", "dimmer", "black"],
    "default",
  ),
  accent: ls.oneOf<Accent>("accent", ["clay", "pine", "indigo", "ink"], "clay"),
  density: ls.oneOf<Density>(
    "density",
    ["compact", "cozy", "spacious"],
    "cozy",
  ),
  viewMode: ls.oneOf<ViewMode>("viewMode", ["list", "card"], "list"),
  readerFont: loadReaderFont(),
  readerSize: ls.num("readerSize", 17, READER_BOUNDS.size.min, READER_BOUNDS.size.max),
  readerLeading: ls.num(
    "readerLeading",
    165,
    READER_BOUNDS.leading.min,
    READER_BOUNDS.leading.max,
  ),
  readerWidth: ls.num("readerWidth", 680, READER_BOUNDS.width.min, READER_BOUNDS.width.max),

  prefs: loadPrefs(),

  focusMode: false,
  aiOpen: false,

  select: (query, label) => {
    // Remember the selection so the "open on startup: last view" preference
    // can restore it next launch.
    ls.set("lastView", JSON.stringify({ query, label }));
    set({ query, queryLabel: label, selectedArticleId: null });
  },
  openArticle: (id) => set({ selectedArticleId: id }),
  toggleUnreadOnly: () => set((s) => ({ unreadOnly: !s.unreadOnly })),
  toggleSort: () => set((s) => ({ sortOldest: !s.sortOldest })),

  setTheme: (theme) => { ls.set("theme", theme); mirrorTheme(theme); set({ theme }); },
  setDarkShade: (darkShade) => { ls.set("darkShade", darkShade); mirrorDarkShade(darkShade); set({ darkShade }); },
  setAccent: (accent) => { ls.set("accent", accent); set({ accent }); },
  setDensity: (density) => { ls.set("density", density); set({ density }); },
  setViewMode: (viewMode) => { ls.set("viewMode", viewMode); set({ viewMode }); },
  setReaderFont: (readerFont) => { ls.set("readerFont", readerFont); set({ readerFont }); },
  setReader: (p) => {
    // Clamp on write too: any caller (or a stale slider range) is kept from
    // pushing an out-of-range value into the persisted store or a CSS var.
    const next: Partial<Pick<UiState, "readerSize" | "readerLeading" | "readerWidth">> = {};
    if (p.readerSize != null) {
      next.readerSize = clamp(p.readerSize, READER_BOUNDS.size.min, READER_BOUNDS.size.max);
      ls.set("readerSize", next.readerSize);
    }
    if (p.readerLeading != null) {
      next.readerLeading = clamp(
        p.readerLeading,
        READER_BOUNDS.leading.min,
        READER_BOUNDS.leading.max,
      );
      ls.set("readerLeading", next.readerLeading);
    }
    if (p.readerWidth != null) {
      next.readerWidth = clamp(p.readerWidth, READER_BOUNDS.width.min, READER_BOUNDS.width.max);
      ls.set("readerWidth", next.readerWidth);
    }
    set(next);
  },

  setPref: (patch) => {
    for (const k of PREF_KEYS) {
      if (patch[k] !== undefined) ls.set(`pref.${k}`, patch[k] as string | boolean);
    }
    set((s) => ({ prefs: { ...s.prefs, ...patch } }));
  },

  setFocusMode: (focusMode) => set({ focusMode }),
  setAiOpen: (aiOpen) => set({ aiOpen }),
}));

// Seed the backend's theme copy on startup so an existing install — whose
// theme has lived only in localStorage until now — still gets the native
// launch background themed correctly from the next launch onward.
mirrorTheme(useUi.getState().theme);
mirrorDarkShade(useUi.getState().darkShade);
