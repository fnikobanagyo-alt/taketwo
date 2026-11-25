import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, ArrowLeft, ArrowRight, AudioWaveform, Check, Circle, FileAudio, Loader2, Mic2, Plus, RefreshCw, Search, Sparkles, Square, Trash2, Upload, Wand2 } from "lucide-react";
import { api, fmtTime, LANGS, timeAgo, type Profile } from "../api";
import VoiceOrb from "../lib/orb";
import Player from "../components/Player";
import { Empty, InlineEdit, PageHeader, Progress, Select, Slider, Switch, useToast } from "../components/ui";
import { go } from "../App";
import VoicePreview from "../components/VoicePreview";

const MAX_SECONDS = 300;
const LATEST = 4; // keep in sync with backend PIPELINE_VERSION

export default function VoicesPage({ id }: { id?: string }) {
  if (id === "new") return <NewVoice />;
  if (id) return <VoiceDetail id={id} />;
  return <VoiceList />;
}

// ------------------------------------------------------------------ list

function VoiceList() {
  const toast = useToast();
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [q, setQ] = useState("");
  const [version, setVersion] = useState(0);
  const [upBusy, setUpBusy] = useState(false);
  const [poke, setPoke] = useState(0);
  useEffect(() => { api.pipelineVersion().then((v) => setVersion(v.version)).catch(() => {}); }, []);
  useEffect(() => {
    let alive = true;
    let t: number;
    const tick = () => api.profiles().then((p) => {
      if (!alive) return;
      setProfiles(p);
      if (p.some((x) => x.status === "queued" || x.status === "analyzing")) t = window.setTimeout(tick, 2000);
    });
    tick();
    return () => { alive = false; clearTimeout(t); };
  }, [poke]);
  const outdated = (profiles ?? []).filter((p) => p.status === "ready" && (p.pipeline_version ?? 0) < version);
  const upgradeAll = async () => {
    if (!confirm(`Update ${outdated.length} voice${outdated.length > 1 ? "s" : ""}?\n\nNothing you chose changes: names, descriptions, the reference clip in use, transcript edits and model choices all stay. Only the internal reference clips are re-cut at natural pauses (each old clip is kept as a backup), and each voice gets a fresh preview.`)) return;
    setUpBusy(true);
    try { const r = await api.upgradeAll(); toast(`Updating ${r.queued.length} voice${r.queued.length === 1 ? "" : "s"}…`, "info"); setPoke((n) => n + 1); }
    catch (e) { toast((e as Error).message, "error"); }
    finally { setUpBusy(false); }
  };
  const removeVoice = async (p: Profile) => {
    if (!confirm(`Delete "${p.name}"?\n\nThis removes the voice and its recordings from VoiceForge. Narrations you already made stay in History.`)) return;
    await api.deleteProfile(p.id);
    toast(`Deleted ${p.name}`);
    setPoke((n) => n + 1);
  };
  const list = (profiles ?? []).filter((p) => (p.name + " " + (p.description ?? "")).toLowerCase().includes(q.toLowerCase()));

  return (
    <div className="mx-auto max-w-6xl px-10 pb-16 pt-6">
      <PageHeader
        title="Your" accent="voices"
        subtitle="Every voice is learned from up to 5 minutes of audio: transcribed, fingerprinted and checked against every line it speaks."
        actions={<>
          <button className="btn btn-secondary" onClick={() => go({ page: "design" })}><Wand2 size={15} /> Design</button>
          <button className="btn btn-primary" onClick={() => go({ page: "voices", id: "new" })}><Plus size={15} /> Clone a voice</button>
        </>}
      />
      {outdated.length > 0 && (
        <div className="card relative mb-6 flex flex-wrap items-center justify-between gap-4 overflow-hidden border-accent/40 p-5">
          <div className="pointer-events-none absolute inset-0" style={{ background: "radial-gradient(500px 200px at 0% 0%, var(--accent-soft), transparent 70%)" }} />
          <div className="relative max-w-2xl">
            <div className="flex items-center gap-2 text-[14px] font-semibold"><Sparkles size={15} className="text-accent" /> {outdated.length} voice{outdated.length > 1 ? "s" : ""} can be updated</div>
            <p className="mt-1 text-[12.5px] leading-relaxed text-fg-3">
              {outdated.map((p) => p.name).join(", ")} {outdated.length > 1 ? "were" : "was"} cloned before the latest improvements: studio restoration, cleaner word endings, delivery tags and clone-spoken previews. Updating re-analyzes each recording and saves it; names, descriptions and model choices are kept.
            </p>
          </div>
          <div className="relative flex items-center gap-4">
            <button className="btn btn-ember ember" disabled={upBusy} onClick={upgradeAll}>{upBusy ? <Loader2 size={15} className="animate-spin" /> : <RefreshCw size={15} />} Update all</button>
          </div>
        </div>
      )}
      {profiles && profiles.length > 3 && (
        <div className="relative mb-5 max-w-sm">
          <Search size={15} className="absolute left-3 top-3 text-fg-3" />
          <input className="field !pl-10" placeholder="Search voices" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      )}
      {profiles === null ? (
        <Loader2 className="animate-spin text-fg-3" />
      ) : profiles.length === 0 ? (
        <Empty icon={<Mic2 size={22} />} title="No voices yet" body="Clone a voice from a recording, or design a brand-new one from a description."
          action={<button className="btn btn-primary" onClick={() => go({ page: "voices", id: "new" })}><Plus size={15} /> Clone a voice</button>} />
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {list.map((p) => (
            <button key={p.id} onClick={() => go({ page: "voices", id: p.id })} className="card group relative overflow-hidden p-5 text-left transition hover:-translate-y-0.5 hover:border-line-strong hover:shadow-float">
              <div className="flex items-start justify-between">
                <VoiceOrb seed={p.id} size={52} active={p.status === "analyzing"} />
                <div className="flex items-center gap-1.5">
                  {p.status === "ready" && (p.pipeline_version ?? 0) < version && <span className="chip bg-accent-soft text-accent">Update available</span>}
                  <StatusChip p={p} />
                  <span role="button" tabIndex={0} title={`Delete ${p.name}`}
                    onClick={(e) => { e.stopPropagation(); removeVoice(p); }}
                    onKeyDown={(e) => { if (e.key === "Enter") { e.stopPropagation(); removeVoice(p); } }}
                    className="icon-btn h-7 w-7 opacity-0 transition group-hover:opacity-100 hover:!text-danger"><Trash2 size={14} /></span>
                </div>
              </div>
              <div className="mt-4 flex items-center gap-2.5">
                <span className="truncate text-[15px] font-semibold">{p.name}</span>
                <VoicePreview profile={p} size={26} />
              </div>
              <div className="mt-1 line-clamp-2 min-h-[36px] text-[12.5px] leading-relaxed text-fg-3">
                {p.description || (p.status === "ready" ? `${Math.round(p.usable_speech_seconds ?? 0)}s of clean speech · ${Math.round((p.speech_rate_wps ?? 0) * 60)} wpm` : "Analyzing the recording…")}
              </div>
              <div className="mt-4 flex items-center justify-between border-t border-line pt-3 text-[12px] text-fg-3">
                <span>{LANGS[p.language] ?? p.language}{p.designed ? " · designed" : " · cloned"}</span>
                <span>{timeAgo(p.created)}</span>
              </div>
            </button>
          ))}
          <button onClick={() => go({ page: "voices", id: "new" })} className="flex min-h-[200px] flex-col items-center justify-center gap-2 rounded-2xl border border-dashed border-line-strong text-fg-3 transition hover:border-accent hover:text-fg">
            <Plus size={20} /><span className="text-[13px]">Clone a new voice</span>
          </button>
        </div>
      )}
    </div>
  );
}

function StatusChip({ p }: { p: Profile }) {
  if (p.status === "ready") return <span className="chip bg-ok/12 text-ok"><Check size={11} /> Ready</span>;
  if (p.status === "error") return <span className="chip bg-danger/12 text-danger">Error</span>;
  return <span className="chip bg-accent-soft text-accent"><Loader2 size={11} className="animate-spin" /> Analyzing</span>;
}

// ------------------------------------------------------------------ new voice

function NewVoice() {
  const toast = useToast();
  const [name, setName] = useState("");
  const [language, setLanguage] = useState("en");
  const [denoise, setDenoise] = useState(false);
  const [studio, setStudio] = useState(true);
  const [file, setFile] = useState<File | null>(null);
  const [url, setUrl] = useState("");
  const [fileDur, setFileDur] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [drag, setDrag] = useState(false);

  const pick = (f: File) => {
    setFile(f);
    setFileDur(null);
    const u = URL.createObjectURL(f);
    setUrl(u);
    if (!name) setName(f.name.replace(/\.[^.]+$/, "").replace(/[_-]+/g, " "));
    const a = new Audio(u);
    a.onloadedmetadata = () => setFileDur(isFinite(a.duration) ? a.duration : null);
  };

  const submit = async () => {
    if (!file) return;
    setBusy(true);
    const fd = new FormData();
    fd.append("name", name || "Untitled voice");
    fd.append("language", language);
    fd.append("denoise", String(denoise));
    fd.append("studio", String(studio));
    fd.append("file", file);
    try {
      const p = await api.createProfile(fd);
      go({ page: "voices", id: p.id });
    } catch (e) {
      toast((e as Error).message, "error");
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto max-w-3xl px-10 pb-16 pt-6">
      <button className="btn btn-ghost btn-sm -ml-2 mb-4" onClick={() => go({ page: "voices" })}><ArrowLeft size={14} /> Voices</button>
      <PageHeader title="Clone a" accent="voice" subtitle="Upload or record 30 seconds to 5 minutes of one person speaking naturally. VoiceForge studies all of it: more clean speech means a sharper fingerprint and better reference clips." />

      <div
        onDragOver={(e) => { e.preventDefault(); setDrag(true); }}
        onDragLeave={() => setDrag(false)}
        onDrop={(e) => { e.preventDefault(); setDrag(false); if (e.dataTransfer.files[0]) pick(e.dataTransfer.files[0]); }}
        className={`card relative overflow-hidden border-dashed p-8 transition ${drag ? "border-accent ring-4 ring-accent-soft" : "border-line-strong"}`}
      >
        {file ? (
          <div>
            <div className="mb-4 flex items-center gap-3">
              <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-accent-soft text-accent"><FileAudio size={18} /></div>
              <div className="min-w-0 flex-1">
                <div className="truncate text-[14px] font-medium">{file.name}</div>
                <div className="text-[12px] text-fg-3">{(file.size / 1e6).toFixed(1)} MB{fileDur ? ` · ${fmtTime(fileDur)}` : ""}</div>
              </div>
              <button className="btn btn-ghost btn-sm" onClick={() => { setFile(null); setUrl(""); }}>Remove</button>
            </div>
            <Player src={url} height={44} />
            {fileDur && fileDur > MAX_SECONDS && (
              <div className="mt-3 flex items-center gap-2 text-[12.5px] text-warn"><AlertTriangle size={14} /> Longer than 5 minutes: the first 5:00 will be used.</div>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-center py-6 text-center">
            <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-2xl bg-surface-2 text-fg-2"><Upload size={22} /></div>
            <div className="text-[15px] font-medium">Drop an audio or video file</div>
            <div className="mt-1 text-[12.5px] text-fg-3">wav · mp3 · m4a · flac · ogg · webm · mp4 · up to 5 minutes used</div>
            <div className="mt-5 flex gap-2">
              <label className="btn btn-primary cursor-pointer">
                <Upload size={15} /> Choose file
                <input type="file" accept="audio/*,video/*" className="hidden" onChange={(e) => e.target.files?.[0] && pick(e.target.files[0])} />
              </label>
              <Recorder onDone={pick} />
            </div>
          </div>
        )}
      </div>

      <div className="mt-6 grid grid-cols-3 gap-4">
        <div className="col-span-2">
          <label className="label">Voice name</label>
          <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Christopher, documentary narration" />
        </div>
        <div>
          <label className="label">Language</label>
          <Select value={language} onChange={setLanguage} options={Object.entries(LANGS).map(([k, v]) => ({ value: k, label: v }))} />
        </div>
      </div>
      <div className="mt-6">
        <label className="label">Recording quality</label>
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          <QualityCard on={studio} onClick={() => setStudio(true)} icon={<Sparkles size={17} />} title="Studio quality mic" badge="Recommended"
            body="Removes room echo, background noise and cheap-mic colour before cloning, and rebuilds crisp 48 kHz detail. Clones sound like a treated studio booth, with no robotic or echoey edge." />
          <QualityCard on={!studio} onClick={() => setStudio(false)} icon={<AudioWaveform size={17} />} title="Keep original sound"
            body="Clone exactly as recorded, room and mic included. Choose this if the source is already a pro studio recording, or you want its character." />
        </div>
        {!studio && (
          <div className="card mt-3 flex items-center justify-between gap-4 p-4">
            <div>
              <div className="text-[13.5px] font-medium">Always reduce background noise</div>
              <div className="text-[12px] text-fg-3">Applied automatically when noise is detected. Force it for fans or hum.</div>
            </div>
            <Switch on={denoise} onChange={setDenoise} />
          </div>
        )}
      </div>
      <button className="btn btn-ember ember btn-lg mt-6 w-full" disabled={!file || busy} onClick={submit}>
        {busy ? <Loader2 size={16} className="animate-spin" /> : <Wand2 size={16} />} Analyze & clone voice
      </button>
    </div>
  );
}

function QualityCard({ on, onClick, icon, title, body, badge }: { on: boolean; onClick: () => void; icon: React.ReactNode; title: string; body: string; badge?: string }) {
  return (
    <button onClick={onClick} className={`card relative p-4 text-left transition ${on ? "border-accent/70 shadow-[0_0_0_3px_var(--accent-soft)]" : "hover:border-line-strong"}`}>
      <div className="flex items-center gap-2.5">
        <span className={`flex h-8 w-8 items-center justify-center rounded-lg ${on ? "ember text-[#1a0f07]" : "bg-surface-2 text-fg-2"}`}>{icon}</span>
        <span className="text-[14px] font-medium">{title}</span>
        {badge && <span className="chip bg-accent-soft text-accent">{badge}</span>}
        <span className={`ml-auto flex h-5 w-5 items-center justify-center rounded-full border ${on ? "ember border-transparent" : "border-line-strong"}`}>{on && <Check size={11} strokeWidth={3} className="text-[#1a0f07]" />}</span>
      </div>
      <p className="mt-2.5 text-[12.5px] leading-relaxed text-fg-3">{body}</p>
    </button>
  );
}

function Recorder({ onDone }: { onDone: (f: File) => void }) {
  const toast = useToast();
  const [rec, setRec] = useState<MediaRecorder | null>(null);
  const [secs, setSecs] = useState(0);
  const timer = useRef<number | undefined>(undefined);
  const streamRef = useRef<MediaStream | null>(null);
  const recRef = useRef<MediaRecorder | null>(null);
  // Release the microphone whenever this recorder goes away (page change, error, stop).
  const release = () => {
    clearInterval(timer.current);
    streamRef.current?.getTracks().forEach((t) => t.stop());
    streamRef.current = null;
  };
  useEffect(() => () => {
    if (recRef.current && recRef.current.state !== "inactive") { recRef.current.onstop = null; recRef.current.stop(); }
    release();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  const start = async () => {
    let stream: MediaStream | null = null;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false } });
      const r = new MediaRecorder(stream);
      streamRef.current = stream;
      recRef.current = r;
      const parts: Blob[] = [];
      r.ondataavailable = (e) => parts.push(e.data);
      r.onstop = () => {
        release();
        recRef.current = null;
        onDone(new File(parts, `Recording ${new Date().toLocaleString()}.webm`, { type: r.mimeType }));
        setRec(null);
      };
      r.start(1000);
      setRec(r);
      const t0 = Date.now();
      timer.current = window.setInterval(() => { const s = (Date.now() - t0) / 1000; setSecs(s); if (s >= MAX_SECONDS) r.stop(); }, 250);
    } catch {
      stream?.getTracks().forEach((t) => t.stop());
      toast("Microphone access was blocked. Allow it in System Settings → Privacy → Microphone.", "error");
    }
  };
  if (rec)
    return (
      <button className="btn btn-secondary border-danger/40 text-danger" onClick={() => rec.stop()}>
        <Square size={12} fill="currentColor" /> Stop · <span className="font-mono">{fmtTime(secs)}</span> / 5:00
      </button>
    );
  return <button className="btn btn-secondary" onClick={start}><Circle size={12} fill="var(--danger)" className="text-danger" /> Record</button>;
}

// ------------------------------------------------------------------ detail

function VoiceDetail({ id }: { id: string }) {
  const toast = useToast();
  const [p, setP] = useState<Profile | null>(null);
  const [refText, setRefText] = useState("");
  // A transcript typed by the user is kept: polling must not overwrite it with the saved text.
  const dirtyRef = useRef(false);
  const loadedVariant = useRef<number | null>(null);
  const [denoise, setDenoise] = useState(false);
  const [windowSec, setWindowSec] = useState(12);
  const [nonce, setNonce] = useState(0);

  const load = useCallback(async () => {
    const x = await api.profile(id);
    setP(x);
    if (x.variants?.length) {
      const av = x.active_variant ?? 0;
      if (!dirtyRef.current || loadedVariant.current !== av) {
        setRefText(x.ref_text_override?.[String(av)] || x.variants[av].text);
        dirtyRef.current = false;
        loadedVariant.current = av;
      }
    }
    return x;
  }, [id]);

  useEffect(() => {
    let alive = true;
    let t: number;
    let waits = 0;
    const tick = async () => {
      const x = await load().catch(() => null);
      if (!alive || !x) return;
      if (x.status !== "ready" && x.status !== "error") t = window.setTimeout(tick, 1000);
      else if (x.job) t = window.setTimeout(tick, 1000);
      else if (x.status === "ready" && !x.preview && waits++ < 60) t = window.setTimeout(tick, 3000); // clone sample on its way
    };
    tick();
    return () => { alive = false; clearTimeout(t); };
  }, [load, nonce]);

  if (!p) return <div className="p-10"><Loader2 className="animate-spin text-fg-3" /></div>;
  const busy = p.status === "queued" || p.status === "analyzing";
  const av = p.active_variant ?? 0;
  const origText = p.variants?.[av]?.text ?? "";

  const patch = async (x: Parameters<typeof api.patchProfile>[1], msg: string) => {
    try { await api.patchProfile(id, x); await load(); toast(msg); } catch (e) { toast((e as Error).message, "error"); }
  };
  const reanalyze = async () => { await api.reanalyze(id, denoise, windowSec); setNonce((n) => n + 1); toast("Re-analyzing voice…", "info"); };
  const del = async () => { if (confirm(`Delete "${p.name}"?\n\nThis removes the voice and its recordings from VoiceForge. Narrations you already made stay in History. This can't be undone.`)) { await api.deleteProfile(id); toast(`Deleted ${p.name}`); go({ page: "voices" }); } };

  return (
    <div className="mx-auto max-w-5xl px-10 pb-16 pt-6">
      <button className="btn btn-ghost btn-sm -ml-2 mb-5" onClick={() => go({ page: "voices" })}><ArrowLeft size={14} /> Voices</button>

      <div className="card relative overflow-hidden p-7">
        <div className="pointer-events-none absolute inset-0" style={{ background: "radial-gradient(520px 300px at 92% 0%, var(--accent-soft), transparent 75%)" }} />
        <div className="relative flex items-start gap-5">
          <VoiceOrb seed={p.id} size={76} active={busy} />
          <div className="min-w-0 flex-1 space-y-1">
            <div className="flex items-center gap-2">
              <span className="eyebrow">{p.designed ? "Designed voice" : "Cloned voice"} · {LANGS[p.language] ?? p.language}</span>
              {p.studio && <span className="chip ember text-[#1a0f07]"><Sparkles size={10} /> Studio restored</span>}
            </div>
            <div className="flex items-center gap-3">
              <VoicePreview profile={p} size={38} />
              <InlineEdit className="min-w-0 flex-1" value={p.name} inputClass="display text-[34px] leading-tight" onSave={(v) => patch({ name: v }, "Name saved")} />
            </div>
            {p.refresh_report && (
              <div className="text-[12px] leading-relaxed text-ok">
                Updated: {p.refresh_report.updated.length} reference clip{p.refresh_report.updated.length === 1 ? "" : "s"} now start and end at natural pauses
                {p.refresh_report.trimmed?.length ? `; clip ${p.refresh_report.trimmed.map((i) => i + 1).join(", ")} ended mid-word, so its unfinished last word was trimmed` : ""}
                {p.refresh_report.kept.length ? `; ${p.refresh_report.kept.length} left as they were` : ""}. Your chosen clip, name and transcript edits are unchanged.
              </div>
            )}
            {p.status === "ready" && (
              <div className="text-[12px] text-fg-3">
                {p.preview ? `Preview: a ${Math.round(p.preview.duration)}s sample spoken by this clone` : "Preview: the reference recording (a sample spoken by the clone is being prepared)"}
              </div>
            )}
            <InlineEdit value={p.description ?? ""} placeholder="Add a description (e.g. calm documentary narrator)" inputClass="text-[13.5px] text-fg-2" onSave={(v) => patch({ description: v }, "Description saved")} />
          </div>
          <div className="flex shrink-0 gap-2">
            {p.status === "ready" && (p.pipeline_version ?? 0) < LATEST && (
              <button className="btn btn-secondary" disabled={!!p.job} title="Re-cut this voice's reference clips at natural pauses. Keeps your chosen clip, transcript edits, name and description." onClick={async () => {
                try { await api.refreshProfile(id); setNonce((n) => n + 1); toast("Updating voice…", "info"); } catch (e) { toast((e as Error).message, "error"); }
              }}>{p.job ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />} Update voice</button>
            )}
            <button className="btn btn-danger" onClick={del}><Trash2 size={15} /> Delete voice</button>
            <button className="btn btn-primary" disabled={p.status !== "ready"} onClick={() => go({ page: "speech", id: p.id })}>Use voice <ArrowRight size={15} /></button>
          </div>
        </div>

        {busy && (
          <div className="relative mt-6">
            <div className="mb-2 flex items-center justify-between text-[13px]">
              <span className="flex items-center gap-2"><Loader2 size={14} className="animate-spin text-accent" /> {p.job?.message || "Waiting in queue…"}</span>
              <span className="font-mono text-[12px] text-fg-3">{Math.round((p.job?.progress ?? 0) * 100)}%</span>
            </div>
            <Progress value={p.job?.progress ?? 0} />
            <p className="mt-3 text-[12.5px] leading-relaxed text-fg-3">Transcribing every word, measuring noise, building a voice fingerprint from all the speech, and choosing the cleanest reference clips. About 20–60 s per 5 minutes of audio.</p>
          </div>
        )}
        {p.status === "error" && (
          <div className="relative mt-5 flex items-center justify-between gap-4 rounded-xl bg-danger/10 px-4 py-3 text-[13px] text-danger">
            {p.error}
            <button className="btn btn-secondary btn-sm" onClick={reanalyze}><RefreshCw size={13} /> Re-analyze</button>
          </div>
        )}
      </div>

      {p.status === "ready" && p.variants && (
        <>
          <div className="mt-5 grid grid-cols-2 gap-3 md:grid-cols-4">
            <Stat label="Clean speech" value={`${Math.round(p.usable_speech_seconds ?? 0)}s`} hint={`of ${fmtTime(p.duration)} analyzed`} />
            <Stat label="Voice consistency" value={`${Math.round((p.self_similarity ?? 0) * 100)}%`} hint={(p.self_similarity ?? 0) > 0.8 ? "single speaker" : "mixed or noisy"} />
            <Stat label="Room echo & noise" value={p.room_echo_db != null ? echoLabel(p.studio && p.room_echo_after_db != null ? p.room_echo_after_db : p.room_echo_db) : `${Math.round(p.snr_db ?? 0)} dB`}
              hint={p.studio && p.room_echo_db != null && p.room_echo_after_db != null ? `was ${echoLabel(p.room_echo_db).toLowerCase()} before restoration` : p.denoised ? "noise reduced" : "as recorded"} />
            <Stat label="Speaking rate" value={`${Math.round((p.speech_rate_wps ?? 0) * 60)} wpm`} hint="used to catch loops" />
          </div>

          {p.tips?.length ? (
            <div className="mt-4 space-y-1.5 rounded-xl border border-warn/30 bg-warn/[0.06] px-4 py-3 text-[13px] text-warn">
              {p.tips.map((t) => <div key={t} className="flex gap-2"><AlertTriangle size={15} className="mt-0.5 shrink-0" />{t}</div>)}
            </div>
          ) : null}

          <StudioPanel p={p} onRun={async (studio) => {
            if (!confirm(studio ? "Restore this voice to studio quality? It re-analyzes the recording (about a minute) and picks fresh reference clips." : "Go back to the original recording's sound? It re-analyzes the recording and picks fresh reference clips.")) return;
            await api.reanalyze(id, false, windowSec, studio); setNonce((n) => n + 1); toast(studio ? "Restoring to studio quality…" : "Re-analyzing with the original sound…", "info");
          }} />

          <section className="mt-10">
            <h2 className="text-[16px] font-semibold">Reference clip</h2>
            <p className="mb-4 mt-1 max-w-2xl text-[13px] leading-relaxed text-fg-3">
              The model copies both the voice and the delivery of this clip, so pick the one that sounds how you want your narration to feel. Options always include the speaker's most expressive and calmest clean moments. With Style exaggeration turned up, the most expressive clip is used automatically.
            </p>
            <div className="space-y-3">
              {p.variants.map((v) => {
                const on = v.index === av;
                return (
                  <div key={v.index} className={`card p-4 transition ${on ? "border-accent/60 shadow-[0_0_0_3px_var(--accent-soft)]" : ""}`}>
                    <div className="mb-3 flex items-center justify-between gap-3">
                      <div className="flex items-center gap-2.5">
                        <span className={`flex h-5 w-5 items-center justify-center rounded-full border ${on ? "ember border-transparent" : "border-line-strong"}`}>{on && <Check size={11} className="text-[#1a0f07]" strokeWidth={3} />}</span>
                        <span className="text-[13.5px] font-medium">Option {v.index + 1}</span>
                        {v.delivery && <DeliveryChip d={v.delivery} />}
                        <span className="text-[12px] text-fg-3">{fmtTime(v.start)}–{fmtTime(v.end)} · {v.duration.toFixed(1)}s · quality {Math.round(Math.min(1, v.score / 1.3) * 100)}</span>
                      </div>
                      {on ? <span className="chip ember text-[#1a0f07]">In use</span> : <button className="btn btn-secondary btn-sm" onClick={() => patch({ active_variant: v.index }, `Now using option ${v.index + 1}`)}>Use this clip</button>}
                    </div>
                    <Player src={api.profileAudio(p.id, v.file)} height={34} compact />
                    <p className="mt-3 text-[12.5px] leading-relaxed text-fg-2">“{p.ref_text_override?.[String(v.index)] || v.text}”</p>
                  </div>
                );
              })}
            </div>

            <div className="card mt-4 p-5">
              <label className="label">Transcript of the clip in use</label>
              <p className="mb-2 text-[12px] text-fg-3">Must match the audio word for word. Fix any names Whisper misheard: a wrong word here is a common cause of odd sounds.</p>
              <textarea className="field text-[13.5px]" rows={3} value={refText} onChange={(e) => { setRefText(e.target.value); dirtyRef.current = true; }} />
              <div className="mt-3 flex gap-2">
                <button className="btn btn-primary btn-sm" disabled={refText === (p.ref_text_override?.[String(av)] || origText)} onClick={() => { dirtyRef.current = false; patch({ ref_text: refText }, "Transcript saved"); }}><Check size={13} /> Save transcript</button>
                <button className="btn btn-ghost btn-sm" onClick={() => { dirtyRef.current = false; setRefText(p.ref_text_override?.[String(av)] || origText); }}>Revert</button>
              </div>
            </div>
          </section>

          <section className="mt-10 grid grid-cols-1 gap-4 md:grid-cols-2">
            <div className="card p-5">
              <h3 className="text-[14px] font-semibold">Re-analyze</h3>
              <div className="mt-4">
                <div className="mb-1 flex justify-between text-[13px]"><span>Reference length</span><span className="font-mono text-fg-2">{windowSec}s</span></div>
                <Slider value={windowSec} min={6} max={25} step={1} onChange={setWindowSec} />
                <p className="mt-1 text-[11.5px] text-fg-3">10–15s suits most models. Longer can help very expressive voices.</p>
              </div>
              <div className="mt-4 flex items-center justify-between text-[13px]">Force noise reduction <Switch on={denoise} onChange={setDenoise} /></div>
              <button className="btn btn-secondary btn-sm mt-5" onClick={reanalyze}><RefreshCw size={13} /> Re-analyze voice</button>
            </div>
            <div className="card p-5">
              <h3 className="text-[14px] font-semibold">Source recording</h3>
              <div className="mt-4"><Player src={api.profileAudio(p.id, "clean.wav")} height={34} compact /></div>
              <p className="mt-3 max-h-32 overflow-y-auto text-[12.5px] leading-relaxed text-fg-3">{p.transcript}</p>
            </div>
          </section>
        </>
      )}
    </div>
  );
}

function echoLabel(db: number) {
  return db <= -40 ? "Studio clean" : db <= -32 ? "Low" : db <= -26 ? "Noticeable" : "Heavy";
}

function StudioPanel({ p, onRun }: { p: Profile; onRun: (studio: boolean) => void }) {
  if (p.studio)
    return (
      <section className="card mt-5 p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h3 className="flex items-center gap-2 text-[14px] font-semibold"><Sparkles size={15} className="text-accent" /> Studio restored</h3>
            <p className="mt-1 text-[12.5px] text-fg-3">Echo, noise and mic colour were removed before cloning. Compare below.</p>
          </div>
          <button className="btn btn-ghost btn-sm" onClick={() => onRun(false)}>Use original sound instead</button>
        </div>
        <div className="mt-4 grid grid-cols-1 gap-4 md:grid-cols-2">
          <div><div className="mb-2 text-[12px] text-fg-3">Before · as recorded</div><Player src={api.profileAudio(p.id, p.original_file)} height={30} compact /></div>
          <div><div className="mb-2 text-[12px] font-medium text-accent">After · studio restored</div><Player src={api.profileAudio(p.id, "studio.wav")} height={30} compact /></div>
        </div>
      </section>
    );
  const bad = (p.room_echo_db ?? -50) > -32;
  return (
    <section className={`card mt-5 flex flex-wrap items-center justify-between gap-4 p-5 ${bad ? "border-accent/40" : ""}`}>
      <div className="max-w-xl">
        <h3 className="text-[14px] font-semibold">{bad ? "Room echo detected in this recording" : "Want a studio-mic sound?"}</h3>
        <p className="mt-1 text-[12.5px] leading-relaxed text-fg-3">
          Clones copy the room and mic along with the voice, which can sound echoey or robotic. Studio restoration removes echo and noise and rebuilds crisp detail before cloning.
        </p>
      </div>
      <button className="btn btn-ember ember" onClick={() => onRun(true)}><Sparkles size={15} /> Restore to studio quality</button>
    </section>
  );
}

function DeliveryChip({ d }: { d: "expressive" | "balanced" | "calm" }) {
  const map = { expressive: ["Expressive", "bg-accent-soft text-accent"], balanced: ["Balanced", "bg-surface-2 text-fg-2"], calm: ["Calm", "bg-ok/12 text-ok"] } as const;
  return <span className={`chip ${map[d][1]}`}>{map[d][0]}</span>;
}

function Stat({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="card px-4 py-3.5">
      <div className="text-[12px] text-fg-3">{label}</div>
      <div className="mt-1 text-[22px] font-semibold tabular-nums tracking-tight">{value}</div>
      {hint && <div className="text-[11.5px] text-fg-3">{hint}</div>}
    </div>
  );
}
