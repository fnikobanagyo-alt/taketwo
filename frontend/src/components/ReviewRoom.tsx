import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import WaveSurfer from "wavesurfer.js";
import RegionsPlugin from "wavesurfer.js/dist/plugins/regions.esm.js";
import { AlertTriangle, Check, CheckCircle2, Keyboard, Loader2, Pause, Pencil, Play, RefreshCw, Undo2, X } from "lucide-react";
import { api, fmtTime, ISSUE_LABEL, type Chunk, type Generation, type ReviewItem } from "../api";
import { claimAudio, cssVar, onClaim } from "../lib/player";
import Player from "./Player";
import { useToast } from "./ui";

/** The review room: one waveform of the finished narration (parts that need review marked on it)
 *  above one list of the script's parts, where each doubtful part can be cleared, re-spoken or edited. */
export default function ReviewRoom({ g, busy, refresh }: { g: Generation; busy: boolean; refresh: () => void }) {
  const toast = useToast();
  const review = g.review ?? [];
  const byPart = useMemo(() => new Map(review.map((r) => [r.part - 1, r])), [review]);
  const [only, setOnly] = useState(false);
  const [active, setActive] = useState<number | null>(null); // part under the playhead
  const [focus, setFocus] = useState<number | null>(review[0] ? review[0].part - 1 : null);
  const [keys, setKeys] = useState(() => { try { return localStorage.getItem("tt-review-keys") === "1"; } catch { return false; } });
  const [editing, setEditing] = useState<number | null>(null);
  const seekRef = useRef<(t: number, play?: boolean) => void>(() => {});
  const toggleRef = useRef<() => void>(() => {});
  const rows = useRef(new Map<number, HTMLDivElement>());

  const ready = g.status === "done" && !!g.file && !busy;
  const span = (i: number) => g.timeline?.find((x) => x.i === i);
  const jump = useCallback((i: number, play = true) => {
    const s = g.timeline?.find((x) => x.i === i);
    setFocus(i);
    if (s) seekRef.current(s.start, play);
    rows.current.get(i)?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [g.timeline]);

  const accept = useCallback(async (i: number) => {
    try {
      await api.accept(g.id, i);
      refresh();
      toast(`Part ${i + 1} marked as sounding fine`, "ok", { action: { label: "Undo", run: async () => { await api.unaccept(g.id, i); refresh(); } } });
    } catch (e) { toast((e as Error).message, "error"); }
  }, [g.id, refresh, toast]);
  const respeak = useCallback(async (i: number, text?: string) => {
    try { await api.regenerate(g.id, i, text); toast(`Re-speaking part ${i + 1}. Take ${(g.chunks[i].take ?? 1)} stays available to compare.`, "info"); refresh(); }
    catch (e) { toast((e as Error).message, "error"); }
  }, [g.id, g.chunks, refresh, toast]);

  // optional keyboard review: Space play/pause, N/P next/previous part needing review, A sounds fine, R re-speak, E edit
  useEffect(() => {
    if (!keys) return;
    const k = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement | null;
      if (e.metaKey || e.ctrlKey || e.altKey || el?.closest?.("input, textarea, select, [contenteditable=true], [role=listbox], [role=dialog]")) return;
      const flagged = review.map((r) => r.part - 1);
      const at = focus == null ? -1 : flagged.indexOf(focus);
      const key = e.key.toLowerCase();
      if (e.code === "Space" && !el?.closest?.("button, a, [role=button], [role=switch], [role=slider]")) { e.preventDefault(); e.stopPropagation(); toggleRef.current(); }
      else if (key === "n" && flagged.length) jump(flagged[(at + 1) % flagged.length]);
      else if (key === "p" && flagged.length) jump(flagged[(at - 1 + flagged.length) % flagged.length]);
      else if (key === "a" && focus != null && byPart.has(focus) && ready) accept(focus);
      else if (key === "r" && focus != null && ready) respeak(focus);
      else if (key === "e" && focus != null && ready) { e.preventDefault(); setEditing(focus); }
    };
    window.addEventListener("keydown", k, true);
    return () => window.removeEventListener("keydown", k, true);
  }, [keys, review, focus, byPart, ready, jump, accept, respeak]);

  const parts = only ? g.chunks.filter((c) => byPart.has(c.i)) : g.chunks;
  return (
    <section aria-label="Script and parts" className="border-t border-line">
      {g.file && !busy && (
        <ReviewPlayer g={g} review={review} onActive={setActive} seekRef={seekRef} toggleRef={toggleRef} />
      )}
      <div className="flex flex-wrap items-center justify-between gap-3 border-y border-line bg-surface-2/40 px-6 py-2.5">
        <h3 className="text-[13px] font-semibold">Script &amp; parts</h3>
        <div className="flex flex-wrap items-center gap-1">
          <button className={`btn btn-sm ${!only ? "bg-surface text-fg shadow-soft" : "btn-ghost"}`} aria-pressed={!only} onClick={() => setOnly(false)}>All {g.chunks.length}</button>
          <button className={`btn btn-sm ${only ? "bg-surface text-fg shadow-soft" : "btn-ghost"}`} aria-pressed={only} onClick={() => setOnly(true)}>Needs review {review.length}</button>
          <button className={`btn btn-sm ${keys ? "bg-surface text-fg shadow-soft" : "btn-ghost"}`} aria-pressed={keys}
            title="Space play/pause · N/P next/previous part needing review · A sounds fine · R re-speak · E edit words"
            onClick={() => { const v = !keys; setKeys(v); try { localStorage.setItem("tt-review-keys", v ? "1" : "0"); } catch { /* private mode */ } }}>
            <Keyboard size={13} /> Shortcuts {keys ? "on" : "off"}
          </button>
        </div>
      </div>
      {keys && (
        <p className="border-b border-line px-6 py-2 text-[12px] text-fg-2">
          <kbd className="kbd">Space</kbd> play/pause · <kbd className="kbd">N</kbd>/<kbd className="kbd">P</kbd> next/previous part needing review · <kbd className="kbd">A</kbd> sounds fine · <kbd className="kbd">R</kbd> re-speak · <kbd className="kbd">E</kbd> edit words
        </p>
      )}
      <div className="max-h-[640px] divide-y divide-line overflow-y-auto">
        {parts.map((c) => (
          <PartRow key={c.i} g={g} c={c} item={byPart.get(c.i)} span={span(c.i)} active={active === c.i} focused={focus === c.i} ready={ready} busy={busy}
            editing={editing === c.i} setEditing={(on) => setEditing(on ? c.i : null)}
            rowRef={(el) => { if (el) rows.current.set(c.i, el); else rows.current.delete(c.i); }}
            onJump={() => jump(c.i)} onAccept={() => accept(c.i)} onRespeak={(t) => respeak(c.i, t)}
            onUnaccept={async () => { await api.unaccept(g.id, c.i); refresh(); }} refresh={refresh} />
        ))}
        {!parts.length && <div className="flex items-center justify-center gap-2 p-8 text-[13px] text-fg-2"><CheckCircle2 size={15} className="text-ok-ink" /> Nothing needs review.</div>}
      </div>
    </section>
  );
}

function ReviewPlayer({ g, review, onActive, seekRef, toggleRef }: {
  g: Generation; review: ReviewItem[]; onActive: (i: number | null) => void;
  seekRef: React.MutableRefObject<(t: number, play?: boolean) => void>; toggleRef: React.MutableRefObject<() => void>;
}) {
  const el = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(false);
  const [t, setT] = useState(0);
  const [d, setD] = useState(0);
  const src = api.audioUrl(g.id, "wav", Math.round(g.audio_rev ?? g.duration ?? 0));
  const marks = review.filter((r) => r.start != null && r.end != null).map((r) => `${r.part}:${r.start}-${r.end}`).join(",");
  useEffect(() => {
    if (!el.current) return;
    const regions = RegionsPlugin.create();
    const w = WaveSurfer.create({ container: el.current, url: src, height: 56, barWidth: 2, barGap: 2, barRadius: 2, normalize: true,
      waveColor: cssVar("--wave"), progressColor: cssVar("--accent"), cursorWidth: 1, cursorColor: cssVar("--fg-3"), plugins: [regions] });
    const owner = {};
    const off = onClaim((o) => { if (o !== owner) w.pause(); });
    w.on("ready", () => {
      setD(w.getDuration());
      for (const r of review) {
        if (r.start == null || r.end == null) continue;
        regions.addRegion({ start: r.start, end: r.end, drag: false, resize: false, color: "rgba(201,122,6,0.22)", content: `Part ${r.part}` });
      }
    });
    w.on("timeupdate", (x) => { setT(x); const s = g.timeline?.find((p) => x >= p.start && x < p.end); onActive(s ? s.i : null); });
    w.on("play", () => { claimAudio(owner); setPlaying(true); });
    w.on("pause", () => setPlaying(false));
    w.on("finish", () => setPlaying(false));
    seekRef.current = (time, play = true) => { w.setTime(time); if (play) w.play(); };
    toggleRef.current = () => w.playPause();
    return () => { off(); w.destroy(); };
  }, [src, marks]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="flex items-center gap-4 px-6 py-4">
      <button className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-invert text-invert-fg transition hover:opacity-85"
        aria-label={playing ? "Pause narration" : "Play narration"} onClick={() => toggleRef.current()}>
        {playing ? <Pause size={16} fill="currentColor" /> : <Play size={16} fill="currentColor" className="ml-0.5" />}
      </button>
      <div className="min-w-0 flex-1">
        <div ref={el} className="w-full cursor-pointer" aria-label="Narration waveform. Shaded stretches need review." role="img" />
      </div>
      <span className="shrink-0 font-mono text-[12px] tabular-nums text-fg-2">{fmtTime(t)} / {fmtTime(d)}</span>
    </div>
  );
}

function PartRow({ g, c, item, span, active, focused, ready, busy, editing, setEditing, rowRef, onJump, onAccept, onRespeak, onUnaccept, refresh }: {
  g: Generation; c: Chunk; item?: ReviewItem; span?: { start: number; end: number }; active: boolean; focused: boolean; ready: boolean; busy: boolean;
  editing: boolean; setEditing: (on: boolean) => void; rowRef: (el: HTMLDivElement | null) => void;
  onJump: () => void; onAccept: () => void; onRespeak: (text?: string) => void; onUnaccept: () => void; refresh: () => void;
}) {
  const [text, setText] = useState(c.text);
  const [showTakes, setShowTakes] = useState(false);
  useEffect(() => setText(c.text), [c.text]);
  const takes = c.takes ?? [];
  const accepted = !!c.accepted;
  const tone = item ? "bg-warn/[0.06]" : active ? "bg-accent-soft/60" : "";
  const label = `Part ${c.i + 1}`;
  return (
    <div ref={rowRef} className={`px-6 py-4 transition ${tone} ${focused ? "outline outline-2 -outline-offset-2 outline-[var(--focus)]" : ""}`}>
      <div className="flex items-start gap-4">
        <div className="w-14 shrink-0 pt-0.5 font-mono text-[12px] leading-tight text-fg-2">
          <div>{c.i + 1}</div>
          {span && <div className="text-fg-3">{fmtTime(span.start)}</div>}
        </div>
        <div className="min-w-0 flex-1">
          {editing ? (
            <>
              <label className="sr-only" htmlFor={`edit-${c.i}`}>Words for {label}</label>
              <textarea id={`edit-${c.i}`} className="field text-[13.5px]" rows={3} value={text} onChange={(e) => setText(e.target.value)} autoFocus />
            </>
          ) : (
            <button type="button" disabled={!span} onClick={onJump} className={`block w-full rounded text-left text-[14px] leading-relaxed ${c.file ? "text-fg" : "text-fg-3"} ${active ? "font-medium" : ""} disabled:cursor-default`}>
              {c.text}
            </button>
          )}

          {item && !editing && (
            <div className="mt-2.5 rounded-xl border border-warn/30 bg-surface px-3.5 py-3 text-[13px]">
              <div className="flex items-start gap-2 font-medium text-warn-ink"><AlertTriangle size={14} className="mt-0.5 shrink-0" />Needs review: {item.reasons.join(", ")}</div>
              <Measured c={c} item={item} />
              <div className="mt-3 flex flex-wrap items-center gap-2">
                <button className="btn btn-primary btn-sm" disabled={!ready} onClick={onAccept}><Check size={13} /> Sounds fine</button>
                <button className="btn btn-secondary btn-sm" disabled={!ready} onClick={() => onRespeak()}><RefreshCw size={13} /> Re-speak</button>
                <button className="btn btn-ghost btn-sm" disabled={!ready} onClick={() => setEditing(true)}><Pencil size={13} /> Edit words</button>
              </div>
            </div>
          )}

          {!item && !editing && (
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              <PartStatus c={c} />
              {accepted && <button className="btn btn-ghost btn-sm !h-6 !px-2" disabled={!ready} onClick={onUnaccept}><Undo2 size={12} /> Undo</button>}
            </div>
          )}

          {editing && (
            <div className="mt-2 flex gap-2">
              <button className="btn btn-primary btn-sm" disabled={busy || !text.trim()} onClick={() => { setEditing(false); onRespeak(text); }}><RefreshCw size={13} /> Re-speak with these words</button>
              <button className="btn btn-ghost btn-sm" onClick={() => { setEditing(false); setText(c.text); }}><X size={13} /> Cancel</button>
            </div>
          )}

          {takes.length > 0 && !editing && (
            <div className="mt-2.5">
              <button className="btn btn-ghost btn-sm -ml-2" aria-expanded={showTakes} onClick={() => setShowTakes(!showTakes)}>
                Compare takes ({takes.length + 1})
              </button>
              {showTakes && <TakeCompare g={g} c={c} ready={ready} refresh={refresh} />}
            </div>
          )}
        </div>
        {!item && !editing && c.file && (
          <div className="flex shrink-0 items-center gap-0.5">
            <button className="icon-btn" aria-label={`Re-speak ${label}`} title="Re-speak this part" disabled={!ready} onClick={() => onRespeak()}><RefreshCw size={14} /></button>
            <button className="icon-btn" aria-label={`Edit words of ${label}`} title="Edit words" disabled={!ready} onClick={() => setEditing(true)}><Pencil size={14} /></button>
          </div>
        )}
      </div>
    </div>
  );
}

function Measured({ c, item }: { c: Chunk; item: ReviewItem }) {
  const bits: React.ReactNode[] = [];
  if (item.issues.includes("off_pitch") && item.pitch_hz && item.voice_pitch_hz)
    bits.push(<li key="p">Pitch {Math.round(item.pitch_hz)} Hz, your voice is usually {Math.round(item.voice_pitch_hz)} Hz.</li>);
  if (c.sim != null && item.issues.includes("voice_drift")) bits.push(<li key="s">Sounds {Math.min(100, Math.round(c.sim * 100))}% like your voice.</li>);
  if (item.heard && (item.issues.some((i) => ["wrong_words", "mispronounced", "end_word_missing", "start_word_missing", "leak"].includes(i)) || norm(item.heard) !== norm(item.text)))
    bits.push(<li key="h"><span className="text-fg-2">Heard:</span> <HeardDiff text={item.text} heard={item.heard} /></li>);
  if (item.start != null && item.end != null) bits.push(<li key="t">At {fmtTime(item.start)} to {fmtTime(item.end)} in the narration.</li>);
  return bits.length ? <ul className="mt-1.5 space-y-0.5 pl-6 text-[12.5px] text-fg-2">{bits}</ul> : null;
}

const norm = (s: string) => s.toLowerCase().replace(/[^\p{L}\p{N}\s']/gu, "").replace(/\s+/g, " ").trim();

/** What the checker heard, with words that aren't in the script marked. */
function HeardDiff({ text, heard }: { text: string; heard: string }) {
  const want = new Set(norm(text).split(" "));
  return <>{heard.split(/\s+/).map((w, i) => {
    const off = w && !want.has(norm(w));
    return <span key={i}>{i ? " " : ""}{off ? <mark className="rounded bg-warn/20 px-0.5 text-warn-ink">{w}</mark> : w}</span>;
  })}</>;
}

function PartStatus({ c }: { c: Chunk }) {
  if (c.accepted) return <span className="chip bg-ok/12 text-ok-ink"><Check size={11} /> Sounds fine (you listened)</span>;
  if (c.status === "ok") return <span className="chip bg-ok/12 text-ok-ink"><CheckCircle2 size={11} /> Checks passed</span>;
  if (c.status === "flagged") return <span className="chip bg-warn/15 text-warn-ink"><AlertTriangle size={11} /> Needs review</span>;
  if (c.status === "running") return <span className="chip bg-accent-soft text-ember-ink"><Loader2 size={11} className="animate-spin" /> Speaking</span>;
  return <span className="chip bg-surface-2 text-fg-2">Waiting</span>;
}

/** Take 1 vs Take 2: hear each take of one part and keep either. */
function TakeCompare({ g, c, ready, refresh }: { g: Generation; c: Chunk; ready: boolean; refresh: () => void }) {
  const toast = useToast();
  const kept = c.take ?? 1;
  const all = [...(c.takes ?? []).map((t) => ({ ...t, kept: false })), { take: kept, text: c.text, issues: [...(c.issues ?? []), ...(c.final_issues ?? [])], sim: c.sim, kept: true }]
    .sort((a, b) => a.take - b.take);
  const keep = async (k: number) => {
    try { await api.selectTake(g.id, c.i, k); toast(`Keeping take ${k} of part ${c.i + 1}. Re-mixing the narration…`, "info"); refresh(); }
    catch (e) { toast((e as Error).message, "error"); }
  };
  return (
    <div className="mt-2 grid gap-2 md:grid-cols-2">
      {all.map((t) => (
        <div key={t.take} className={`rounded-xl border p-3 ${t.kept ? "border-[var(--focus)] bg-surface" : "border-line bg-surface-2/50"}`}>
          <div className="mb-2 flex items-center justify-between gap-2 text-[12.5px]">
            <span className="font-semibold">Take {t.take}{t.kept ? " · kept" : ""}</span>
            {t.issues?.length ? <span className="text-warn-ink">{t.issues.map((i) => ISSUE_LABEL[i] ?? i).join(", ")}</span> : <span className="text-ok-ink">Checks passed</span>}
          </div>
          <Player src={api.takeAudio(g.id, c.i, t.take, t.kept ? c.audio_revision : t.archived && Math.round(t.archived))} height={28} compact />
          {t.text !== c.text && <p className="mt-1.5 text-[12px] text-fg-2">Words: “{t.text}”</p>}
          {!t.kept && <button className="btn btn-secondary btn-sm mt-2" disabled={!ready} onClick={() => keep(t.take)}>Keep take {t.take}</button>}
        </div>
      ))}
    </div>
  );
}
