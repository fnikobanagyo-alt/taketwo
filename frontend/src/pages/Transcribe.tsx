import { useEffect, useState } from "react";
import { ArrowLeft, Captions, Copy, Download, Loader2, RotateCcw, Trash2, Upload } from "lucide-react";
import { api, fmtDuration, fmtTime, LANGS, timeAgo, type Transcript } from "../api";
import Player from "../components/Player";
import { Empty, PageHeader, Progress, Select, useToast } from "../components/ui";
import { go } from "../App";

export default function TranscribePage({ id }: { id?: string }) {
  return id ? <Detail id={id} /> : <List />;
}

function List() {
  const toast = useToast();
  const [items, setItems] = useState<Transcript[] | null>(null);
  const [lang, setLang] = useState("");
  const [drag, setDrag] = useState(false);
  useEffect(() => { api.transcripts().then(setItems); }, []);

  const upload = async (f: File) => {
    const fd = new FormData();
    fd.append("language", lang);
    fd.append("file", f);
    try { const t = await api.createTranscript(fd); go({ page: "transcribe", id: t.id }); }
    catch (e) { toast((e as Error).message, "error"); }
  };

  return (
    <div className="mx-auto max-w-4xl px-10 pb-16 pt-6">
      <PageHeader title="Speech to" accent="text" subtitle="Accurate transcripts with timestamps, and caption files (SRT / VTT) ready for YouTube, Premiere or CapCut. Runs on-device with Whisper large-v3-turbo." />
      <div
        onDragOver={(e) => { e.preventDefault(); setDrag(true); }} onDragLeave={() => setDrag(false)}
        onDrop={(e) => { e.preventDefault(); setDrag(false); if (e.dataTransfer.files[0]) upload(e.dataTransfer.files[0]); }}
        className={`card flex flex-col items-center border-dashed px-8 py-12 text-center transition ${drag ? "border-accent ring-4 ring-accent-soft" : "border-line-strong"}`}
      >
        <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-2xl bg-surface-2 text-fg-2"><Captions size={22} /></div>
        <div className="text-[15px] font-medium">Drop audio or video to transcribe</div>
        <div className="mt-1 text-[12.5px] text-fg-3">Any length · mp3, wav, m4a, mp4, mov, webm…</div>
        <div className="mt-5 flex items-center gap-2">
          <Select className="w-44" value={lang} onChange={setLang} options={[{ value: "", label: "Detect language" }, ...Object.entries(LANGS).map(([k, v]) => ({ value: k, label: v }))]} />
          <label className="btn btn-primary cursor-pointer"><Upload size={15} /> Choose file
            <input type="file" accept="audio/*,video/*" className="hidden" onChange={(e) => e.target.files?.[0] && upload(e.target.files[0])} />
          </label>
        </div>
      </div>

      <h2 className="mb-3 mt-10 text-[15px] font-semibold">Transcripts</h2>
      {items && !items.length ? (
        <Empty icon={<Captions size={20} />} title="No transcripts yet" body="Upload a file above to get started." />
      ) : (
        <div className="card divide-y divide-line">
          {(items ?? []).map((t) => (
            <button key={t.id} onClick={() => go({ page: "transcribe", id: t.id })} className="flex w-full items-center gap-4 px-5 py-3.5 text-left transition hover:bg-surface-2">
              <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-surface-2 text-fg-2"><Captions size={16} /></div>
              <div className="min-w-0 flex-1">
                <div className="truncate text-[13.5px]">{t.name}</div>
                <div className="text-[12px] text-fg-3">{t.status === "done" ? `${t.words?.toLocaleString()} words · ${fmtDuration(t.duration)}` : t.status} · {timeAgo(t.created)}</div>
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function Detail({ id }: { id: string }) {
  const toast = useToast();
  const [t, setT] = useState<Transcript | null>(null);
  const [poke, setPoke] = useState(0);
  useEffect(() => {
    let alive = true;
    let h: number;
    const tick = async () => {
      const x = await api.transcript(id);
      if (!alive) return;
      setT(x);
      if (x.status !== "done" && x.status !== "error") h = window.setTimeout(tick, 1000);
    };
    tick();
    return () => { alive = false; clearTimeout(h); };
  }, [id, poke]);
  const retry = async () => {
    try { await api.retryTranscript(id); setPoke((n) => n + 1); toast("Transcribing again…", "info"); }
    catch (e) { toast((e as Error).message, "error"); }
  };
  if (!t) return <div className="p-10"><Loader2 className="animate-spin text-fg-3" /></div>;
  const running = t.status !== "done" && t.status !== "error";

  return (
    <div className="mx-auto max-w-4xl px-10 pb-16 pt-6">
      <button className="btn btn-ghost btn-sm -ml-2 mb-4" onClick={() => go({ page: "transcribe" })}><ArrowLeft size={14} /> Speech to Text</button>
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="display text-[34px] leading-tight">{t.name}</h1>
          {t.status === "done" && <div className="mt-1 text-[12.5px] text-fg-3">{t.words?.toLocaleString()} words · {fmtDuration(t.duration)} · {LANGS[t.language ?? ""] ?? t.language}</div>}
        </div>
        {t.status === "done" && (
          <div className="flex gap-2">
            <button className="btn btn-secondary btn-sm" onClick={() => { navigator.clipboard.writeText(t.text ?? ""); toast("Copied transcript"); }}><Copy size={13} /> Copy</button>
            <a className="btn btn-secondary btn-sm" href={api.transcriptExport(t.id, "srt")}><Download size={13} /> SRT</a>
            <a className="btn btn-secondary btn-sm" href={api.transcriptExport(t.id, "vtt")}><Download size={13} /> VTT</a>
            <a className="btn btn-secondary btn-sm" href={api.transcriptExport(t.id, "txt")}><Download size={13} /> TXT</a>
            <button className="btn btn-danger btn-sm" onClick={async () => { if (confirm("Delete this transcript?")) { await api.deleteTranscript(t.id); go({ page: "transcribe" }); } }}><Trash2 size={13} /></button>
          </div>
        )}
      </div>
      {running && (
        <div className="card p-5">
          <div className="mb-2 flex justify-between text-[13px]"><span className="flex items-center gap-2"><Loader2 size={14} className="animate-spin text-accent" /> {t.job?.message || "Queued…"}</span><span className="font-mono text-fg-3">{Math.round((t.job?.progress ?? 0) * 100)}%</span></div>
          <Progress value={t.job?.progress ?? 0} />
        </div>
      )}
      {t.status === "error" && (
        <div className="card flex items-center justify-between gap-4 border-danger/40 p-4 text-[13px]">
          <span className="text-danger">{t.error}</span>
          <button className="btn btn-secondary btn-sm shrink-0" onClick={retry}><RotateCcw size={13} /> Retry</button>
        </div>
      )}
      {t.status === "done" && (
        <>
          <div className="card mb-4 p-4"><Player src={api.transcriptAudio(t.id)} height={40} /></div>
          <div className="card divide-y divide-line">
            {t.captions?.map((c, i) => (
              <div key={i} className="flex gap-5 px-5 py-3">
                <span className="w-14 shrink-0 pt-0.5 font-mono text-[11.5px] text-fg-3">{fmtTime(c.start)}</span>
                <p className="text-[14px] leading-relaxed">{c.text}</p>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
