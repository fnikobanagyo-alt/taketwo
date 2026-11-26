import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import WaveSurfer from "wavesurfer.js";
import { Download, Pause, Play, RotateCcw, RotateCw, X } from "lucide-react";
import VoiceOrb from "./orb";
import { fmtTime } from "../api";

export type Track = { src: string; title: string; subtitle?: string; seed: string; download?: string; autoplay?: boolean };

type Ctx = { track: Track | null; playing: boolean; play: (t: Track) => void; toggle: () => void; close: () => void };
const PlayerCtx = createContext<Ctx>({ track: null, playing: false, play: () => {}, toggle: () => {}, close: () => {} });
export const usePlayer = () => useContext(PlayerCtx);

// Mini players elsewhere pause the global one (and vice versa) through this.
let pauseOthers: (except: unknown) => void = () => {};
export function claimAudio(owner: unknown) { pauseOthers(owner); }
const listeners = new Set<(owner: unknown) => void>();
pauseOthers = (owner) => listeners.forEach((l) => l(owner));
export function onClaim(fn: (owner: unknown) => void) { listeners.add(fn); return () => { listeners.delete(fn); }; }

export function cssVar(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
}

export function PlayerProvider({ children }: { children: ReactNode }) {
  const [track, setTrack] = useState<Track | null>(null);
  const [playing, setPlaying] = useState(false);
  const ws = useRef<WaveSurfer | null>(null);
  const play = useCallback((t: Track) => setTrack({ ...t }), []);
  const toggle = useCallback(() => ws.current?.playPause(), []);
  const close = useCallback(() => { ws.current?.pause(); setTrack(null); }, []);
  return (
    <PlayerCtx.Provider value={{ track, playing, play, toggle, close }}>
      {children}
      {track && <PlayerBar track={track} wsRef={ws} onPlaying={setPlaying} onClose={close} />}
    </PlayerCtx.Provider>
  );
}

function PlayerBar({ track, wsRef, onPlaying, onClose }: { track: Track; wsRef: React.MutableRefObject<WaveSurfer | null>; onPlaying: (p: boolean) => void; onClose: () => void }) {
  const el = useRef<HTMLDivElement>(null);
  const [t, setT] = useState(0);
  const [d, setD] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [rate, setRate] = useState(1);
  const rateRef = useRef(rate);  // a new track starts at the speed the user picked, not at 1x
  rateRef.current = rate;

  useEffect(() => {
    if (!el.current) return;
    const w = WaveSurfer.create({
      container: el.current, url: track.src, height: 34, barWidth: 2, barGap: 2, barRadius: 2, normalize: true,
      waveColor: cssVar("--wave"), progressColor: cssVar("--accent"), cursorWidth: 0, autoplay: track.autoplay !== false,
    });
    wsRef.current = w;
    const owner = {};
    const off = onClaim((o) => { if (o !== owner) w.pause(); });
    w.on("ready", () => { setD(w.getDuration()); w.setPlaybackRate(rateRef.current, true); });
    w.on("timeupdate", setT);
    w.on("play", () => { claimAudio(owner); setPlaying(true); onPlaying(true); });
    w.on("pause", () => { setPlaying(false); onPlaying(false); });
    w.on("finish", () => { setPlaying(false); onPlaying(false); });
    return () => { off(); w.destroy(); wsRef.current = null; onPlaying(false); };
  }, [track, wsRef, onPlaying]);

  useEffect(() => { wsRef.current?.setPlaybackRate(rate, true); }, [rate, wsRef]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      // Space plays/pauses only when focus is not on something Space already operates.
      const el = e.target as HTMLElement | null;
      if (e.code !== "Space" || e.metaKey || e.ctrlKey || e.altKey) return;
      if (el?.closest?.('button, a, summary, input, textarea, select, [contenteditable="true"], [role="button"], [role="switch"], [role="slider"], [role="checkbox"], [role="option"], [role="menuitem"], [role="tab"]')) return;
      e.preventDefault(); wsRef.current?.playPause();
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [wsRef]);

  return (
    <div className="animate-rise fixed inset-x-0 bottom-0 z-40 border-t border-line bg-surface/85 backdrop-blur-xl">
      <div className="flex h-[68px] items-center gap-4 px-5">
        <div className="flex w-64 min-w-0 items-center gap-3">
          <VoiceOrb seed={track.seed} size={36} active={playing} />
          <div className="min-w-0">
            <div className="truncate text-[13px] font-medium">{track.title}</div>
            {track.subtitle && <div className="truncate text-xs text-fg-3">{track.subtitle}</div>}
          </div>
        </div>
        <div className="flex items-center gap-1">
          <button className="icon-btn" onClick={() => wsRef.current?.skip(-10)} title="Back 10s" aria-label="Back 10 seconds"><RotateCcw size={15} /></button>
          <button className="flex h-10 w-10 items-center justify-center rounded-full bg-invert text-invert-fg transition hover:opacity-85" onClick={() => wsRef.current?.playPause()} title="Play / pause (space)">
            {playing ? <Pause size={17} fill="currentColor" /> : <Play size={17} fill="currentColor" className="ml-0.5" />}
          </button>
          <button className="icon-btn" onClick={() => wsRef.current?.skip(10)} title="Forward 10s" aria-label="Forward 10 seconds"><RotateCw size={15} /></button>
        </div>
        <span className="w-12 text-right font-mono text-[12px] tabular-nums text-fg-3">{fmtTime(t)}</span>
        <div ref={el} className="min-w-0 flex-1 cursor-pointer" />
        <span className="w-12 font-mono text-[12px] tabular-nums text-fg-3">{fmtTime(d)}</span>
        <button className="btn btn-ghost btn-sm w-12 font-mono" onClick={() => setRate(rate >= 1.5 ? 0.75 : +(rate + 0.25).toFixed(2))} title="Playback speed (preview only)">
          {rate}×
        </button>
        {track.download && <a className="icon-btn" href={track.download} title="Download" aria-label="Download"><Download size={16} /></a>}
        <button className="icon-btn" onClick={onClose} title="Close" aria-label="Close"><X size={16} /></button>
      </div>
    </div>
  );
}
