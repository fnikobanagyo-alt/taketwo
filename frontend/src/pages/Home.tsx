import { useEffect, useState } from "react";
import { AlertTriangle, ArrowRight, FileText, Mic2, Play, RotateCcw } from "lucide-react";
import { api, fmtDuration, timeAgo, type Generation, type Profile } from "../api";
import VoiceOrb from "../lib/orb";
import { usePlayer } from "../lib/player";
import { DRAFT_KEY, DRAFT_TITLE_KEY, groupByScript } from "../lib/groups";
import { go } from "../App";
import VoicePreview from "../components/VoicePreview";
import { SourcePill, StatusPill } from "../components/StatusPill";
import { Empty } from "../components/ui";

function greeting() {
  const h = new Date().getHours();
  return h < 5 ? "Working late" : h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
}

const readDraft = () => { try { return { text: localStorage.getItem(DRAFT_KEY) ?? "", title: localStorage.getItem(DRAFT_TITLE_KEY) ?? "" }; } catch { return { text: "", title: "" }; } };

/** Studio desk: what's in progress, what needs review, recent scripts and voices. */
export default function HomePage() {
  const [gens, setGens] = useState<Generation[] | null>(null);
  const [voices, setVoices] = useState<Profile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const player = usePlayer();
  const load = () => {
    setError(null);
    Promise.all([api.generations(), api.profiles()])
      .then(([g, p]) => { setGens(g); setVoices(p.filter((x) => x.status === "ready")); })
      .catch((e) => setError((e as Error).message));
  };
  useEffect(load, []);

  if (error)
    return (
      <div className="mx-auto max-w-6xl px-10 pb-16 pt-8">
        <Empty icon={<AlertTriangle size={20} />} title="TakeTwo's engine isn't answering" body={`${error}. It may still be starting, or it was closed.`}
          action={<button className="btn btn-secondary btn-sm" onClick={load}><RotateCcw size={13} /> Try again</button>} />
      </div>
    );
  if (!gens || !voices) return <div className="mx-auto max-w-6xl px-10 pt-8 text-[13px] text-fg-2" role="status">Loading…</div>;

  if (!voices.length)
    return (
      <div className="mx-auto max-w-3xl px-10 pb-16 pt-16">
        <h1 className="display-home text-[44px] leading-[1.05]">{greeting()}. <em>Let's clone your voice.</em></h1>
        <p className="mt-4 max-w-xl text-[15px] leading-relaxed text-fg-2">
          TakeTwo learns a voice from 30 seconds to 5 minutes of one person speaking, then reads your scripts in it and checks every part. Everything stays on this computer.
        </p>
        <div className="mt-8 flex flex-wrap gap-3">
          <button className="btn btn-primary btn-lg" onClick={() => go({ page: "voices", id: "new" })}><Mic2 size={17} /> Clone a voice</button>
          <button className="btn btn-secondary btn-lg" onClick={() => go({ page: "design" })}>Design one from a description</button>
        </div>
      </div>
    );

  const groups = groupByScript(gens);
  const review = groups.filter((g) => g.latest.outcome === "needs_listen");
  const running = gens.find((g) => g.status === "running" || g.status === "queued");
  const draft = readDraft();
  const parts = review.reduce((n, g) => n + (g.latest.review_count ?? 0), 0);
  const summary = running ? `“${running.title || "A narration"}” is being spoken.` : review.length ? `${review.length} script${review.length > 1 ? "s" : ""} need${review.length > 1 ? "" : "s"} review.` : "Everything you've made passed the checks.";
  const playG = (g: Generation) => player.play({ src: api.audioUrl(g.id, "wav", Math.round(g.updated)), title: g.title || g.profile_name, subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" });

  return (
    <div className="mx-auto max-w-6xl px-10 pb-16 pt-8">
      <header className="mb-6">
        <h1 className="display-home text-[34px] leading-tight">{greeting()}. <em>{summary}</em></h1>
      </header>

      <div className="mb-10 grid gap-4 md:grid-cols-2">
        {draft.text.trim() ? (
          <button className="card flex items-start gap-4 p-5 text-left transition hover:border-line-strong" onClick={() => go({ page: "speech" })}>
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-surface-2 text-fg-2"><FileText size={18} /></span>
            <span className="min-w-0 flex-1">
              <span className="block text-[14px] font-medium">Resume your draft</span>
              <span className="mt-0.5 block truncate text-[13px] text-fg-2">{draft.title || draft.text.slice(0, 90)}</span>
              <span className="mt-1 block text-[12px] text-fg-3">{draft.text.trim().split(/\s+/).length.toLocaleString()} words</span>
            </span>
            <ArrowRight size={16} className="mt-1 shrink-0 text-fg-3" />
          </button>
        ) : (
          <button className="card flex items-start gap-4 p-5 text-left transition hover:border-line-strong" onClick={() => go({ page: "speech" })}>
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-surface-2 text-fg-2"><FileText size={18} /></span>
            <span className="min-w-0 flex-1">
              <span className="block text-[14px] font-medium">No draft open</span>
              <span className="mt-0.5 block text-[13px] text-fg-2">Paste a script in Text to Speech, up to 10,000 words.</span>
            </span>
            <ArrowRight size={16} className="mt-1 shrink-0 text-fg-3" />
          </button>
        )}
        {review.length ? (
          <button className="card flex items-start gap-4 border-warn/40 p-5 text-left transition hover:border-warn" onClick={() => go({ page: "history", id: review[0].latest.id })}>
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-warn/15 text-warn-ink"><AlertTriangle size={18} /></span>
            <span className="min-w-0 flex-1">
              <span className="block text-[14px] font-medium">Needs review: {parts} part{parts === 1 ? "" : "s"} in {review.length} script{review.length > 1 ? "s" : ""}</span>
              <span className="mt-0.5 block truncate text-[13px] text-fg-2">Start with “{review[0].latest.title || review[0].latest.text_preview}”</span>
            </span>
            <ArrowRight size={16} className="mt-1 shrink-0 text-fg-3" />
          </button>
        ) : (
          <div className="card flex items-start gap-4 p-5">
            <span className="grid h-10 w-10 shrink-0 place-items-center rounded-xl bg-ok/12 text-ok-ink"><FileText size={18} /></span>
            <span><span className="block text-[14px] font-medium">Nothing needs review</span><span className="mt-0.5 block text-[13px] text-fg-2">New narrations show here when a part needs your ear.</span></span>
          </div>
        )}
      </div>

      <div className="grid grid-cols-1 gap-10 lg:grid-cols-5">
        <section className="lg:col-span-3" aria-labelledby="recent-h">
          <div className="mb-3 flex items-center justify-between">
            <h2 id="recent-h" className="text-[15px] font-semibold">Recent scripts</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => go({ page: "history" })}>All of History</button>
          </div>
          <ul className="card divide-y divide-line">
            {groups.length === 0 && <li className="p-6 text-[13px] text-fg-2">Your narrations will appear here.</li>}
            {groups.slice(0, 7).map(({ key, latest: g, takes }) => (
              <li key={key} className="flex items-center gap-3 px-4 py-3">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13.5px] font-medium">{g.title || g.text_preview}</div>
                  <div className="mt-1 flex flex-wrap items-center gap-1.5 text-[12px] text-fg-2">
                    <StatusPill g={g} />
                    {takes.length > 1 && <span className="chip bg-surface-2 text-fg-2">{takes.length} takes</span>}
                    <SourcePill g={g} />
                    <span>{g.profile_name} · {timeAgo(g.created)}{g.duration ? ` · ${fmtDuration(g.duration)}` : ""}</span>
                  </div>
                </div>
                <button className="btn btn-secondary btn-sm" disabled={g.status !== "done"} aria-label={`Play ${g.title || "narration"}`} onClick={() => playG(g)}><Play size={13} fill="currentColor" /> Play</button>
                <button className="btn btn-ghost btn-sm" aria-label={`Review ${g.title || "narration"}`} onClick={() => go({ page: "history", id: g.id })}>Review</button>
              </li>
            ))}
          </ul>
        </section>
        <section className="lg:col-span-2" aria-labelledby="voices-h">
          <div className="mb-3 flex items-center justify-between">
            <h2 id="voices-h" className="text-[15px] font-semibold">Your voices</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => go({ page: "voices" })}>Manage</button>
          </div>
          <ul className="card divide-y divide-line">
            {voices.slice(0, 6).map((v) => (
              <li key={v.id} className="flex items-center gap-3 px-4 py-2.5">
                <VoiceOrb seed={v.id} size={30} />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[13.5px]">{v.name}</div>
                  <div className="truncate text-[12px] text-fg-2">{v.description || `${Math.round(v.usable_speech_seconds ?? 0)}s of clean speech`}</div>
                </div>
                <VoicePreview profile={v} size={28} />
                <button className="btn btn-ghost btn-sm" aria-label={`Use ${v.name}`} onClick={() => go({ page: "speech", id: v.id })}>Use</button>
              </li>
            ))}
          </ul>
        </section>
      </div>
    </div>
  );
}
