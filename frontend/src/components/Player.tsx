import { useEffect, useRef, useState } from "react";
import WaveSurfer from "wavesurfer.js";
import { Pause, Play } from "lucide-react";
import { fmtTime } from "../api";
import { claimAudio, cssVar, onClaim } from "../lib/player";

/** Inline mini player (reference clips, chunks, previews). */
export default function Player({ src, height = 40, compact = false }: { src: string; height?: number; compact?: boolean }) {
  const el = useRef<HTMLDivElement>(null);
  const ws = useRef<WaveSurfer | null>(null);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [dur, setDur] = useState(0);
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!el.current) return;
    setError(false);
    const w = WaveSurfer.create({
      container: el.current, url: src, height, barWidth: 2, barGap: 2, barRadius: 2, normalize: true,
      waveColor: cssVar("--wave"), progressColor: cssVar("--accent"), cursorWidth: 0,
    });
    ws.current = w;
    const owner = {};
    const off = onClaim((o) => { if (o !== owner) w.pause(); });
    w.on("ready", () => setDur(w.getDuration()));
    w.on("timeupdate", setTime);
    w.on("play", () => { claimAudio(owner); setPlaying(true); });
    w.on("pause", () => setPlaying(false));
    w.on("finish", () => setPlaying(false));
    w.on("error", () => setError(true));
    return () => { off(); w.destroy(); };
  }, [src, height]);

  const s = compact ? 30 : 36;
  return (
    <div className="flex items-center gap-3">
      <button
        onClick={() => ws.current?.playPause()}
        style={{ width: s, height: s }}
        className="flex shrink-0 items-center justify-center rounded-full bg-invert text-invert-fg transition hover:opacity-85"
        aria-label={playing ? "Pause" : "Play"}
      >
        {playing ? <Pause size={compact ? 12 : 14} fill="currentColor" /> : <Play size={compact ? 12 : 14} fill="currentColor" className="ml-0.5" />}
      </button>
      <div className="min-w-0 flex-1">
        {/* The waveform container stays mounted so a later source can load after an error. */}
        <div ref={el} className={`w-full cursor-pointer ${error ? "hidden" : ""}`} />
        {error && <div className="text-xs text-danger">Could not load audio</div>}
      </div>
      <div className="shrink-0 text-right font-mono text-[11px] tabular-nums text-fg-3">
        {fmtTime(time)} / {fmtTime(dur)}
      </div>
    </div>
  );
}
