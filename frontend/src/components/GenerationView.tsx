import { useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, ChevronDown, Download, Gauge, Info, Loader2, Pencil, Play, RefreshCw, RotateCcw, Square, Star, X } from "lucide-react";
import { api, fmtDuration, fmtTime, ISSUE_LABEL, type Chunk, type Generation } from "../api";
import VoiceOrb from "../lib/orb";
import { usePlayer } from "../lib/player";
import Player from "./Player";
import { InlineEdit, Progress, useOutside, useToast } from "./ui";

export default function GenerationView({ id, bare = false }: { id: string; bare?: boolean }) {
  const [g, setG] = useState<Generation | null>(null);
  const [filter, setFilter] = useState<"all" | "flagged">("all");
  const [open, setOpen] = useState<number | null>(null);
  const [poke, setPoke] = useState(0);
  const player = usePlayer();
  const toast = useToast();
  const wasRunning = useRef(false);
  const [fixing, setFixing] = useState(false);

  useEffect(() => {
    let alive = true;
    let t: number;
    let fails = 0;
    const tick = async () => {
      // A single failed request (a restart, a busy moment) must not freeze progress: retry with backoff.
      let x: Generation | null = null;
      try { x = await api.generation(id); fails = 0; } catch { fails++; }
      if (!alive) return;
      if (!x) {
        if (fails < 30) t = window.setTimeout(tick, Math.min(10000, 1000 * 2 ** Math.min(fails, 4)));
        return;
      }
      setG(x);
      const running = !!x.job || x.status === "running";
      if (wasRunning.current && !running && x.status === "done") toast(x.flagged?.length ? `Finished · ${x.flagged.length} part(s) to review` : "Narration ready. Every part passed.");
      wasRunning.current = running;
      if (running) t = window.setTimeout(tick, 1000);
    };
    tick();
    return () => { alive = false; clearTimeout(t); };
  }, [id, poke, toast]);

  // The player follows the audio, not the metadata: favouriting or renaming must not restart it.
  const audioRev = (x: Generation) => Math.round(x.audio_rev ?? x.duration ?? 0);

  // After a part is regenerated, swap the player bar to the new mix (without auto-playing).
  useEffect(() => {
    if (!g?.file || !player.track) return;
    const mine = `/api/generations/${g.id}/audio`;
    const fresh = api.audioUrl(g.id, "wav", audioRev(g));
    if (player.track.src.includes(mine) && player.track.src !== fresh && !g.job) {
      player.play({ ...player.track, src: fresh, autoplay: false });
      toast("Player updated with the new version", "info");
    }
  }, [g?.audio_rev, g?.duration, g?.job]); // eslint-disable-line react-hooks/exhaustive-deps

  if (!g) return <div className="card flex h-40 items-center justify-center"><Loader2 className="animate-spin text-fg-3" /></div>;

  const running = !!g.job || g.status === "running";
  const done = g.chunks.filter((c) => c.status === "ok" || c.status === "flagged").length;
  const flagged = g.chunks.filter((c) => c.status === "flagged");
  const review = g.review ?? [];
  const v = audioRev(g);
  const chunks = filter === "flagged" ? flagged : g.chunks;
  const playFull = () => player.play({ src: api.audioUrl(g.id, "wav", v), title: g.title || "Narration", subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" });
  const refresh = () => setPoke((n) => n + 1);
  const fixRemaining = async () => {
    setFixing(true);
    try { await api.fixRemaining(g.id); toast("Re-recording the remaining parts…", "info"); refresh(); }
    catch (e) { toast((e as Error).message, "error"); }
    finally { setFixing(false); }
  };

  const waiting = g.job?.status === "queued";
  const stopping = !!g.job?.cancel;
  return (
    <div className={bare ? "" : "card overflow-hidden shadow-soft"}>
      {/* header */}
      <div className="relative p-6">
        <div className="flex items-start gap-4">
          <VoiceOrb seed={g.profile_id} size={48} active={running} />
          <div className="min-w-0 flex-1">
            <InlineEdit value={g.title || "Untitled narration"} inputClass="text-[17px] font-semibold" onSave={async (t) => { await api.patchGeneration(g.id, { title: t }); refresh(); }} />
            <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[12.5px] text-fg-3">
              <span>{g.profile_name}</span>·<span>{g.engine}</span>
              {g.status === "done" && <>·<span>{fmtTime(g.duration)}</span>{g.speed && g.speed !== 1 ? <>·<span>{g.speed}×</span></> : null}</>}
              {g.outcome === "needs_listen" && <>·<span className="font-medium text-warn">Done, needs a listen</span></>}
              {g.outcome === "clean" && <>·<span className="text-ok">Done</span></>}
              {g.voice_settings && <>·<span title="Stability · Similarity · Style">stab {Math.round(g.voice_settings.stability * 100)} · sim {Math.round(g.voice_settings.similarity * 100)} · style {Math.round(g.voice_settings.style * 100)}{g.mic_eq ? " · mic matched" : ""}</span></>}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            <button className="icon-btn" title={g.favorite ? "Unfavorite" : "Favorite"} onClick={async () => { await api.patchGeneration(g.id, { favorite: !g.favorite }); refresh(); }}>
              <Star size={16} className={g.favorite ? "fill-accent text-accent" : ""} />
            </button>
            {running && <button className="btn btn-secondary btn-sm" onClick={() => api.cancel(g.id)}><Square size={12} /> Stop</button>}
            {!running && ["cancelled", "interrupted", "error"].includes(g.status) && (g.chunks.some((c) => c.status === "pending") || !g.file) && (
              <button className="btn btn-secondary btn-sm" onClick={async () => { await api.resume(g.id); refresh(); }}><RotateCcw size={13} /> Resume</button>
            )}
            {g.file && !running && <SpeedMenu g={g} onDone={refresh} />}
            {g.file && !running && <DownloadMenu g={g} />}
            {g.file && !running && <button className="btn btn-primary btn-sm" onClick={playFull}><Play size={13} fill="currentColor" /> Play</button>}
          </div>
        </div>

        {running && (
          <div className="mt-5">
            <div className="mb-2 flex items-center justify-between text-[12.5px]">
              <span className="flex items-center gap-2 text-fg-2"><Loader2 size={13} className="animate-spin text-accent" />
                {stopping
                  ? "Stopping after the current take…"
                  : waiting
                  ? `Waiting its turn${g.queue?.ahead ? ` · ${g.queue.ahead} ahead: ${g.queue.waiting_for.join(", ")}` : ""}`
                  : g.message}
              </span>
              {!waiting && <span className="font-mono text-fg-3">{done}/{g.chunks.length}{g.eta ? ` · ~${fmtDuration(g.eta)} left` : ""}</span>}
            </div>
            <Progress value={waiting ? 0 : done / g.chunks.length} />
            <p className="mt-2 text-[12px] text-fg-3">
              {waiting ? "It starts automatically when the narration ahead finishes. Stop removes it from the queue." : "You can listen to finished parts below while the rest renders."}
            </p>
          </div>
        )}
        {running && g.duplicate_notice && (
          <div className="mt-4 flex items-start gap-2 rounded-xl bg-accent-soft px-4 py-2.5 text-[12.5px] text-fg-2">
            <Info size={13} className="mt-0.5 shrink-0 text-accent" />
            <span>
              Already in the queue. You tried to start this same narration again {g.duplicate_notice.count === 1 ? "once" : `${g.duplicate_notice.count} times`}, so it wasn't added twice.
            </span>
          </div>
        )}
        {!running && g.status === "error" && <div className="mt-4 rounded-xl bg-danger/10 px-4 py-3 text-[13px] text-danger">{g.message}</div>}
        {!running && g.status === "done" && (
          <div className="mt-5 grid grid-cols-2 gap-3 md:grid-cols-4">
            <Metric label="Length" value={fmtTime(g.duration)} />
            <Metric label="Parts" value={String(g.chunks.length)} />
            <Metric label="Auto re-takes" value={String(g.retries ?? 0)} />
            <Metric label="Needs a listen" value={String(review.length)} tone={review.length ? "warn" : "ok"} />
          </div>
        )}
        {!running && g.status === "done" && review.length > 0 && (
          <div className="mt-3 rounded-xl bg-warn/10 px-4 py-3 text-[12.5px] text-warn">
            <div className="flex flex-wrap items-center gap-3">
              <AlertTriangle size={14} className="shrink-0" />
              <span className="min-w-0 flex-1 font-medium">
                Done, but {review.length === 1 ? "1 part needs" : `${review.length} parts need`} a listen. The checks found a possible problem and couldn't fix it.
              </span>
              <button className="btn btn-primary btn-sm" disabled={fixing} onClick={fixRemaining}>
                {fixing ? <Loader2 size={13} className="animate-spin" /> : <RefreshCw size={13} />} Fix remaining errors ({review.length})
              </button>
            </div>
            <ul className="mt-2 space-y-1 pl-6 text-fg-2">
              {review.map((r) => (
                <li key={r.part}><span className="font-medium text-warn">Part {r.part}</span> · {r.reasons.join(", ")} · <span className="text-fg-3">“{r.text.length > 90 ? r.text.slice(0, 90) + "…" : r.text}”</span></li>
              ))}
            </ul>
          </div>
        )}
        {!running && g.status === "done" && g.final_scan && review.length === 0 && (
          <div className="mt-3 flex items-center gap-2 text-[12.5px] text-fg-3">
            <CheckCircle2 size={13} className="text-ok" />
            Automated checks completed for {g.final_scan.checked} parts
            {g.final_scan.fixed.length ? ` and re-recorded ${g.final_scan.fixed.length} with detected issues (part ${g.final_scan.fixed.map((i) => i + 1).join(", ")}).` : ": no issues detected."}
            {" Listen through to confirm every word is complete."}
          </div>
        )}
      </div>

      {/* script */}
      {g.text && (
        <div className="border-t border-line px-6 py-4">
          <div className="eyebrow mb-2">Script</div>
          <div className="max-h-72 overflow-y-auto whitespace-pre-wrap text-[13.5px] leading-relaxed text-fg-2">{g.text}</div>
        </div>
      )}

      {/* parts */}
      <div className="flex items-center justify-between border-y border-line bg-surface-2/40 px-6 py-2.5">
        <span className="eyebrow">Parts</span>
        <div className="flex gap-1">
          {(["all", "flagged"] as const).map((f) => (
            <button key={f} onClick={() => setFilter(f)} className={`btn btn-sm ${filter === f ? "bg-surface text-fg shadow-soft" : "btn-ghost"}`}>
              {f === "all" ? `All ${g.chunks.length}` : `Needs review ${flagged.length}`}
            </button>
          ))}
        </div>
      </div>
      <div className="max-h-[600px] divide-y divide-line overflow-y-auto">
        {chunks.map((c) => (
          <ChunkRow key={c.i} g={g} c={c} open={open === c.i} busy={running} onToggle={() => setOpen(open === c.i ? null : c.i)}
            onRegen={async (text) => { try { await api.regenerate(g.id, c.i, text); refresh(); } catch (e) { toast((e as Error).message, "error"); } }} />
        ))}
        {!chunks.length && <div className="flex items-center justify-center gap-2 p-8 text-[13px] text-fg-3"><CheckCircle2 size={15} className="text-ok" /> Nothing to review. Every part passed.</div>}
      </div>
    </div>
  );
}

function Metric({ label, value, tone }: { label: string; value: string; tone?: "ok" | "warn" }) {
  return (
    <div className="rounded-xl bg-surface-2 px-3.5 py-2.5">
      <div className="text-[11.5px] text-fg-3">{label}</div>
      <div className={`mt-0.5 text-[16px] font-semibold tabular-nums ${tone === "warn" ? "text-warn" : tone === "ok" ? "text-ok" : ""}`}>{value}</div>
    </div>
  );
}

function Menu({ label, icon, children }: { label: string; icon: React.ReactNode; children: (close: () => void) => React.ReactNode }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useOutside(ref, () => setOpen(false));
  return (
    <div ref={ref} className="relative">
      <button className="btn btn-secondary btn-sm" onClick={() => setOpen((o) => !o)}>{icon}{label}<ChevronDown size={13} /></button>
      {open && <div className="animate-rise absolute right-0 z-40 mt-1.5 w-56 rounded-xl border border-line-strong bg-surface p-1.5 shadow-float">{children(() => setOpen(false))}</div>}
    </div>
  );
}

function DownloadMenu({ g }: { g: Generation }) {
  const unresolved = g.review?.length ?? 0;
  const items = [
    { href: api.audioUrl(g.id, "wav") + "&download=true", label: "Audio · WAV", hint: "Lossless, for editing" },
    { href: api.audioUrl(g.id, "mp3") + "&download=true", label: "Audio · MP3", hint: "192 kbps" },
    { href: api.subtitlesUrl(g.id, "srt"), label: "Subtitles · SRT", hint: "YouTube / Premiere / CapCut" },
    { href: api.subtitlesUrl(g.id, "vtt"), label: "Subtitles · VTT", hint: "Web players" },
    { href: api.subtitlesUrl(g.id, "txt"), label: "Script · TXT", hint: "Cleaned text as spoken" },
  ];
  return (
    <Menu label="Export" icon={<Download size={13} />}>
      {(close) => (
        <>
          {unresolved > 0 && (
            <div className="mb-1 flex items-start gap-2 rounded-lg bg-warn/10 px-2.5 py-2 text-[11.5px] text-warn">
              <AlertTriangle size={12} className="mt-0.5 shrink-0" />
              <span>{unresolved} part{unresolved > 1 ? "s" : ""} still {unresolved > 1 ? "have" : "has"} a cut-off or stray ending. The export includes {unresolved > 1 ? "them" : "it"} as they are.</span>
            </div>
          )}
          {items.map((i) => (
            <a key={i.label} href={i.href} onClick={close} className="block rounded-lg px-2.5 py-2 transition hover:bg-surface-2">
              <div className="text-[13px]">{i.label}</div>
              <div className="text-[11.5px] text-fg-3">{i.hint}</div>
            </a>
          ))}
        </>
      )}
    </Menu>
  );
}


function SpeedMenu({ g, onDone }: { g: Generation; onDone: () => void }) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const cur = g.speed ?? 1;
  return (
    <Menu label={busy ? "…" : `${cur}×`} icon={<Gauge size={13} />}>
      {(close) => (
        <>
          <div className="px-2.5 pb-1.5 pt-1 text-[11.5px] text-fg-3">Re-time without re-rendering (pitch preserved)</div>
          {[0.85, 0.9, 0.95, 1, 1.05, 1.1, 1.15, 1.2].map((s) => (
            <button key={s} disabled={busy} className={`flex w-full items-center justify-between rounded-lg px-2.5 py-1.5 text-[13px] hover:bg-surface-2 ${s === cur ? "text-accent" : ""}`}
              onClick={async () => {
                close(); setBusy(true);
                try { await api.patchGeneration(g.id, { speed: s }); toast(`Speed set to ${s}×`); onDone(); } catch (e) { toast((e as Error).message, "error"); } finally { setBusy(false); }
              }}>
              {s}× {s === cur && <CheckCircle2 size={13} />}
            </button>
          ))}
        </>
      )}
    </Menu>
  );
}

function ChunkRow({ g, c, open, busy, onToggle, onRegen }: { g: Generation; c: Chunk; open: boolean; busy: boolean; onToggle: () => void; onRegen: (t?: string) => void }) {
  const [edit, setEdit] = useState(false);
  const [text, setText] = useState(c.text);
  useEffect(() => setText(c.text), [c.text]);
  return (
    <div className={`px-6 py-3.5 transition ${c.status === "flagged" ? "bg-warn/[0.05]" : ""} ${open ? "bg-surface-2/40" : ""}`}>
      <div className="flex items-start gap-4">
        <div className="w-6 shrink-0 pt-0.5 text-right font-mono text-[11.5px] text-fg-3">{c.i + 1}</div>
        <div className="min-w-0 flex-1">
          {edit ? (
            <textarea className="field text-[13.5px]" rows={3} value={text} onChange={(e) => setText(e.target.value)} autoFocus />
          ) : (
            <p className={`text-[13.5px] leading-relaxed ${c.file ? "cursor-pointer text-fg" : "text-fg-3"}`} onClick={() => c.file && onToggle()}>{c.text}</p>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            <Status c={c} />
            {Array.from(new Set([...(c.issues ?? []), ...(c.final_issues ?? [])])).map((i) => <span key={i} className="chip bg-warn/15 text-warn">{ISSUE_LABEL[i] ?? i}</span>)}
            {c.wer != null && <span className="chip bg-surface-2 text-fg-2" title="Whisper transcript match. A recognized word can still have an incomplete consonant.">transcript {Math.round((1 - Math.min(1, c.wer)) * 100)}%</span>}
            {c.sim != null && <span className="chip bg-surface-2 text-fg-2" title="Match against your voice fingerprint">voice {Math.min(100, Math.round(c.sim * 100))}%</span>}
            {c.attempts && c.attempts.length > 1 && <span className="chip bg-surface-2 text-fg-3">{c.attempts.length} takes</span>}
          </div>
          {open && c.file && (
            <div className="mt-3 space-y-2">
              {/* Progress on other parts must not destroy an actively playing clip.
                  Legacy recordings use their take identity until re-rendered. */}
              <Player src={api.chunkUrl(g.id, c.i, c.audio_revision ?? c.attempts?.map((a) => `${a.seed}-${a.secs}`).join("_") ?? 0)} height={32} compact />
              {c.heard && <div className="text-[12px] text-fg-3"><span className="text-fg-2">Heard:</span> {c.heard}</div>}
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          {edit ? (
            <>
              <button className="btn btn-ghost btn-sm" onClick={() => { setEdit(false); setText(c.text); }}><X size={13} /> Cancel</button>
              <button className="btn btn-primary btn-sm" disabled={busy} onClick={() => { setEdit(false); onRegen(text); }}><RefreshCw size={13} /> Re-speak</button>
            </>
          ) : (
            <>
              {c.file && <button className="icon-btn" title="Listen" onClick={onToggle}><Play size={14} /></button>}
              <button className="icon-btn" title="Edit text & re-speak" disabled={busy} onClick={() => setEdit(true)}><Pencil size={14} /></button>
              <button className="icon-btn" title="Regenerate this part" disabled={busy || c.status === "pending" || c.status === "running"} onClick={() => onRegen()}><RefreshCw size={14} /></button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Status({ c }: { c: Chunk }) {
  if (c.status === "ok") return <span className="chip bg-ok/12 text-ok"><CheckCircle2 size={11} /> checks passed</span>;
  if (c.status === "flagged") return <span className="chip bg-warn/15 text-warn"><AlertTriangle size={11} /> review</span>;
  if (c.status === "running") return <span className="chip shimmer bg-accent-soft text-accent"><Loader2 size={11} className="animate-spin" /> speaking</span>;
  return <span className="chip bg-surface-2 text-fg-3">queued</span>;
}
