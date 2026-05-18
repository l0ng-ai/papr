// Audio player state. The mini-player survives article navigation, so the
// "now playing" track lives in its own global store — independent of the
// reading-pane selection. The actual <audio> element is owned by PlayerBar;
// this store only carries intent (which track, playing or paused, speed).

import { create } from "zustand";

export interface Track {
  /** The article the audio belongs to — lets the player deep-link back. */
  articleId: number;
  title: string;
  feedTitle: string;
  src: string;
}

/** Selectable playback speeds, slowest to fastest. The single source of
 *  truth for what counts as a valid rate — PlayerBar cycles through these. */
export const PLAYBACK_RATES = [0.75, 1, 1.25, 1.5, 2] as const;

const DEFAULT_RATE = 1;

interface PlayerState {
  track: Track | null;
  /** Whether playback should be running (PlayerBar drives the element). */
  playing: boolean;
  /** Playback speed multiplier, persisted across sessions. */
  rate: number;

  /** Load a track and start playing it. Re-playing the same src toggles. */
  play: (track: Track) => void;
  setPlaying: (v: boolean) => void;
  toggle: () => void;
  setRate: (r: number) => void;
  /** Stop playback and tear the player down. */
  close: () => void;
}

const RATE_KEY = "player.rate";

// localStorage is webview-writable and may hold a stale or corrupt value
// (an out-of-range, negative, or non-numeric rate). An invalid playbackRate
// throws on assignment or plays garbled audio, so only accept a known rate.
function loadRate(): number {
  const stored = Number(localStorage.getItem(RATE_KEY));
  return (PLAYBACK_RATES as readonly number[]).includes(stored)
    ? stored
    : DEFAULT_RATE;
}

export const usePlayer = create<PlayerState>((set, get) => ({
  track: null,
  playing: false,
  rate: loadRate(),

  play: (track) => {
    const cur = get().track;
    if (cur && cur.src === track.src) {
      set((s) => ({ playing: !s.playing }));
    } else {
      set({ track, playing: true });
    }
  },
  setPlaying: (playing) => set({ playing }),
  toggle: () => set((s) => ({ playing: s.track ? !s.playing : false })),
  setRate: (rate) => {
    // Guard the element's playbackRate: ignore anything off the known list.
    const next = (PLAYBACK_RATES as readonly number[]).includes(rate)
      ? rate
      : DEFAULT_RATE;
    localStorage.setItem(RATE_KEY, String(next));
    set({ rate: next });
  },
  close: () => set({ track: null, playing: false }),
}));
