import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { usePlayer, PLAYBACK_RATES } from "../player";
import { useUi } from "../store";
import Icon from "./Icon";

/** m:ss for a seconds value, or h:mm:ss past an hour (podcasts run long);
 *  "–:––" while the duration is unknown. */
function clock(s: number): string {
  if (!isFinite(s) || s < 0) return "–:––";
  const total = Math.floor(s);
  const sec = (total % 60).toString().padStart(2, "0");
  const min = Math.floor(total / 60) % 60;
  const hr = Math.floor(total / 3600);
  return hr > 0
    ? `${hr}:${min.toString().padStart(2, "0")}:${sec}`
    : `${min}:${sec}`;
}

/**
 * Persistent audio mini-player. Mounted once at the app shell, it keeps a
 * podcast episode playing while the user browses other articles. Renders
 * nothing until a track is loaded.
 */
export default function PlayerBar() {
  const { t } = useTranslation();
  const track = usePlayer((s) => s.track);
  const playing = usePlayer((s) => s.playing);
  const rate = usePlayer((s) => s.rate);
  const setPlaying = usePlayer((s) => s.setPlaying);
  const toggle = usePlayer((s) => s.toggle);
  const setRate = usePlayer((s) => s.setRate);
  const close = usePlayer((s) => s.close);

  const audioRef = useRef<HTMLAudioElement>(null);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [failed, setFailed] = useState(false);
  // True between starting a new src `load()` and its metadata arriving. The
  // element fires a spurious `pause` while it resets for the new src; that
  // pause must not be mirrored into the store (it would cancel the new track).
  const loadingRef = useRef(false);

  // Load a new src and reset the timeline when the track changes.
  useEffect(() => {
    const el = audioRef.current;
    if (!el || !track) return;
    loadingRef.current = true;
    el.src = track.src;
    el.load();
    setTime(0);
    setDuration(0);
    setFailed(false);
  }, [track?.src]);

  // Reflect play/pause intent onto the element.
  useEffect(() => {
    const el = audioRef.current;
    if (!el || !track) return;
    if (playing) {
      // Re-requesting playback of a track that previously failed (same src,
      // so the track-change effect above does not re-run) should retry the
      // load — the failure may have been a transient network error. Without
      // this the player stays permanently stuck on "load error".
      if (failed) {
        setFailed(false);
        // `el.load()` resets the element, which fires a transient `pause`.
        // Flag the reload so `onPause` ignores that pause — otherwise it would
        // mirror `playing → false` into the store and cancel the very retry
        // the user just asked for (leaving the track stuck until a 2nd click).
        loadingRef.current = true;
        el.load();
      }
      el.play().catch(() => setPlaying(false));
    } else {
      el.pause();
    }
  }, [playing, track?.src, setPlaying, failed]);

  // Keep the element's speed in sync with the store. `defaultPlaybackRate` is
  // set alongside `playbackRate` because loading a new src resets the live
  // `playbackRate` back to `defaultPlaybackRate` once the media is ready — so
  // without this a track switch would silently snap the user's chosen speed
  // back to 1×. The `onLoadedMetadata` handler re-asserts it after the load
  // actually completes, which is when the reset would otherwise land.
  useEffect(() => {
    const el = audioRef.current;
    if (el) {
      el.defaultPlaybackRate = rate;
      el.playbackRate = rate;
    }
  }, [rate, track?.src]);

  if (!track) return null;

  const seek = (to: number) => {
    const el = audioRef.current;
    if (el && isFinite(to)) {
      el.currentTime = to;
      setTime(to);
    }
  };
  const nudge = (delta: number) =>
    seek(Math.min(duration || Infinity, Math.max(0, time + delta)));
  const cycleRate = () => {
    // indexOf is -1 for an unknown rate, so this lands on the first entry.
    const i = PLAYBACK_RATES.indexOf(rate as (typeof PLAYBACK_RATES)[number]);
    setRate(PLAYBACK_RATES[(i + 1) % PLAYBACK_RATES.length]);
  };

  return (
    <div className="player-bar">
      <audio
        ref={audioRef}
        onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => {
          // The new src is ready: any further `pause` is now a genuine one
          // (user / media key), so let `onPause` mirror it to the store.
          loadingRef.current = false;
          setDuration(e.currentTarget.duration);
          // The element resets playbackRate to its default once new media is
          // loaded — re-apply the chosen speed so a track switch keeps it.
          e.currentTarget.playbackRate = rate;
        }}
        onEnded={() => setPlaying(false)}
        // The element's play state can change without the store driving it —
        // hardware media keys (F8 / AirPods) and the macOS Now Playing widget
        // pause/resume the <audio> directly. Mirror those back into the store
        // so the play/pause button never shows the wrong icon (and a click is
        // not wasted just re-syncing the state). Re-asserting the same value
        // is a no-op for both the store and the play/pause effect.
        onPlay={() => {
          loadingRef.current = false;
          setPlaying(true);
        }}
        onPause={() => {
          // `pause` also fires transiently while the element resets for a new
          // src; that pause is internal, not a user/media-key pause, so it
          // must not flip the store off (it would cancel the new track).
          if (!loadingRef.current) setPlaying(false);
        }}
        onError={() => {
          // A bad enclosure URL or unsupported codec would otherwise leave
          // the bar stuck at 0:00 with no hint of what went wrong.
          setFailed(true);
          setPlaying(false);
        }}
        preload="metadata"
      />

      <span className="player-ico">
        <Icon name="headphones" size={15} />
      </span>

      <div className="player-meta">
        <button
          className="player-title"
          onClick={() => useUi.getState().openArticle(track.articleId)}
          title={track.title}
          aria-label={t("player.openArticle", { title: track.title })}
        >
          {track.title}
        </button>
        <span className="player-feed">{track.feedTitle}</span>
      </div>

      <div className="player-controls">
        <button
          className="player-btn"
          onClick={() => nudge(-15)}
          title={t("player.back15")}
          aria-label={t("player.back15")}
          disabled={failed}
        >
          <Icon name="skip-back" size={15} />
        </button>
        <button
          className="player-btn play"
          onClick={toggle}
          title={
            failed
              ? t("player.retry")
              : playing
                ? t("player.pause")
                : t("player.play")
          }
          aria-label={
            failed
              ? t("player.retry")
              : playing
                ? t("player.pause")
                : t("player.play")
          }
        >
          <Icon name={playing ? "pause" : "play"} size={15} />
        </button>
        <button
          className="player-btn"
          onClick={() => nudge(30)}
          title={t("player.fwd30")}
          aria-label={t("player.fwd30")}
          disabled={failed}
        >
          <Icon name="skip-fwd" size={15} />
        </button>
      </div>

      {failed ? (
        <span className="player-error">
          <Icon name="alert" size={13} />
          {t("player.loadError")}
        </span>
      ) : (
        <>
          <span className="player-time">{clock(time)}</span>
          <input
            className="player-scrub"
            type="range"
            min={0}
            max={duration || 0}
            step={1}
            value={Math.min(time, duration || 0)}
            onChange={(e) => seek(Number(e.target.value))}
            aria-label={t("player.seek")}
            // Announce the elapsed time, not the raw second count.
            aria-valuetext={clock(time)}
          />
          <span className="player-time">{clock(duration)}</span>
        </>
      )}

      <button
        className="player-btn rate"
        onClick={cycleRate}
        title={t("player.speed")}
        aria-label={t("player.speedValue", { rate })}
      >
        {rate}×
      </button>
      <button
        className="player-btn"
        onClick={close}
        title={t("player.close")}
        aria-label={t("player.close")}
      >
        <Icon name="x" size={14} />
      </button>
    </div>
  );
}
