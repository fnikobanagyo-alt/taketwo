import { useEffect, useRef, useState } from "react";
import { Pause, Play } from "lucide-react";
import type { Profile } from "../api";
import { claimAudio, onClaim } from "../lib/player";

// One shared <audio> element for every preview button, so only one plays at a time.
const audio = typeof Audio !== "undefined" ? new Audio() : null;
let owner: symbol | null = null;
const subs = new Set<() => void>();
const notify = () => subs.forEach((f) => f());
if (audio) {
  ["play", "pause", "ended", "timeupdate", "loadedmetadata"].forEach((ev) => audio.addEventListener(ev, notify));
  onClaim((o) => { if (o !== audio) audio.pause(); });
}

export function previewUrl(p: Pick<Profile, "id"> & { preview?: { created?: number } | null }) {
  return `/api/profiles/${p.id}/preview?v=${Math.round(p.preview?.created ?? 0)}`;
}

/** Small play/pause button that previews a voice. Safe to place inside clickable rows:
 *  it never triggers the row's own click. */
export default function VoicePreview({ profile, size = 28, className = "" }: { profile: Profile; size?: number; className?: string }) {
  const me = useRef(Symbol(profile.id));
  const [, force] = useState(0);
  useEffect(() => {
    const f = () => force((n) => n + 1);
    subs.add(f);
    return () => { subs.delete(f); if (owner === me.current) audio?.pause(); };
  }, []);

  if (!audio || profile.status !== "ready") return null;
  const mine = owner === me.current;
  const playing = mine && !audio.paused;
  const pct = mine && audio.duration ? audio.currentTime / audio.duration : 0;
  const generated = !!profile.preview;

  const toggle = (e: React.SyntheticEvent) => {
    e.stopPropagation();
    e.preventDefault();
    if (playing) { audio.pause(); return; }
    const url = previewUrl(profile);
    if (!mine || !audio.src.endsWith(url)) {
      owner = me.current;
      audio.src = url;
    } else if (audio.ended) {
      audio.currentTime = 0;
    }
    claimAudio(audio);
    audio.play().catch(() => {});
    notify();
  };

  const r = size / 2 - 1.5, c = 2 * Math.PI * r;
  return (
    <span
      role="button" tabIndex={0} onClick={toggle} onMouseDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") toggle(e); }}
      title={playing ? "Pause preview" : generated ? "Play a sample spoken by this clone" : "Play the voice's reference recording (a clone sample is being prepared)"}
      aria-label={playing ? `Pause ${profile.name} preview` : `Play ${profile.name} preview`}
      className={`relative inline-flex shrink-0 cursor-pointer items-center justify-center rounded-full transition ${playing ? "bg-invert text-invert-fg" : "bg-surface-2 text-fg-2 hover:bg-invert hover:text-invert-fg"} ${className}`}
      style={{ width: size, height: size }}
    >
      {playing ? <Pause size={size * 0.42} fill="currentColor" /> : <Play size={size * 0.42} fill="currentColor" className="ml-[1px]" />}
      {mine && pct > 0 && (
        <svg className="pointer-events-none absolute inset-0 -rotate-90" width={size} height={size}>
          <circle cx={size / 2} cy={size / 2} r={r} fill="none" stroke="var(--accent)" strokeWidth="2" strokeDasharray={c} strokeDashoffset={c * (1 - pct)} strokeLinecap="round" />
        </svg>
      )}
    </span>
  );
}
