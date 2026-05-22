import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cloneElement, isValidElement, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";
import * as api from "../api";
import { useUi, READER_BOUNDS } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { LANGUAGES, setLanguage, type Language } from "../i18n";
import { feedHost } from "../lib/feedMeta";
import { modKey, modCombo } from "../lib/platform";
import { reportError } from "../toast";
import { checkForUpdates } from "../lib/updater";
import { downloadFile } from "../lib/download";
import type { Feed, Rule, RuleAction, RuleField, RulePreview } from "../types";
import Icon, { type IconName } from "./Icon";
import ConfirmDialog from "./ConfirmDialog";
import FeedAvatar from "./FeedAvatar";

interface Props {
  onClose: () => void;
  onToast: (msg: string) => void;
  initialSection?: string;
  onAddFeed: () => void;
}

// `labelKey` holds an i18n key — resolved with t() at render time.
const SECTIONS: { id: string; labelKey: string; icon: IconName; color: string }[] = [
  { id: "general", labelKey: "settings.nav.general", icon: "settings", color: "#7a756c" },
  { id: "appearance", labelKey: "settings.nav.appearance", icon: "globe", color: "#bb6743" },
  { id: "reading", labelKey: "settings.nav.reading", icon: "eye", color: "#3a4cb8" },
  { id: "subscriptions", labelKey: "settings.nav.subscriptions", icon: "rss", color: "#d97706" },
  { id: "filters", labelKey: "settings.nav.filters", icon: "mute", color: "#9333ea" },
  { id: "sync", labelKey: "settings.nav.sync", icon: "refresh", color: "#2c8a3e" },
  { id: "shortcuts", labelKey: "settings.nav.shortcuts", icon: "command", color: "#5a5fc4" },
  { id: "notifications", labelKey: "settings.nav.notifications", icon: "inbox", color: "#a8501f" },
  { id: "advanced", labelKey: "settings.nav.advanced", icon: "sort", color: "#4a4a4a" },
  { id: "about", labelKey: "settings.nav.about", icon: "sparkle", color: "#111" },
];

/** The app version read from the Tauri bundle config at runtime, cached so the
 *  one IPC round-trip is shared between the sidebar footer and the About pane.
 *  Sourcing it live keeps the displayed version from drifting out of sync with
 *  `tauri.conf.json` the way a hardcoded string does on every release bump. */
let versionPromise: Promise<string> | null = null;
function useAppVersion(): string {
  const [version, setVersion] = useState("");
  useEffect(() => {
    versionPromise ??= getVersion().catch(() => "");
    let live = true;
    versionPromise.then((v) => {
      if (live) setVersion(v);
    });
    return () => {
      live = false;
    };
  }, []);
  return version;
}

export default function SettingsDialog({
  onClose,
  onToast,
  initialSection,
  onAddFeed,
}: Props) {
  const { t } = useTranslation();
  const [section, setSection] = useState(initialSection ?? "general");
  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const windowRef = useRef<HTMLDivElement>(null);
  const version = useAppVersion();
  useFocusTrap(windowRef);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const cur = SECTIONS.find((s) => s.id === section)!;
  const feedCount = feeds.data?.length ?? 0;

  const subs: Record<string, string> = {
    general: t("settings.sub.general"),
    appearance: t("settings.sub.appearance"),
    reading: t("settings.sub.reading"),
    subscriptions: t("settings.sub.subscriptions", { count: feedCount }),
    filters: t("settings.sub.filters"),
    sync: t("settings.sub.sync"),
    shortcuts: t("settings.sub.shortcuts"),
    notifications: t("settings.sub.notifications"),
    advanced: t("settings.sub.advanced"),
    about: t("settings.sub.about"),
  };

  return (
    <div className="settings-backdrop" onClick={onClose}>
      <div
        className="settings-window"
        ref={windowRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-sidebar">
          <div className="settings-sidebar-title">
            {t("settings.title")}
            <span className="badge">{modCombo(",")}</span>
          </div>
          {SECTIONS.map((s) => (
            <div
              key={s.id}
              className={`settings-nav-item ${section === s.id ? "active" : ""}`}
              onClick={() => setSection(s.id)}
            >
              <span className="nav-ico" style={{ background: s.color }}>
                <Icon name={s.icon} size={11} color="#fff" />
              </span>
              {t(s.labelKey)}
            </div>
          ))}
          <div className="settings-nav-spacer" />
          <div className="settings-version">
            Papr{version && ` ${version}`}
          </div>
        </div>

        <div className="settings-content">
          <div className="settings-header">
            <h2>{t(cur.labelKey)}</h2>
            <span className="sub">{subs[section]}</span>
          </div>
          <button
            className="settings-close"
            onClick={onClose}
            title={t("settings.closeTitle")}
          >
            <Icon name="x" size={15} />
          </button>

          <div className="settings-scroll">
            {section === "general" && <GeneralSection />}
            {section === "appearance" && <AppearanceSection />}
            {section === "reading" && <ReadingSection />}
            {section === "subscriptions" && (
              <SubscriptionsSection
                feeds={feeds.data ?? []}
                onToast={onToast}
                onAddFeed={onAddFeed}
              />
            )}
            {section === "filters" && (
              <FiltersSection feeds={feeds.data ?? []} onToast={onToast} />
            )}
            {section === "sync" && <SyncSection onToast={onToast} />}
            {section === "shortcuts" && <ShortcutsSection />}
            {section === "notifications" && <NotificationsSection />}
            {section === "advanced" && <AdvancedSection onToast={onToast} />}
            {section === "about" && <AboutSection />}
          </div>
        </div>
      </div>
    </div>
  );
}

/* ── row helpers ─────────────────────────────────────────── */
function Row({
  label,
  desc,
  children,
}: {
  label: string;
  desc?: string;
  children: React.ReactNode;
}) {
  // Name the row's control with the row label so screen readers don't just
  // announce a bare "checkbox" / "slider" / "combobox". The control
  // components forward the injected aria-label to their element.
  const control = isValidElement(children)
    ? cloneElement(children as React.ReactElement<{ "aria-label"?: string }>, {
        "aria-label": label,
      })
    : children;
  return (
    <div className="settings-row">
      <div className="settings-row-text">
        <div className="settings-row-label">{label}</div>
        {desc && <div className="settings-row-desc">{desc}</div>}
      </div>
      <div className="settings-row-control">{control}</div>
    </div>
  );
}

function Toggle({
  checked,
  onChange,
  "aria-label": ariaLabel,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  "aria-label"?: string;
}) {
  return (
    <input
      type="checkbox"
      className="s-toggle"
      checked={checked}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.checked)}
    />
  );
}

function Select<T extends string>({
  value,
  options,
  onChange,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  "aria-label"?: string;
}) {
  return (
    <select
      className="s-select"
      value={value}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.value as T)}
    >
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

function Segmented<T extends string>({
  value,
  options,
  onChange,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  "aria-label"?: string;
}) {
  return (
    <div className="s-seg" role="group" aria-label={ariaLabel}>
      {options.map((o) => (
        <button
          key={o.value}
          className={value === o.value ? "on" : ""}
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** The keys that actually move an `<input type="range">`. `onKeyUp` fires for
 *  every key release while the slider is focused — Tab (which merely lands or
 *  leaves focus), Shift, the modifier keys — so committing on a bare keyup
 *  would run `onCommit` for a key that never changed the value. For the
 *  network-timeout slider that side effect is a full HTTP-client rebuild, so a
 *  user simply Tab-navigating through Settings would trigger one. Restrict the
 *  commit to releases of a value-changing key. */
const SLIDER_KEYS = new Set([
  "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
  "Home", "End", "PageUp", "PageDown",
]);

function Slider({
  value,
  min,
  max,
  step = 1,
  unit = "",
  onChange,
  onCommit,
  "aria-label": ariaLabel,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  unit?: string;
  /** Fires on every drag tick — for cheap, live updates (e.g. reader preview). */
  onChange?: (v: number) => void;
  /** Fires once the drag/keypress settles — for costly side effects (a backend
   *  write, an HTTP-client rebuild) that must not run ~20× across one drag. */
  onCommit?: (v: number) => void;
  "aria-label"?: string;
}) {
  const [draft, setDraft] = useState(value);
  // Follow external changes (async settings load, reset) when not mid-drag.
  useEffect(() => setDraft(value), [value]);
  return (
    <>
      <input
        type="range"
        className="s-slider"
        min={min}
        max={max}
        step={step}
        value={draft}
        aria-label={ariaLabel}
        aria-valuetext={`${draft}${unit}`}
        onChange={(e) => {
          const v = Number(e.target.value);
          setDraft(v);
          onChange?.(v);
        }}
        onPointerUp={(e) =>
          onCommit?.(Number((e.target as HTMLInputElement).value))
        }
        onKeyUp={(e) => {
          // Only a key that can move the slider commits — a bare keyup from
          // Tab / Shift / a modifier never changed the value.
          if (SLIDER_KEYS.has(e.key)) {
            onCommit?.(Number((e.target as HTMLInputElement).value));
          }
        }}
      />
      <span className="s-value">
        {draft}
        {unit}
      </span>
    </>
  );
}

/** A toggle row backed by a persisted backend setting ("1" / "0"). */
function SettingFlag({
  settingKey,
  label,
  desc,
  fallback = false,
  onChanged,
}: {
  settingKey: string;
  label: string;
  desc?: string;
  fallback?: boolean;
  onChanged?: (v: boolean) => void;
}) {
  const [val, setVal] = useState(fallback);
  useEffect(() => {
    api
      .getSetting(settingKey)
      .then((v) => {
        if (v != null && v !== "") setVal(v === "1");
      })
      .catch(() => {});
  }, [settingKey]);
  const change = (v: boolean) => {
    setVal(v);
    api.setSetting(settingKey, v ? "1" : "0").catch(() => {});
    onChanged?.(v);
  };
  return (
    <Row label={label} desc={desc}>
      <Toggle checked={val} onChange={change} />
    </Row>
  );
}

/** Bytes → human-readable size. */
function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Open-at-login toggle, backed by the OS via the autostart plugin. */
function LaunchAtLogin() {
  const { t } = useTranslation();
  const [on, setOn] = useState(false);
  useEffect(() => {
    isEnabled().then(setOn).catch(() => {});
  }, []);
  const change = async (v: boolean) => {
    try {
      if (v) await enable();
      else await disable();
      setOn(v);
    } catch (e) {
      reportError(e);
    }
  };
  return (
    <Row
      label={t("settings.general.launchAtLogin")}
      desc={t("settings.general.launchAtLoginDesc")}
    >
      <Toggle checked={on} onChange={change} />
    </Row>
  );
}

/* ── general ─────────────────────────────────────────────── */
// Auto-refresh "off" is stored as a year-long interval — the only lever the
// backend scheduler exposes (it reads `refresh_interval_min`, minimum 5).
const OFF_INTERVAL = 525600;

// A persisted numeric setting, coerced into the range its `<Slider>` accepts.
// Settings live in the backend DB and are normally written numeric, but a
// stale value from an older build with different slider limits — or a corrupt
// non-numeric value — would otherwise flow straight into a `<Slider>`: an
// out-of-range value pins the thumb at the limit while the readout shows a
// contradicting number, and a NaN renders the value as a literal "NaN". This
// mirrors `store.ts`'s `ls.num`, which validates the localStorage-backed
// reader sliders for exactly the same reason.
function clampSetting(raw: string | null, fallback: number, min: number, max: number): number {
  if (raw == null || raw === "") return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function GeneralSection() {
  const { t } = useTranslation();
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [refreshMins, setRefreshMins] = useState(30);

  useEffect(() => {
    api
      .getSetting("refresh_interval_min")
      .then((v) => {
        const n = v ? Number(v) : 30;
        // A finite interval at/above the "off" sentinel means auto-refresh is
        // disabled; anything else is a live interval clamped to the slider's
        // 5–120 range (a stale larger value would otherwise show e.g. "150
        // minutes" with the thumb stuck at 120, and a NaN would read "NaN").
        if (Number.isFinite(n) && n >= 100000) setAutoRefresh(false);
        else {
          setAutoRefresh(true);
          setRefreshMins(clampSetting(v ?? null, 30, 5, 120));
        }
      })
      .catch(() => {});
  }, []);

  const writeInterval = (auto: boolean, mins: number) => {
    api
      .setSetting("refresh_interval_min", auto ? String(mins) : String(OFF_INTERVAL))
      .catch(() => {});
  };

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.refresh")}</h3>
        <Row
          label={t("settings.general.autoRefresh")}
          desc={t("settings.general.autoRefreshDesc")}
        >
          <Toggle
            checked={autoRefresh}
            onChange={(v) => {
              setAutoRefresh(v);
              writeInterval(v, refreshMins);
            }}
          />
        </Row>
        {autoRefresh && (
          <Row
            label={t("settings.general.refreshInterval")}
            desc={t("settings.general.refreshIntervalDesc")}
          >
            <Slider
              value={refreshMins}
              min={5}
              max={120}
              step={5}
              unit={t("settings.general.minutesUnit")}
              onChange={setRefreshMins}
              onCommit={(m) => writeInterval(true, m)}
            />
          </Row>
        )}
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.readBehavior")}</h3>
        <Row label={t("settings.general.markReadOnOpen")}>
          <Toggle
            checked={prefs.markReadOnOpen}
            onChange={(v) => setPref({ markReadOnOpen: v })}
          />
        </Row>
        <Row
          label={t("settings.general.markReadOnScroll")}
          desc={t("settings.general.markReadOnScrollDesc")}
        >
          <Toggle
            checked={prefs.markReadOnScroll}
            onChange={(v) => setPref({ markReadOnScroll: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.startup")}</h3>
        <LaunchAtLogin />
        <Row
          label={t("settings.general.startupView")}
          desc={t("settings.general.startupViewDesc")}
        >
          <Select
            value={prefs.startupView}
            options={[
              { value: "all", label: t("settings.general.startupAll") },
              { value: "unread", label: t("smart.unread") },
              { value: "starred", label: t("smart.starred") },
              { value: "last", label: t("settings.general.startupLast") },
            ]}
            onChange={(v) => setPref({ startupView: v })}
          />
        </Row>
        <Row label={t("settings.general.hideReadOnStartup")}>
          <Toggle
            checked={prefs.hideReadOnStartup}
            onChange={(v) => setPref({ hideReadOnStartup: v })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── appearance ──────────────────────────────────────────── */
function AppearanceSection() {
  const { t, i18n } = useTranslation();
  const theme = useUi((s) => s.theme);
  const setTheme = useUi((s) => s.setTheme);
  const accent = useUi((s) => s.accent);
  const setAccent = useUi((s) => s.setAccent);
  const density = useUi((s) => s.density);
  const setDensity = useUi((s) => s.setDensity);
  const viewMode = useUi((s) => s.viewMode);
  const setViewMode = useUi((s) => s.setViewMode);
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);

  const accents = [
    { value: "clay", color: "#bb6743", label: t("settings.appearance.accentClay") },
    { value: "pine", color: "#3d7a5e", label: t("settings.appearance.accentPine") },
    { value: "indigo", color: "#5a5fc4", label: t("settings.appearance.accentIndigo") },
    { value: "ink", color: "#2b2620", label: t("settings.appearance.accentInk") },
  ] as const;

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.language")}</h3>
        <Row
          label={t("settings.appearance.uiLanguage")}
          desc={t("settings.appearance.languageDesc")}
        >
          <Select
            value={i18n.language}
            options={LANGUAGES.map((l) => ({ value: l.code, label: l.label }))}
            onChange={(v) => setLanguage(v as Language)}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.theme")}</h3>
        <Row
          label={t("settings.appearance.appearance")}
          desc={t("settings.appearance.appearanceDesc")}
        >
          <Segmented
            value={theme}
            options={[
              { value: "light", label: t("settings.appearance.light") },
              { value: "dark", label: t("settings.appearance.dark") },
            ]}
            onChange={setTheme}
          />
        </Row>
        <Row
          label={t("settings.appearance.accent")}
          desc={t("settings.appearance.accentDesc")}
        >
          <div className="s-swatches" role="group">
            {accents.map((a) => (
              <button
                key={a.value}
                className={`s-swatch ${accent === a.value ? "on" : ""}`}
                style={{ background: a.color }}
                onClick={() => setAccent(a.value)}
                title={a.label}
                aria-label={a.label}
                aria-pressed={accent === a.value}
              />
            ))}
          </div>
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.layout")}</h3>
        <Row
          label={t("settings.appearance.density")}
          desc={t("settings.appearance.densityDesc")}
        >
          <Segmented
            value={density}
            options={[
              { value: "compact", label: t("settings.appearance.densityCompact") },
              { value: "cozy", label: t("settings.appearance.densityCozy") },
              { value: "spacious", label: t("settings.appearance.densitySpacious") },
            ]}
            onChange={setDensity}
          />
        </Row>
        <Row label={t("settings.appearance.listStyle")}>
          <Segmented
            value={viewMode}
            options={[
              { value: "list", label: t("settings.appearance.listStyleList") },
              { value: "card", label: t("settings.appearance.listStyleCard") },
            ]}
            onChange={setViewMode}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.details")}</h3>
        <Row label={t("settings.appearance.sidebarCounts")}>
          <Toggle
            checked={prefs.showSidebarCounts}
            onChange={(v) => setPref({ showSidebarCounts: v })}
          />
        </Row>
        <Row
          label={t("settings.appearance.cardThumbs")}
          desc={t("settings.appearance.cardThumbsDesc")}
        >
          <Toggle
            checked={prefs.showCardThumbs}
            onChange={(v) => setPref({ showCardThumbs: v })}
          />
        </Row>
        <Row
          label={t("settings.appearance.reduceMotion")}
          desc={t("settings.appearance.reduceMotionDesc")}
        >
          <Toggle
            checked={prefs.reduceMotion}
            onChange={(v) => setPref({ reduceMotion: v })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── reading ─────────────────────────────────────────────── */
function ReadingSection() {
  const { t } = useTranslation();
  const readerFont = useUi((s) => s.readerFont);
  const setReaderFont = useUi((s) => s.setReaderFont);
  const readerSize = useUi((s) => s.readerSize);
  const readerLeading = useUi((s) => s.readerLeading);
  const readerWidth = useUi((s) => s.readerWidth);
  const setReader = useUi((s) => s.setReader);
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);
  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.font")}</h3>
        <Row
          label={t("settings.reading.bodyFont")}
          desc={t("settings.reading.bodyFontDesc")}
        >
          <Segmented
            value={readerFont}
            options={[
              { value: "serif", label: t("settings.reading.serif") },
              { value: "sans", label: t("settings.reading.sans") },
              { value: "hyperlegible", label: t("settings.reading.hyperlegible") },
            ]}
            onChange={setReaderFont}
          />
        </Row>
        <Row label={t("settings.reading.fontSize")}>
          <Slider
            value={readerSize}
            min={READER_BOUNDS.size.min}
            max={READER_BOUNDS.size.max}
            unit="px"
            onChange={(v) => setReader({ readerSize: v })}
          />
        </Row>
        <Row label={t("settings.reading.lineHeight")}>
          <Slider
            value={readerLeading}
            min={READER_BOUNDS.leading.min}
            max={READER_BOUNDS.leading.max}
            step={5}
            unit="%"
            onChange={(v) => setReader({ readerLeading: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.layout")}</h3>
        <Row label={t("settings.reading.maxWidth")}>
          <Slider
            value={readerWidth}
            min={READER_BOUNDS.width.min}
            max={READER_BOUNDS.width.max}
            step={20}
            unit="px"
            onChange={(v) => setReader({ readerWidth: v })}
          />
        </Row>
        <Row
          label={t("settings.reading.readingTime")}
          desc={t("settings.reading.readingTimeDesc")}
        >
          <Toggle
            checked={prefs.showReadingTime}
            onChange={(v) => setPref({ showReadingTime: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.fulltext")}</h3>
        <Row
          label={t("settings.reading.autoExtract")}
          desc={t("settings.reading.autoExtractDesc")}
        >
          <Toggle
            checked={prefs.autoExtract}
            onChange={(v) => setPref({ autoExtract: v })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── subscriptions ───────────────────────────────────────── */
function SubscriptionsSection({
  feeds,
  onToast,
  onAddFeed,
}: {
  feeds: Feed[];
  onToast: (m: string) => void;
  onAddFeed: () => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const [search, setSearch] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);
  const filtered = feeds.filter(
    (f) => !search || f.title.toLowerCase().includes(search.toLowerCase()),
  );

  const exportOpml = async () => {
    try {
      const xml = await api.exportOpml();
      downloadFile(xml, "subscriptions.opml", "text/xml");
      onToast(t("settings.subscriptions.opmlExported"));
    } catch (e) {
      reportError(e);
    }
  };

  const importOpml = async (file: File) => {
    try {
      const n = await api.importOpml(await file.text());
      await qc.invalidateQueries();
      onToast(t("settings.subscriptions.opmlImported", { count: n }));
    } catch (e) {
      reportError(e);
    }
  };

  const unsubscribe = (f: Feed) =>
    api
      .deleteFeed(f.id)
      .then(() => {
        // Unsubscribing touches only article-bearing caches — unlike OPML
        // import, it needs no full invalidation.
        actions.refreshAfterBulk();
        onToast(t("settings.subscriptions.unsubscribed", { title: f.title }));
      })
      .catch((e) => reportError(e));

  return (
    <>
      <input
        ref={fileRef}
        type="file"
        accept=".opml,.xml"
        style={{ display: "none" }}
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) importOpml(f);
          e.target.value = "";
        }}
      />
      <div className="settings-group" style={{ marginBottom: 18 }}>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <div
            style={{
              flex: 1,
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "6px 10px",
              borderRadius: 7,
              border: "1px solid var(--hair-strong)",
              background: "var(--panel)",
            }}
          >
            <Icon name="search" size={13} color="var(--muted)" />
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t("settings.subscriptions.searchPlaceholder")}
              style={{
                flex: 1,
                border: 0,
                outline: 0,
                background: "transparent",
                fontFamily: "inherit",
                fontSize: 12.5,
                color: "var(--ink)",
              }}
            />
          </div>
          <button className="s-btn" onClick={() => fileRef.current?.click()}>
            <Icon name="arrow-down" size={12} /> {t("settings.subscriptions.importOpml")}
          </button>
          <button className="s-btn" onClick={exportOpml}>
            <Icon name="arrow-up" size={12} /> {t("settings.subscriptions.export")}
          </button>
          <button className="s-btn primary" onClick={onAddFeed}>
            <Icon name="plus" size={12} /> {t("common.add")}
          </button>
        </div>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">
          {t("settings.subscriptions.feedsCount", { count: filtered.length })}
        </h3>
        <div>
          {filtered.map((f) => (
            <div key={f.id} className="s-feed-row">
              <FeedAvatar
                title={f.title}
                faviconUrl={f.faviconUrl}
                seed={f.id}
                style={{ width: 22, height: 22, borderRadius: 5 }}
              />
              <span className="name">{f.title}</span>
              <span className="url">{feedHost(f)}</span>
              <div className="actions">
                <button
                  className="icon-btn"
                  title={t("settings.subscriptions.unsubscribe")}
                  onClick={() => unsubscribe(f)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            </div>
          ))}
          {filtered.length === 0 && (
            <div
              style={{ padding: "16px 4px", fontSize: 13, color: "var(--muted)" }}
            >
              {t("settings.subscriptions.noMatch")}
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/* ── sync ────────────────────────────────────────────────── */
function SyncSection({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const status = useQuery({
    queryKey: ["freshrss-status"],
    queryFn: api.freshrssStatus,
  });
  const [url, setUrl] = useState("");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [busy, setBusy] = useState(false);
  const connected = status.data?.connected ?? false;

  const connect = async () => {
    if (!url.trim() || !user.trim()) return;
    setBusy(true);
    try {
      await api.freshrssConnect(url.trim(), user.trim(), pass);
      await qc.invalidateQueries({ queryKey: ["freshrss-status"] });
      onToast(t("settings.sync.connected"));
      setPass("");
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    try {
      await api.freshrssDisconnect();
      await qc.invalidateQueries({ queryKey: ["freshrss-status"] });
      onToast(t("settings.sync.disconnected"));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const syncNow = async () => {
    setBusy(true);
    try {
      const n = await api.freshrssSync();
      // Sync reconciles read/starred state and may add feeds — refresh the
      // article-bearing caches, not unrelated ones (AI summaries, settings).
      actions.refreshAfterBulk();
      onToast(t("settings.sync.syncDone", { count: n }));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const unavailable = [
    { name: "Feedly", initial: "F", color: "#2BB24C", reason: t("settings.sync.reasonOauth") },
    { name: "Inoreader", initial: "I", color: "#1976D2", reason: t("settings.sync.reasonOauth") },
    { name: "iCloud", initial: "☁", color: "#0089E0", reason: t("settings.sync.reasonEntitlements") },
  ];

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.sync.freshrss")}</h3>
        {connected ? (
          <>
            <div className="s-service">
              <div className="logo" style={{ background: "#4A4A4A" }}>
                ⚡
              </div>
              <div className="info">
                <div className="title">FreshRSS</div>
                <div className="desc">{status.data?.url}</div>
              </div>
              <span className="status on">{t("settings.sync.statusConnected")}</span>
            </div>
            <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
              <button
                className="s-btn primary"
                onClick={syncNow}
                disabled={busy}
              >
                <Icon name="refresh" size={12} />{" "}
                {busy ? t("settings.sync.syncing") : t("settings.sync.syncNow")}
              </button>
              <button className="s-btn" onClick={disconnect} disabled={busy}>
                {t("settings.sync.disconnect")}
              </button>
            </div>
            <p
              style={{
                fontSize: 12,
                color: "var(--muted)",
                marginTop: 12,
                lineHeight: 1.5,
              }}
            >
              {t("settings.sync.syncHint")}
            </p>
          </>
        ) : (
          <>
            <p className="modal-hint" style={{ marginBottom: 14 }}>
              {t("settings.sync.connectHint")}
            </p>
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <input
                className="modal-input"
                style={{ margin: 0 }}
                placeholder={t("settings.sync.serverPlaceholder")}
                value={url}
                onChange={(e) => setUrl(e.target.value)}
              />
              <input
                className="modal-input"
                style={{ margin: 0 }}
                placeholder={t("settings.sync.userPlaceholder")}
                value={user}
                onChange={(e) => setUser(e.target.value)}
              />
              <input
                className="modal-input"
                style={{ margin: 0 }}
                type="password"
                placeholder={t("settings.sync.passPlaceholder")}
                value={pass}
                onChange={(e) => setPass(e.target.value)}
              />
              <div>
                <button
                  className="s-btn primary"
                  onClick={connect}
                  disabled={busy || !url.trim() || !user.trim()}
                >
                  {busy ? t("settings.sync.connecting") : t("settings.sync.connect")}
                </button>
              </div>
            </div>
          </>
        )}
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.sync.otherServices")}</h3>
        {unavailable.map((s) => (
          <div key={s.name} className="s-service" style={{ opacity: 0.6 }}>
            <div className="logo" style={{ background: s.color }}>
              {s.initial}
            </div>
            <div className="info">
              <div className="title">{s.name}</div>
              <div className="desc">{s.reason}</div>
            </div>
            <span className="status">{t("settings.sync.statusUnavailable")}</span>
          </div>
        ))}
      </div>
    </>
  );
}

/* ── shortcuts ───────────────────────────────────────────── */
function ShortcutsSection() {
  const { t } = useTranslation();
  const groups = [
    {
      title: t("settings.shortcuts.navigation"),
      items: [
        { desc: t("settings.shortcuts.nextArticle"), keys: ["J"] },
        { desc: t("settings.shortcuts.prevArticle"), keys: ["K"] },
        { desc: t("settings.shortcuts.openInBrowser"), keys: ["O"] },
        { desc: t("settings.shortcuts.toggleRead"), keys: ["U"] },
        { desc: t("settings.shortcuts.exitFocus"), keys: ["Esc"] },
      ],
    },
    {
      title: t("settings.shortcuts.actions"),
      items: [
        { desc: t("settings.shortcuts.star"), keys: ["S"] },
        { desc: t("settings.shortcuts.readLater"), keys: ["B"] },
        { desc: t("settings.shortcuts.aiSummary"), keys: ["I"] },
        { desc: t("settings.shortcuts.markAllRead"), keys: ["⇧", "A"] },
      ],
    },
    {
      title: t("settings.shortcuts.view"),
      items: [
        { desc: t("settings.shortcuts.focusReading"), keys: ["F"] },
        { desc: t("settings.shortcuts.hideRead"), keys: ["V"] },
        { desc: t("settings.shortcuts.toggleTheme"), keys: ["⇧", "D"] },
      ],
    },
    {
      title: t("settings.shortcuts.global"),
      items: [
        { desc: t("settings.shortcuts.commandPalette"), keys: [modKey, "K"] },
        { desc: t("settings.shortcuts.refreshAll"), keys: [modKey, "R"] },
        { desc: t("settings.shortcuts.addFeed"), keys: ["A"] },
        { desc: t("settings.shortcuts.openSettings"), keys: [modKey, ","] },
      ],
    },
  ];
  return (
    <>
      {groups.map((g) => (
        <div className="settings-group" key={g.title}>
          <h3 className="settings-group-title">{g.title}</h3>
          <div className="s-shortcuts">
            {g.items.map((it, i) => (
              <div className="s-shortcut" key={i}>
                <span className="desc">{it.desc}</span>
                <span className="keys">
                  {it.keys.map((k, j) => (
                    <span className="s-key" key={j}>
                      {k}
                    </span>
                  ))}
                </span>
              </div>
            ))}
          </div>
        </div>
      ))}
    </>
  );
}

/* ── notifications ───────────────────────────────────────── */
function NotificationsSection() {
  const { t } = useTranslation();
  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.notifications.system")}</h3>
        <SettingFlag
          settingKey="notify_enabled"
          fallback
          label={t("settings.notifications.allow")}
          desc={t("settings.notifications.allowDesc")}
        />
        <SettingFlag
          settingKey="notify_badge"
          fallback
          label={t("settings.notifications.badge")}
          desc={t("settings.notifications.badgeDesc")}
        />
        <SettingFlag
          settingKey="notify_sound"
          label={t("settings.notifications.sound")}
          desc={t("settings.notifications.soundDesc")}
        />
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.notifications.dnd")}</h3>
        <SettingFlag
          settingKey="notify_dnd_night"
          label={t("settings.notifications.dndNight")}
          desc={t("settings.notifications.dndNightDesc")}
        />
      </div>
    </>
  );
}

/* ── advanced ────────────────────────────────────────────── */
function AdvancedSection({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  return (
    <>
      <AiSettingsGroup onToast={onToast} />
      <StorageGroup onToast={onToast} />
      <NetworkGroup onToast={onToast} />
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.advanced.experimental")}</h3>
        <SettingFlag
          settingKey="dedup_enabled"
          label={t("settings.advanced.dedup")}
          desc={t("settings.advanced.dedupDesc")}
        />
      </div>
      <DangerZone onToast={onToast} />
    </>
  );
}

/** Storage panel — real database size, retention cleanup, vacuum. */
function StorageGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const stats = useQuery({
    queryKey: ["storage-stats"],
    queryFn: api.storageStats,
  });
  const [retention, setRetention] = useState("forever");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .getSetting("retention_days")
      .then((v) => {
        if (v) setRetention(v);
      })
      .catch(() => {});
  }, []);

  const cleanup = async () => {
    if (retention === "forever") {
      onToast(t("settings.advanced.cleanupForever"));
      return;
    }
    setBusy(true);
    try {
      const n = await api.cleanupArticles(Number(retention));
      await qc.invalidateQueries();
      onToast(
        n > 0
          ? t("settings.advanced.cleanupDone", { count: n })
          : t("settings.advanced.cleanupNone"),
      );
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };
  const vacuum = async () => {
    setBusy(true);
    try {
      await api.vacuumDb();
      await qc.invalidateQueries({ queryKey: ["storage-stats"] });
      onToast(t("settings.advanced.vacuumDone"));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const s = stats.data;
  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.storage")}</h3>
      <Row
        label={t("settings.advanced.dbUsage")}
        desc={
          s
            ? t("settings.advanced.dbUsageDesc", {
                articles: s.articleCount,
                feeds: s.feedCount,
              })
            : t("settings.advanced.calculating")
        }
      >
        <span className="s-value">{s ? formatBytes(s.dbBytes) : "—"}</span>
      </Row>
      <Row
        label={t("settings.advanced.retention")}
        desc={t("settings.advanced.retentionDesc")}
      >
        <Select
          value={retention}
          options={[
            { value: "30", label: t("settings.advanced.retention30") },
            { value: "90", label: t("settings.advanced.retention90") },
            { value: "180", label: t("settings.advanced.retention180") },
            { value: "forever", label: t("settings.advanced.retentionForever") },
          ]}
          onChange={(v) => {
            setRetention(v);
            api.setSetting("retention_days", v).catch(() => {});
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.cleanupNow")}
        desc={t("settings.advanced.cleanupNowDesc")}
      >
        <button className="s-btn" onClick={cleanup} disabled={busy}>
          {t("settings.advanced.cleanup")}
        </button>
      </Row>
      <Row
        label={t("settings.advanced.vacuum")}
        desc={t("settings.advanced.vacuumDesc")}
      >
        <button className="s-btn" onClick={vacuum} disabled={busy}>
          {t("settings.advanced.compress")}
        </button>
      </Row>
    </div>
  );
}

/** Network panel — proxy, fetch concurrency, request timeout. */
function NetworkGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const [proxy, setProxy] = useState("system");
  const [customProxy, setCustomProxy] = useState("");
  const [concurrency, setConcurrency] = useState(6);
  const [timeoutSec, setTimeoutSec] = useState(30);

  useEffect(() => {
    Promise.all([
      api.getSetting("net_proxy"),
      api.getSetting("net_concurrency"),
      api.getSetting("net_timeout_sec"),
    ])
      .then(([p, c, t]) => {
        if (p === "system" || p === "none") setProxy(p);
        else if (p) {
          setProxy("custom");
          setCustomProxy(p);
        }
        // Clamp to each slider's range (concurrency 1–16, timeout 5–120) so a
        // stale or corrupt stored value can't show a NaN / out-of-range readout.
        if (c) setConcurrency(clampSetting(c, 6, 1, 16));
        if (t) setTimeoutSec(clampSetting(t, 30, 5, 120));
      })
      .catch(() => {});
  }, []);

  const saveProxy = (mode: string, custom: string) => {
    const value = mode === "custom" ? custom : mode;
    api
      .setSetting("net_proxy", value)
      .then(() => api.applyNetworkSettings())
      .then(() => onToast(t("settings.advanced.proxyApplied")))
      .catch((e) => reportError(e));
  };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.network")}</h3>
      <Row label={t("settings.advanced.proxy")}>
        <Select
          value={proxy}
          options={[
            { value: "system", label: t("settings.advanced.proxySystem") },
            { value: "none", label: t("settings.advanced.proxyNone") },
            { value: "custom", label: t("settings.advanced.proxyCustom") },
          ]}
          onChange={(v) => {
            setProxy(v);
            if (v !== "custom") saveProxy(v, "");
          }}
        />
      </Row>
      {proxy === "custom" && (
        <Row
          label={t("settings.advanced.proxyAddress")}
          desc={t("settings.advanced.proxyAddressDesc")}
        >
          <input
            className="s-text-input"
            value={customProxy}
            placeholder="http://host:port"
            onChange={(e) => setCustomProxy(e.target.value)}
            onBlur={() => saveProxy("custom", customProxy)}
          />
        </Row>
      )}
      <Row
        label={t("settings.advanced.concurrency")}
        desc={t("settings.advanced.concurrencyDesc")}
      >
        <Slider
          value={concurrency}
          min={1}
          max={16}
          onChange={setConcurrency}
          onCommit={(v) =>
            api.setSetting("net_concurrency", String(v)).catch(() => {})
          }
        />
      </Row>
      <Row label={t("settings.advanced.timeout")}>
        <Slider
          value={timeoutSec}
          min={5}
          max={120}
          step={5}
          unit={t("settings.advanced.secondsUnit")}
          onChange={setTimeoutSec}
          onCommit={(v) =>
            api
              .setSetting("net_timeout_sec", String(v))
              .then(() => api.applyNetworkSettings())
              .catch(() => {})
          }
        />
      </Row>
    </div>
  );
}

/** Danger zone — reset settings, wipe all local data. Each action is gated by
 *  a themed ConfirmDialog rather than the native, unstyled window.confirm. */
function DangerZone({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [confirming, setConfirming] = useState<null | "reset" | "clear">(null);

  const doReset = async () => {
    try {
      await api.resetSettings();
      for (const k of Object.keys(localStorage)) {
        if (
          k.startsWith("pref.") ||
          [
            "theme", "accent", "density", "viewMode", "readerFont", "useSerif",
            "readerSize", "readerLeading", "readerWidth", "collapsedFolders",
          ].includes(k)
        ) {
          localStorage.removeItem(k);
        }
      }
      onToast(t("settings.advanced.resetDone"));
      setTimeout(() => location.reload(), 900);
    } catch (e) {
      reportError(e);
    }
  };
  const doClear = async () => {
    try {
      await api.clearAllData();
      await qc.invalidateQueries();
      onToast(t("settings.advanced.clearDone"));
    } catch (e) {
      reportError(e);
    }
  };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.dangerZone")}</h3>
      <Row
        label={t("settings.advanced.resetSettings")}
        desc={t("settings.advanced.resetSettingsDesc")}
      >
        <button className="s-btn" onClick={() => setConfirming("reset")}>
          {t("settings.advanced.reset")}
        </button>
      </Row>
      <Row
        label={t("settings.advanced.clearData")}
        desc={t("settings.advanced.clearDataDesc")}
      >
        <button className="s-btn danger" onClick={() => setConfirming("clear")}>
          {t("settings.advanced.clear")}
        </button>
      </Row>
      {confirming === "reset" && (
        <ConfirmDialog
          title={t("settings.advanced.resetSettings")}
          message={t("settings.advanced.resetConfirm")}
          confirmLabel={t("settings.advanced.reset")}
          onConfirm={doReset}
          onClose={() => setConfirming(null)}
        />
      )}
      {confirming === "clear" && (
        <ConfirmDialog
          title={t("settings.advanced.clearData")}
          message={t("settings.advanced.clearConfirm")}
          confirmLabel={t("common.delete")}
          onConfirm={doClear}
          onClose={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

/** Real AI provider configuration — backing the AI summary feature. */
function AiSettingsGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [provider, setProvider] = useState<"anthropic" | "openai">("anthropic");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  // Empty = not yet set; the Select then falls back to the UI language.
  const [translateLang, setTranslateLang] = useState("");
  const savedKey = useRef("");
  const savedModel = useRef("");
  const savedBaseUrl = useRef("");

  useEffect(() => {
    Promise.all([
      api.getSetting("ai_provider"),
      api.getSetting("ai_api_key"),
      api.getSetting("ai_model"),
      api.getSetting("ai_base_url"),
      api.getSetting("translate_target_lang"),
    ])
      .then(([p, k, m, b, tl]) => {
        if (p === "openai" || p === "anthropic") setProvider(p);
        if (k) {
          setApiKey(k);
          savedKey.current = k;
        }
        if (m) {
          setModel(m);
          savedModel.current = m;
        }
        if (b) {
          setBaseUrl(b);
          savedBaseUrl.current = b;
        }
        if (tl) setTranslateLang(tl);
      })
      .catch(() => {});
  }, []);

  const save = (key: string, value: string, label: string) => {
    api
      .setSetting(key, value)
      .then(() => onToast(t("settings.advanced.aiSaved", { label })))
      .catch((e) => reportError(e));
  };

  const placeholder =
    provider === "openai"
      ? t("settings.advanced.aiModelPlaceholderOpenai")
      : t("settings.advanced.aiModelPlaceholderAnthropic");

  const baseUrlPlaceholder =
    provider === "openai"
      ? "https://api.openai.com/v1"
      : "https://api.anthropic.com/v1";

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.aiSummary")}</h3>
      <Row
        label={t("settings.advanced.aiProvider")}
        desc={t("settings.advanced.aiProviderDesc")}
      >
        <Select
          value={provider}
          options={[
            { value: "anthropic", label: "Anthropic" },
            { value: "openai", label: "OpenAI" },
          ]}
          onChange={(v) => {
            setProvider(v);
            // The model name and base URL are provider-specific — carrying
            // them over would send e.g. an OpenAI model to Anthropic. Clear
            // both so the backend falls back to the new provider's defaults.
            setModel("");
            savedModel.current = "";
            setBaseUrl("");
            savedBaseUrl.current = "";
            Promise.all([
              api.setSetting("ai_provider", v),
              api.setSetting("ai_model", ""),
              api.setSetting("ai_base_url", ""),
            ])
              .then(() =>
                onToast(
                  t("settings.advanced.aiSaved", {
                    label: t("settings.advanced.aiProviderLabel"),
                  }),
                ),
              )
              .catch((e) => reportError(e));
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiApiKey")}
        desc={t("settings.advanced.aiApiKeyDesc")}
      >
        <input
          className="s-text-input"
          type="password"
          value={apiKey}
          placeholder="sk-…"
          onChange={(e) => setApiKey(e.target.value)}
          onBlur={() => {
            // Trim before persisting — a pasted key routinely carries a
            // trailing newline / space that would break the auth header.
            const trimmed = apiKey.trim();
            if (trimmed !== apiKey) setApiKey(trimmed);
            if (trimmed !== savedKey.current) {
              savedKey.current = trimmed;
              save("ai_api_key", trimmed, t("settings.advanced.aiApiKeyLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiModel")}
        desc={t("settings.advanced.aiModelDesc")}
      >
        <input
          className="s-text-input"
          type="text"
          value={model}
          placeholder={placeholder}
          onChange={(e) => setModel(e.target.value)}
          onBlur={() => {
            // Trim before persisting — a pasted model name with a stray
            // space / newline yields a "model not found" from the provider.
            const trimmed = model.trim();
            if (trimmed !== model) setModel(trimmed);
            if (trimmed !== savedModel.current) {
              savedModel.current = trimmed;
              save("ai_model", trimmed, t("settings.advanced.aiModelLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiBaseUrl")}
        desc={t("settings.advanced.aiBaseUrlDesc")}
      >
        <input
          className="s-text-input"
          type="text"
          value={baseUrl}
          placeholder={baseUrlPlaceholder}
          onChange={(e) => setBaseUrl(e.target.value)}
          onBlur={() => {
            const trimmed = baseUrl.trim();
            if (trimmed !== baseUrl) setBaseUrl(trimmed);
            if (trimmed !== savedBaseUrl.current) {
              savedBaseUrl.current = trimmed;
              save("ai_base_url", trimmed, t("settings.advanced.aiBaseUrlLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.translateLang")}
        desc={t("settings.advanced.translateLangDesc")}
      >
        <Select
          value={translateLang || i18n.language}
          options={LANGUAGES.map((l) => ({ value: l.code, label: l.label }))}
          onChange={(v) => {
            setTranslateLang(v);
            save("translate_target_lang", v, t("settings.advanced.translateLangLabel"));
            // The reader caches this setting to decide whether a stored
            // translation is still current — refresh it so a language change
            // takes effect immediately.
            qc.invalidateQueries({ queryKey: ["setting", "translate_target_lang"] });
          }}
        />
      </Row>
    </div>
  );
}

/* ── filters ─────────────────────────────────────────────── */
function FiltersSection({
  feeds,
  onToast,
}: {
  feeds: Feed[];
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const rules = useQuery({ queryKey: ["rules"], queryFn: api.listRules });
  // `null` = not editing, "new" = the add form, a Rule = editing that rule.
  const [editing, setEditing] = useState<Rule | "new" | null>(null);

  const refresh = () => qc.invalidateQueries({ queryKey: ["rules"] });
  // Saving a rule also backfills existing articles (star / read / skip), so
  // refresh the article list, sidebar counts and per-feed counts the backfill
  // touched — not just the rule list. The editor shows the "saved"/"applied"
  // toast itself, since only it knows how many articles were affected.
  const afterSaved = () => {
    setEditing(null);
    refresh();
    qc.invalidateQueries({ queryKey: ["articles"] });
    qc.invalidateQueries({ queryKey: ["counts"] });
    qc.invalidateQueries({ queryKey: ["feeds"] });
  };
  const feedName = (id: number | null) =>
    id == null
      ? t("settings.filters.allFeeds")
      : feeds.find((f) => f.id === id)?.title ?? t("settings.filters.allFeeds");

  const toggle = (r: Rule) =>
    api
      .updateRule(r.id, r.name, !r.enabled, r.feedId, r.field, r.query, r.action)
      .then(refresh)
      .catch((e) => reportError(e));

  const remove = (r: Rule) =>
    api
      .deleteRule(r.id)
      .then(() => {
        refresh();
        onToast(t("settings.filters.deleted"));
      })
      .catch((e) => reportError(e));

  const summary = (r: Rule) =>
    [
      t(`settings.filters.action.${r.action}`),
      "·",
      t(`settings.filters.field.${r.field}`),
      `“${r.query}”`,
      "·",
      feedName(r.feedId),
    ].join(" ");

  const list = rules.data ?? [];

  return (
    <>
      <div className="settings-group" style={{ marginBottom: 18 }}>
        <h3 className="settings-group-title">{t("settings.filters.title")}</h3>
        <p className="settings-group-desc">{t("settings.filters.intro")}</p>
        {editing !== "new" && (
          <button
            className="s-btn primary"
            style={{ marginTop: 10 }}
            onClick={() => setEditing("new")}
          >
            <Icon name="plus" size={12} /> {t("settings.filters.newRule")}
          </button>
        )}
      </div>

      {editing === "new" && (
        <RuleEditor
          rule={null}
          feeds={feeds}
          onCancel={() => setEditing(null)}
          onSaved={afterSaved}
          onToast={onToast}
        />
      )}

      <div className="settings-group">
        {list.length === 0 && editing !== "new" && (
          <div style={{ padding: "16px 4px", fontSize: 13, color: "var(--muted)" }}>
            {t("settings.filters.empty")}
          </div>
        )}
        {list.map((r) =>
          editing !== "new" && typeof editing === "object" && editing?.id === r.id ? (
            <RuleEditor
              key={r.id}
              rule={r}
              feeds={feeds}
              onCancel={() => setEditing(null)}
              onSaved={afterSaved}
              onToast={onToast}
            />
          ) : (
            <div className="rule-row" key={r.id}>
              <Toggle checked={r.enabled} onChange={() => toggle(r)} />
              <div className="rule-text" style={{ opacity: r.enabled ? 1 : 0.5 }}>
                <div className="rule-name">
                  {r.name || t("settings.filters.untitled")}
                </div>
                <div className="rule-summary">{summary(r)}</div>
              </div>
              <div className="actions">
                <button
                  className="icon-btn"
                  title={t("common.rename")}
                  onClick={() => setEditing(r)}
                >
                  <Icon name="settings" size={13} />
                </button>
                <button
                  className="icon-btn"
                  title={t("common.delete")}
                  onClick={() => remove(r)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            </div>
          ),
        )}
      </div>
    </>
  );
}

function RuleEditor({
  rule,
  feeds,
  onCancel,
  onSaved,
  onToast,
}: {
  rule: Rule | null;
  feeds: Feed[];
  onCancel: () => void;
  onSaved: () => void;
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState(rule?.name ?? "");
  const [query, setQuery] = useState(rule?.query ?? "");
  const [field, setField] = useState<RuleField>(rule?.field ?? "title");
  const [action, setAction] = useState<RuleAction>(rule?.action ?? "skip");
  const [scope, setScope] = useState(rule?.feedId == null ? "" : String(rule.feedId));
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<RulePreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  // When a `skip` rule would delete existing matches, hold that count here to
  // pop a confirmation before the destructive backfill runs.
  const [pendingDelete, setPendingDelete] = useState<number | null>(null);

  // Debounced dry-run: count matching stored articles as the draft changes.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setPreview(null);
      return;
    }
    setPreviewing(true);
    const feedId = scope === "" ? null : Number(scope);
    // `cancelled` guards against a stale response: a request started before
    // the draft changed could otherwise resolve last and overwrite the
    // preview for the current draft.
    let cancelled = false;
    const handle = window.setTimeout(() => {
      api
        .previewRule(feedId, field, q)
        .then((r) => !cancelled && setPreview(r))
        .catch(() => !cancelled && setPreview(null))
        .finally(() => !cancelled && setPreviewing(false));
    }, 400);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [query, field, scope]);

  // A rule backfills the existing backlog only when it's active: new rules
  // start enabled, and an existing rule keeps its current enabled state (the
  // editor has no enabled toggle — that lives in the rule list).
  const willApply = rule ? rule.enabled : true;

  // Save the rule, then backfill the existing articles it matches. Split out
  // from `save` so the `skip` confirmation can resume here after the user agrees.
  const persist = async () => {
    setBusy(true);
    const feedId = scope === "" ? null : Number(scope);
    const q = query.trim();
    try {
      if (rule) {
        await api.updateRule(rule.id, name, rule.enabled, feedId, field, q, action);
      } else {
        await api.createRule(name, feedId, field, q, action);
      }
      const applied = willApply
        ? await api.applyRuleToExisting(feedId, field, q, action)
        : 0;
      onToast(
        applied > 0
          ? t("settings.filters.appliedExisting", { count: applied })
          : t("settings.filters.saved"),
      );
      onSaved();
    } catch (e) {
      reportError(e);
      setBusy(false);
    }
  };

  const save = async () => {
    const q = query.trim();
    if (!q) {
      onToast(t("settings.filters.needQuery"));
      return;
    }
    // `skip` deletes the existing articles it matches — confirm first, showing
    // the exact count fetched fresh (not the debounced preview, which may lag).
    if (willApply && action === "skip") {
      setBusy(true);
      const feedId = scope === "" ? null : Number(scope);
      try {
        const p = await api.previewRule(feedId, field, q);
        if (p.count > 0) {
          setPendingDelete(p.count);
          return;
        }
      } catch (e) {
        // Never fall through to the destructive backfill when we couldn't
        // confirm how many articles it would delete.
        reportError(e);
        return;
      } finally {
        setBusy(false);
      }
    }
    await persist();
  };

  return (
    <div className="rule-card">
      <input
        className="rule-input"
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder={t("settings.filters.namePlaceholder")}
      />
      <input
        className="rule-input"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder={t("settings.filters.queryPlaceholder")}
      />
      <div className="rule-fields">
        <label>
          {t("settings.filters.matchIn")}
          <Select
            value={field}
            onChange={(v) => setField(v as RuleField)}
            options={[
              { value: "title", label: t("settings.filters.field.title") },
              { value: "author", label: t("settings.filters.field.author") },
              { value: "content", label: t("settings.filters.field.content") },
              { value: "any", label: t("settings.filters.field.any") },
            ]}
          />
        </label>
        <label>
          {t("settings.filters.thenLabel")}
          <Select
            value={action}
            onChange={(v) => setAction(v as RuleAction)}
            options={[
              { value: "skip", label: t("settings.filters.action.skip") },
              { value: "read", label: t("settings.filters.action.read") },
              { value: "star", label: t("settings.filters.action.star") },
            ]}
          />
        </label>
        <label>
          {t("settings.filters.scopeLabel")}
          <Select
            value={scope}
            onChange={setScope}
            options={[
              { value: "", label: t("settings.filters.allFeeds") },
              ...feeds.map((f) => ({ value: String(f.id), label: f.title })),
            ]}
          />
        </label>
      </div>
      {query.trim() && (
        <div className="rule-preview">
          <span className="rule-preview-count">
            {previewing && !preview
              ? t("settings.filters.preview.checking")
              : t("settings.filters.preview.count", {
                  count: preview?.count ?? 0,
                })}
          </span>
          {preview && preview.samples.length > 0 && (
            <ul className="rule-preview-samples">
              {preview.samples.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="rule-card-actions">
        <button className="s-btn" onClick={onCancel} disabled={busy}>
          {t("common.cancel")}
        </button>
        <button className="s-btn primary" onClick={save} disabled={busy}>
          {t("common.save")}
        </button>
      </div>
      {pendingDelete != null && (
        <ConfirmDialog
          title={t("settings.filters.confirmSkipTitle")}
          message={t("settings.filters.confirmSkipMessage", { count: pendingDelete })}
          confirmLabel={t("settings.filters.confirmSkipConfirm")}
          onConfirm={persist}
          onClose={() => setPendingDelete(null)}
        />
      )}
    </div>
  );
}

/* ── about ───────────────────────────────────────────────── */
function AboutSection() {
  const { t } = useTranslation();
  const version = useAppVersion();
  const [checking, setChecking] = useState(false);
  const onCheck = async () => {
    setChecking(true);
    try {
      await checkForUpdates({ silent: false });
    } finally {
      setChecking(false);
    }
  };
  return (
    <div className="s-about">
      <div className="mark">
        <Icon name="papr" size={34} color="#fff" />
      </div>
      <h1 className="app-name">Papr</h1>
      <p className="tagline">{t("settings.about.tagline")}</p>
      <div className="version">
        Version{version && ` ${version}`}
      </div>
      <button className="s-btn about-update" onClick={onCheck} disabled={checking}>
        {checking ? t("update.checking") : t("update.checkButton")}
      </button>
      <p className="credits">
        {t("settings.about.creditsFonts")}
        <br />
        {t("settings.about.creditsRender")}
        <br />
        {t("settings.about.creditsThanks")}
      </p>
    </div>
  );
}
