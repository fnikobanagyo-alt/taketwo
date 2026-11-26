import { useEffect, useState } from "react";
import { ArrowUpRight, AudioLines, Captions, Mic2, Play, Wand2 } from "lucide-react";
import { api, fmtDuration, timeAgo, type Generation, type Profile } from "../api";
import VoiceOrb from "../lib/orb";
import { usePlayer } from "../lib/player";
import { go } from "../App";
import VoicePreview from "../components/VoicePreview";

function greeting() {
  const h = new Date().getHours();
  return h < 5 ? "Working late" : h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
}

const ACTIONS = [
  { page: "speech", title: "Text to Speech", body: "Turn a full script into narration in your voice.", icon: AudioLines },
  { page: "voices", id: "new", title: "Clone a voice", body: "Up to 5 minutes of audio, analyzed end to end.", icon: Mic2 },
  { page: "design", title: "Design a voice", body: "Describe a voice in words and create it.", icon: Wand2 },
  { page: "transcribe", title: "Speech to Text", body: "Transcripts and SRT captions from any audio.", icon: Captions },
] as const;

export default function HomePage() {
  const [gens, setGens] = useState<Generation[]>([]);
  const [voices, setVoices] = useState<Profile[]>([]);
  const player = usePlayer();
  useEffect(() => {
    api.generations().then((g) => setGens(g.filter((x) => x.status === "done").slice(0, 6)));
    api.profiles().then((p) => setVoices(p.filter((x) => x.status === "ready")));
  }, []);

  return (
    <div className="mx-auto max-w-6xl px-10 pb-16 pt-8">
      <div className="relative mb-10 overflow-hidden rounded-3xl border border-line bg-surface px-10 py-12">
        <div className="pointer-events-none absolute inset-0" style={{ background: "radial-gradient(600px 420px at 88% 18%, rgba(255,150,70,.22), rgba(228,72,110,.08) 45%, transparent 75%)" }} />
        <div className="relative">
          <div className="eyebrow">TakeTwo Studio</div>
          <h1 className="display mt-3 text-[56px] leading-[1.02]">
            {greeting()}.<br /><em className="ember-text">What should we voice today?</em>
          </h1>
          <p className="mt-4 max-w-lg text-[14.5px] leading-relaxed text-fg-2">
            Scripts up to 10,000 words, cloned from your own recordings, checked line by line for gaps, glitches and voice drift. All on this Mac.
          </p>
          <div className="mt-7 flex gap-3">
            <button className="btn btn-ember ember btn-lg" onClick={() => go({ page: "speech" })}><AudioLines size={17} /> Start a narration</button>
            <button className="btn btn-secondary btn-lg" onClick={() => go({ page: "voices", id: "new" })}><Mic2 size={17} /> Clone a voice</button>
          </div>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {ACTIONS.map((a) => (
          <button key={a.title} onClick={() => go({ page: a.page, id: "id" in a ? a.id : undefined })} className="card group p-5 text-left transition hover:-translate-y-0.5 hover:border-line-strong hover:shadow-float">
            <div className="flex items-center justify-between">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-surface-2 text-fg-2 transition group-hover:bg-accent-soft group-hover:text-accent"><a.icon size={19} /></div>
              <ArrowUpRight size={16} className="text-fg-3 opacity-0 transition group-hover:opacity-100" />
            </div>
            <div className="mt-4 text-[14px] font-medium">{a.title}</div>
            <div className="mt-1 text-[12.5px] leading-relaxed text-fg-3">{a.body}</div>
          </button>
        ))}
      </div>

      <div className="mt-12 grid grid-cols-1 gap-10 lg:grid-cols-5">
        <section className="lg:col-span-3">
          <div className="mb-4 flex items-center justify-between">
            <h2 className="text-[15px] font-semibold">Recent narrations</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => go({ page: "history" })}>View all</button>
          </div>
          <div className="card divide-y divide-line">
            {gens.length === 0 && <div className="p-6 text-[13px] text-fg-3">Your finished narrations will appear here.</div>}
            {gens.map((g) => (
              <div key={g.id} className="group flex items-center gap-3.5 px-4 py-3">
                <button
                  className="relative"
                  onClick={() => player.play({ src: api.audioUrl(g.id, "wav", Math.round(g.updated)), title: g.title || g.profile_name, subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" })}
                >
                  <VoiceOrb seed={g.profile_id} size={36} />
                  <span className="absolute inset-0 flex items-center justify-center rounded-full bg-black/40 text-white opacity-0 transition group-hover:opacity-100"><Play size={14} fill="currentColor" /></span>
                </button>
                <button className="min-w-0 flex-1 text-left" onClick={() => go({ page: "history", id: g.id })}>
                  <div className="truncate text-[13.5px]">{g.title || g.text_preview}</div>
                  <div className="text-[12px] text-fg-3">{g.profile_name} · {timeAgo(g.created)}</div>
                </button>
                <span className="font-mono text-[11.5px] text-fg-3">{fmtDuration(g.duration)}</span>
              </div>
            ))}
          </div>
        </section>
        <section className="lg:col-span-2">
          <div className="mb-4 flex items-center justify-between">
            <h2 className="text-[15px] font-semibold">Your voices</h2>
            <button className="btn btn-ghost btn-sm" onClick={() => go({ page: "voices" })}>Manage</button>
          </div>
          <div className="card divide-y divide-line">
            {voices.length === 0 && <div className="p-6 text-[13px] text-fg-3">No voices yet.</div>}
            {voices.slice(0, 6).map((v) => (
              <button key={v.id} onClick={() => go({ page: "speech", id: v.id })} className="flex w-full items-center gap-3.5 px-4 py-3 text-left transition hover:bg-surface-2">
                <VoiceOrb seed={v.id} size={36} />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2"><span className="truncate text-[13.5px]">{v.name}</span><VoicePreview profile={v} size={22} /></div>
                  <div className="truncate text-[12px] text-fg-3">{v.description || `${Math.round(v.usable_speech_seconds ?? 0)}s of clean speech`}</div>
                </div>
                <span className="text-[12px] text-fg-3">Use →</span>
              </button>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
