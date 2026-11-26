import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, ChevronDown, FileUp, Info, Loader2, Mic2, Play, RotateCcw, Sparkles, Trash2 } from "lucide-react";
import { api, ApiError, DEFAULT_VS, fmtDuration, timeAgo, wordCount, type Engine, type Found, type Generation, type Profile, type Settings, type VoiceSettings } from "../api";
import VoiceOrb from "../lib/orb";
import { usePlayer } from "../lib/player";
import { Empty, Segmented, Select, Slider, Switch, useToast } from "../components/ui";
import GenerationView from "../components/GenerationView";
import VoicePreview from "../components/VoicePreview";
import { go } from "../App";

import { DRAFT_KEY, DRAFT_TITLE_KEY } from "../lib/groups";
const MAX_WORDS = 10_000;
// rough wall-clock seconds of rendering (incl. QC) per second of audio, per engine
const RENDER_FACTOR: Record<string, number> = { "qwen3-tts-1.7b": 1.7, "qwen3-tts-0.6b": 1.1, voxcpm2: 4.5, chatterbox: 1.4, "chatterbox-multilingual": 1.5, "fish-s2-pro": 3, omnivoice: 1.4 };

export default function SpeechPage({ id }: { id?: string }) {
  const toast = useToast();
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [engines, setEngines] = useState<Engine[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [profileId, setProfileId] = useState("");
  const [engineId, setEngineId] = useState("");
  const [text, setText] = useState(() => { try { return localStorage.getItem(DRAFT_KEY) ?? ""; } catch { return ""; } });
  const [title, setTitle] = useState(() => { try { return localStorage.getItem(DRAFT_TITLE_KEY) ?? ""; } catch { return ""; } });
  const [showTone, setShowTone] = useState(false);
  const [already, setAlready] = useState<Found | null>(null);
  const [params, setParams] = useState<Record<string, number>>({});
  const [speed, setSpeed] = useState(1);
  const [vs, setVs] = useState<VoiceSettings>(DEFAULT_VS);
  const [showAdv, setShowAdv] = useState(false);
  const [genId, setGenId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [tab, setTab] = useState<"settings" | "history">("settings");
  const [drag, setDrag] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const resultRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    Promise.all([api.profiles(), api.engines(), api.settings()]).then(([p, e, s]) => {
      const ready = p.filter((x) => x.status === "ready");
      setProfiles(ready);
      setEngines(e.filter((x) => x.kind !== "design"));
      setSettings(s);
      const pick = ready.find((x) => x.id === id) ?? ready[0];
      if (pick) setProfileId(pick.id);
      setEngineId(pick?.default_engine || s.default_engine);
    });
  }, [id]);
  useEffect(() => { try { localStorage.setItem(DRAFT_KEY, text); } catch { /* ignore */ } }, [text]);
  useEffect(() => { try { localStorage.setItem(DRAFT_TITLE_KEY, title); } catch { /* ignore */ } }, [title]);
  // Same script already narrated in this voice? Say so before a second render is started.
  useEffect(() => {
    setAlready(null);
    if (!profileId || wordCount(text) < 3) return;
    const t = setTimeout(() => {
      api.findNarrations({ same_script: text, voice: profileId, limit: 1 }).then((r) => setAlready(r[0] ?? null)).catch(() => {});
    }, 600);
    return () => clearTimeout(t);
  }, [text, profileId]);
  useEffect(() => setParams({}), [engineId]);
  // voice settings are remembered per voice, like ElevenLabs
  useEffect(() => {
    if (!profileId) return;
    try { setVs({ ...DEFAULT_VS, ...JSON.parse(localStorage.getItem(`vf-vs-${profileId}`) || "{}") }); } catch { setVs(DEFAULT_VS); }
  }, [profileId]);
  const updateVs = (patch: Partial<VoiceSettings>) => {
    const n = { ...vs, ...patch };
    setVs(n);
    try { localStorage.setItem(`vf-vs-${profileId}`, JSON.stringify(n)); } catch { /* ignore */ }
  };

  const engine = engines.find((e) => e.id === engineId);
  const profile = profiles?.find((p) => p.id === profileId);
  const words = useMemo(() => wordCount(text), [text]);
  const audioSec = words / (profile?.speech_rate_wps ?? 2.6) / speed * 1.12; // + pauses
  const renderSec = audioSec * (RENDER_FACTOR[engineId] ?? 2);
  const parts = Math.max(1, Math.ceil(text.length / (settings?.max_chunk_chars ?? 220)));
  const over = words > MAX_WORDS;

  const start = useCallback(async () => {
    if (!profile || !engine || !text.trim() || over) return;
    setBusy(true);
    try {
      const g = await api.generate({ profile_id: profile.id, text, title: title.trim() || undefined, engine: engine.id, language: profile.language, params, speed, voice_settings: vs });
      setGenId(g.id);
      setTimeout(() => resultRef.current?.scrollIntoView({ behavior: "smooth", block: "start" }), 100);
    } catch (e) {
      const dup = e instanceof ApiError && typeof e.detail === "object" && e.detail ? e.detail.duplicate_of : undefined;
      if (dup) {
        // Not added twice: show the copy that is already queued or rendering.
        setGenId(dup);
        toast("Already in the queue. It wasn't added twice.", "info");
        setTimeout(() => resultRef.current?.scrollIntoView({ behavior: "smooth", block: "start" }), 100);
      } else {
        toast((e as Error).message, "error");
      }
    } finally {
      setBusy(false);
    }
  }, [profile, engine, text, title, over, params, speed, vs, toast]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key === "Enter") { e.preventDefault(); start(); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [start]);

  const importFile = async (f: File) => {
    if (!/\.(txt|md|markdown|text|srt)$/i.test(f.name) && !f.type.startsWith("text/")) {
      toast("Import a .txt or .md file (for Word/Docs, copy and paste the text).", "error");
      return;
    }
    let t = await f.text();
    if (/\.srt$/i.test(f.name)) t = t.replace(/^\d+\s*$/gm, "").replace(/^[\d:,]+ --> [\d:,]+\s*$/gm, "").replace(/\n{2,}/g, "\n");
    const { text: fixed, joined } = unwrapLines(t.trim());
    setText(fixed);
    toast(`Imported ${f.name} · ${wordCount(fixed).toLocaleString()} words${joined ? ` · joined ${joined} broken lines` : ""}`);
  };

  if (profiles && !profiles.length)
    return (
      <div className="mx-auto max-w-xl px-10 py-24">
        <Empty icon={<Mic2 size={22} />} title="Clone a voice first" body="Upload up to 5 minutes of speech and TakeTwo will learn it, or design one from a description."
          action={<div className="flex gap-2"><button className="btn btn-primary" onClick={() => go({ page: "voices", id: "new" })}>Clone a voice</button><button className="btn btn-secondary" onClick={() => go({ page: "design" })}>Design a voice</button></div>} />
      </div>
    );

  const voiceOptions = (profiles ?? []).map((p) => ({
    value: p.id, label: p.name, hint: p.description || `${Math.round(p.usable_speech_seconds ?? 0)}s clean speech`,
    icon: <VoiceOrb seed={p.id} size={24} />, trailing: <VoicePreview profile={p} size={24} />,
  }));
  const engineOptions = engines.map((e) => ({ value: e.id, label: <span>{e.name}{e.recommended && <span className="ml-1.5 text-ember-ink">★</span>}</span>, hint: e.downloaded ? e.license : `${e.size_gb} GB download on first use` }));

  return (
    <div className="flex min-h-full">
      {/* ---------------------------------------------------------- editor */}
      <div className="min-w-0 flex-1 px-10 pb-16 pt-6">
        <div className="mb-5 flex items-center justify-between">
          <h1 className="display text-[34px] leading-none">Text to <em className="ember-ink">Speech</em></h1>
          <div className="flex gap-1">
            <input ref={fileRef} type="file" accept=".txt,.md,.markdown,.srt,text/*" className="hidden" onChange={(e) => e.target.files?.[0] && importFile(e.target.files[0])} />
            <button className="btn btn-ghost btn-sm" onClick={() => fileRef.current?.click()}><FileUp size={14} /> Import script</button>
            <button className="btn btn-ghost btn-sm" disabled={!text && !title} onClick={() => {
              const was = { text, title };
              setText(""); setTitle("");
              toast("Script cleared", "info", { action: { label: "Undo", run: () => { setText(was.text); setTitle(was.title); } } });
            }}><Trash2 size={14} /> Clear</button>
          </div>
        </div>

        <div
          className={`card relative flex flex-col overflow-hidden transition focus-within:border-[var(--focus)] focus-within:ring-4 focus-within:ring-accent-soft ${drag ? "border-accent ring-4 ring-accent-soft" : ""}`}
          onDragOver={(e) => { e.preventDefault(); setDrag(true); }}
          onDragLeave={() => setDrag(false)}
          onDrop={(e) => { e.preventDefault(); setDrag(false); if (e.dataTransfer.files[0]) importFile(e.dataTransfer.files[0]); }}
        >
          <label htmlFor="script-title" className="sr-only">Title</label>
          <input id="script-title" className="w-full border-b border-line bg-transparent px-7 pb-3 pt-5 text-[18px] font-medium text-fg outline-none placeholder:text-fg-3"
            placeholder="Title (optional): how this narration is named in History" value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} />
          <label htmlFor="script-text" className="sr-only">Script</label>
          <textarea
            id="script-text"
            className="min-h-[46vh] w-full flex-1 resize-none bg-transparent px-7 pb-4 pt-6 text-[16px] leading-[1.75] text-fg outline-none placeholder:text-fg-3"
            placeholder={"Paste or drop your script here. Up to 10,000 words.\n\nLong scripts are split at sentence boundaries, and every part is checked for silence gaps, glitches, wrong words and voice drift before it goes into the final audio."}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onPaste={(e) => {
              const pasted = e.clipboardData.getData("text/plain");
              const { text: fixed, joined } = unwrapLines(pasted);
              if (!joined) return;
              e.preventDefault();
              const el = e.currentTarget;
              const next = text.slice(0, el.selectionStart) + fixed + text.slice(el.selectionEnd);
              setText(next);
              toast(`Joined ${joined} line break${joined > 1 ? "s" : ""} that split sentences (from the copied text), so there are no odd pauses.`, "info");
            }}
          />
          <div className="flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-line bg-surface-2/50 px-5 py-3">
            {profile && (
              <div className="flex items-center gap-2 text-[13px]">
                <VoiceOrb seed={profile.id} size={22} active={busy} />
                <span className="font-medium">{profile.name}</span>
                <VoicePreview profile={profile} size={22} />
              </div>
            )}
            <WordMeter words={words} />
            <span className="text-[12.5px] text-fg-3">{words ? <>≈ {fmtDuration(audioSec)} audio · {parts} {parts === 1 ? "part" : "parts"} · renders in ~{fmtDuration(renderSec)}</> : "Paste a script to see length and render time"}</span>
            <div className="ml-auto flex items-center gap-3">
              {over && <span className="flex items-center gap-1 text-[12.5px] text-danger-ink"><AlertTriangle size={13} /> Over 10,000 words: split into parts</span>}
              <button className="btn btn-ember ember btn-lg" disabled={!text.trim() || !profile || busy || over} onClick={start}>
                {busy ? <Loader2 size={16} className="animate-spin" /> : <Sparkles size={16} />} Narrate
                <span className="kbd ml-1 border-[#1a0f07]/40">⌘↵</span>
              </button>
            </div>
          </div>
        </div>

        {already && (
          <p className="mt-3 flex flex-wrap items-center gap-2 text-[13px] text-fg-2" role="status">
            <Info size={14} className="shrink-0 text-ember-ink" />
            You narrated this script in {already.voice} {timeAgo(already.created)}{already.status === "needs_review" ? " (it needs review)" : ""}.
            <button className="btn btn-ghost btn-sm" onClick={() => go({ page: "history", id: already.id })}>Open it</button>
          </p>
        )}
        {engine && !engine.downloaded && (
          <p className="mt-3 text-[12.5px] text-fg-3">{engine.name} downloads once ({engine.size_gb} GB) on first use, then works offline.</p>
        )}

        <div ref={resultRef} className="scroll-mt-6">
          {genId && <div className="mt-8"><GenerationView id={genId} /></div>}
        </div>
      </div>

      {/* ---------------------------------------------------------- side panel */}
      <aside className="sticky top-0 h-screen w-[330px] shrink-0 overflow-y-auto border-l border-line bg-panel/60 px-5 pb-24 pt-6">
        <div className="mb-5"><Segmented value={tab} onChange={setTab} options={[{ value: "settings", label: "Settings" }, { value: "history", label: "History" }]} /></div>
        {tab === "settings" ? (
          <div className="space-y-6">
            <div>
              <span className="label" id="voice-label">Voice</span>
              <Select value={profileId} options={voiceOptions} onChange={setProfileId} placeholder="Choose a voice" label="Voice" />
            </div>
            <div>
              <span className="label">Model</span>
              <Select value={engineId} options={engineOptions} onChange={setEngineId} label="Model" />
              {engine && <p className="mt-2 text-[12px] leading-relaxed text-fg-3">{engine.description}</p>}
            </div>
            <div className="h-px bg-line" />
            <SliderRow label="Speed" value={speed} min={0.7} max={1.3} step={0.05} fmt={(v) => `${v.toFixed(2)}×`} onChange={setSpeed} left="Slower" right="Faster" />
            <div className="rounded-xl border border-line">
              <button className="flex w-full items-center justify-between gap-2 px-3.5 py-2.5 text-left" aria-expanded={showTone} aria-controls="tone" onClick={() => setShowTone((x) => !x)}>
                <span className="min-w-0">
                  <span className="block whitespace-nowrap text-[13px] font-medium">Delivery &amp; tone</span>
                  <span className="mt-0.5 block text-[12px] leading-snug text-fg-2">
                    {pct(vs.stability)} stable · {pct(vs.similarity)} similar{vs.style ? ` · style ${pct(vs.style)}` : ""}{vs.mic_match ? " · mic match" : ""}{vs.studio_finish ? " · studio finish" : ""}
                  </span>
                </span>
                <ChevronDown size={14} className={`shrink-0 transition ${showTone ? "rotate-180" : ""}`} />
              </button>
              {showTone && (
                <div id="tone" className="space-y-6 border-t border-line p-3.5">
              <SliderRow label="Stability" value={vs.stability} min={0} max={1} step={0.05} fmt={pct} onChange={(v) => updateVs({ stability: v })} left="More variable" right="More stable"
                help="Lower gives a livelier, more varied read. Higher keeps every line even and consistent." />
              <SliderRow label="Similarity" value={vs.similarity} min={0} max={1} step={0.05} fmt={pct} onChange={(v) => updateVs({ similarity: v })} left="Low" right="High"
                help={`Every part must match the voice fingerprint by at least ${Math.round((0.7 + 0.2 * vs.similarity) * 100)}% or it is re-recorded.`} />
              <SliderRow label="Style exaggeration" value={vs.style} min={0} max={1} step={0.05} fmt={pct} onChange={(v) => updateVs({ style: v })} left="None" right="Exaggerated"
                help={styleHelp(vs.style, profile, engineId)} />
              <div className="flex items-start justify-between gap-4">
                <div>
                  <div className="text-[13px] font-medium">Mic match</div>
                  <p className="mt-0.5 text-[12px] leading-snug text-fg-3">EQ the result to the tone of the original recording (warmth, presence, air), so it sounds like the same mic.</p>
                </div>
                <Switch on={vs.mic_match} onChange={(v) => updateVs({ mic_match: v })} label="Mic match" />
              </div>
              <div className="flex items-start justify-between gap-4">
                <div>
                  <div className="text-[13px] font-medium">Studio finish</div>
                  <p className="mt-0.5 text-[12px] leading-snug text-fg-3">Broadcast polish: rumble cut, de-essing and gentle compression for an even, radio-ready voice.</p>
                </div>
                <Switch on={vs.studio_finish} onChange={(v) => updateVs({ studio_finish: v })} label="Studio finish" />
              </div>
              {profile && !profile.studio && (
                <button onClick={() => go({ page: "voices", id: profile.id })} className="w-full rounded-xl border border-accent/40 bg-accent-soft px-3.5 py-3 text-left text-[12px] leading-snug text-fg-2 transition hover:border-accent">
                  <span className="font-medium text-fg">Hearing echo or a robotic edge?</span> This voice was cloned from the raw recording. Restore it to studio quality on its voice page →
                </button>
              )}
                </div>
              )}
            </div>
            <button className="btn btn-ghost btn-sm -ml-2" onClick={() => { setParams({}); setSpeed(1); updateVs(DEFAULT_VS); }}><RotateCcw size={13} /> Reset to defaults</button>
            {engine && engine.params.length > 0 && (
              <div className="rounded-xl border border-line">
                <button className="flex w-full items-center justify-between px-3.5 py-2.5 text-[12.5px] text-fg-2" onClick={() => setShowAdv((x) => !x)}>
                  Advanced model controls <ChevronDown size={14} className={`transition ${showAdv ? "rotate-180" : ""}`} />
                </button>
                {showAdv && (
                  <div className="space-y-4 border-t border-line p-3.5">
                    <p className="text-[12px] text-fg-3">Only change these to override the sliders above.</p>
                    {engine.params.map((p) => (
                      <SliderRow key={p.key} label={p.label} value={params[p.key] ?? p.default} min={p.min} max={p.max} step={p.step}
                        fmt={(v) => (Number.isInteger(p.step) ? String(v) : v.toFixed(2)) + (params[p.key] == null ? " · auto" : "")}
                        onChange={(v) => setParams({ ...params, [p.key]: v })} />
                    ))}
                  </div>
                )}
              </div>
            )}
            <div className="rounded-xl border border-line bg-surface p-3.5 text-[12px] leading-relaxed text-fg-3">
              Every part is checked: up to {settings?.max_attempts ?? 4} takes, ≤ {Math.round((settings?.max_wer ?? 0.12) * 100)}% word errors, no cut-off endings, no gaps or noise bursts{vs.style >= 0.3 ? ", and takes that keep the reference's liveliness are preferred" : ""}.
            </div>
          </div>
        ) : (
          <SideHistory onOpen={setGenId} />
        )}
      </aside>
    </div>
  );
}

const pct = (v: number) => `${Math.round(v * 100)}%`;

function styleHelp(style: number, profile: Profile | undefined, engineId: string) {
  const vs = profile?.variants ?? [];
  const hasTags = vs.some((v) => v.delivery);
  const base = style < 0.2
    ? "Uses the reference clip you picked for this voice, delivered as recorded."
    : style < 0.5
      ? "Moves to a livelier reference clip and loosens the read for more emotion."
      : "Uses your most expressive reference clip and a looser, more emotional read.";
  const extra = engineId.startsWith("chatterbox") ? " Also drives Chatterbox's built-in emotion intensity." : "";
  const note = !hasTags && style >= 0.2 ? " (Delivery tags are added to this voice on first use.)" : "";
  return base + extra + note;
}

// Mirror of the engine's line-unwrapping, so pasted PDF/Docs text is fixed visibly.
const TERMINAL = /[.!?:;"')\]]$/;
export function unwrapLines(t: string): { text: string; joined: number } {
  let joined = 0;
  const blocks = t.replace(/\r\n?/g, "\n").split(/\n\s*\n/).map((block) => {
    const lines = block.split("\n").map((l) => l.trim()).filter(Boolean);
    let out = "";
    for (const ln of lines) {
      if (!out) { out = ln; continue; }
      const prev = out.slice(out.lastIndexOf("\n") + 1);
      const heading = prev.length < 50 && /^[A-Z]/.test(ln) && !/[,-]$/.test(prev);
      if (TERMINAL.test(prev) || heading) { out += "\n" + ln; continue; }
      joined++;
      if (prev.endsWith("-")) out = /^[a-z]/.test(ln) ? out.slice(0, -1) + ln : out + ln;
      else out += " " + ln;
    }
    return out;
  });
  return { text: blocks.filter(Boolean).join("\n\n"), joined };
}

function WordMeter({ words }: { words: number }) {
  const p = Math.min(1, words / MAX_WORDS);
  const r = 7, c = 2 * Math.PI * r;
  return (
    <div className="flex items-center gap-2 text-[12.5px]" title="Words per narration (limit 10,000)">
      <svg width="18" height="18" viewBox="0 0 18 18" className="-rotate-90">
        <circle cx="9" cy="9" r={r} fill="none" stroke="var(--surface-3)" strokeWidth="2.5" />
        <circle cx="9" cy="9" r={r} fill="none" stroke={p >= 1 ? "var(--danger)" : "var(--accent)"} strokeWidth="2.5" strokeDasharray={c} strokeDashoffset={c * (1 - p)} strokeLinecap="round" />
      </svg>
      <span className={`font-mono tabular-nums ${words > MAX_WORDS ? "text-danger-ink" : "text-fg-2"}`}>{words.toLocaleString()}</span>
      <span className="text-fg-3">/ 10,000 words</span>
    </div>
  );
}

function SliderRow({ label, value, min, max, step, fmt, onChange, help, left, right }: { label: string; value: number; min: number; max: number; step: number; fmt: (v: number) => string; onChange: (v: number) => void; help?: string; left?: string; right?: string }) {
  return (
    <div>
      <div className="mb-1 flex items-baseline justify-between">
        <span className="text-[13px] font-medium">{label}</span>
        <span className="font-mono text-[12px] text-fg-2">{fmt(value)}</span>
      </div>
      <Slider value={value} min={min} max={max} step={step} onChange={onChange} />
      {(left || right) && <div className="flex justify-between text-[12px] text-fg-3"><span>{left}</span><span>{right}</span></div>}
      {help && <p className="mt-1 text-[12px] leading-snug text-fg-3">{help}</p>}
    </div>
  );
}

function SideHistory({ onOpen }: { onOpen: (id: string) => void }) {
  const [items, setItems] = useState<Generation[]>([]);
  const player = usePlayer();
  useEffect(() => { api.generations().then((g) => setItems(g.slice(0, 25))); }, []);
  if (!items.length) return <p className="text-[13px] text-fg-3">Nothing generated yet.</p>;
  return (
    <div className="-mx-2 space-y-0.5">
      {items.map((g) => (
        <div key={g.id} className="group flex items-center gap-3 rounded-xl px-2 py-2 transition hover:bg-surface">
          <button className="icon-btn" aria-label={`Play ${g.title || "narration"}`}
            disabled={g.status !== "done"}
            onClick={() => player.play({ src: api.audioUrl(g.id, "wav", Math.round(g.updated)), title: g.title || g.profile_name, subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" })}
          >
            <Play size={14} fill="currentColor" />
          </button>
          <button className="min-w-0 flex-1 text-left" onClick={() => onOpen(g.id)}>
            <div className="truncate text-[12.5px]">{g.title || g.text_preview}</div>
            <div className="text-[12px] text-fg-2">{g.status === "done" ? (g.outcome === "needs_listen" ? "Needs review" : fmtDuration(g.duration)) : g.status === "cancelled" || g.status === "interrupted" ? "Stopped" : g.status === "error" ? "Didn't finish" : "Speaking"} · {timeAgo(g.created)}</div>
          </button>
        </div>
      ))}
    </div>
  );
}
