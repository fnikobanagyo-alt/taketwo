import { useEffect, useState } from "react";
import { Check, Loader2, RefreshCw, Save, Wand2 } from "lucide-react";
import { api, LANGS, type Design } from "../api";
import VoiceOrb from "../lib/orb";
import Player from "../components/Player";
import { Modal, PageHeader, Progress, Segmented, Select, useToast } from "../components/ui";
import { go } from "../App";

const EXAMPLES = [
  "A deep, calm male documentary narrator in his fifties, warm and measured, with a slight gravel.",
  "A clear, confident young female voice, friendly and upbeat, perfect for explainer videos.",
  "An elderly British storyteller, soft and wise, speaking slowly by a fireplace.",
  "A tense, whispery male voice for true-crime narration, low and deliberate.",
  "An energetic YouTube host in his twenties, fast-paced, bright and enthusiastic.",
];

export default function DesignPage() {
  const toast = useToast();
  const [desc, setDesc] = useState("");
  const [text, setText] = useState("");
  const [lang, setLang] = useState("en");
  const [count, setCount] = useState<"2" | "3" | "4">("3");
  const [design, setDesign] = useState<Design | null>(null);
  const [saveIdx, setSaveIdx] = useState<number | null>(null);
  const [name, setName] = useState("");

  useEffect(() => {
    if (!design || design.status === "done" || design.status === "error") return;
    const t = setTimeout(async () => setDesign(await api.design(design.id)), 1200);
    return () => clearTimeout(t);
  }, [design]);

  const run = async () => {
    try { setDesign(await api.createDesign({ description: desc, text: text || undefined, language: lang, count: +count })); }
    catch (e) { toast((e as Error).message, "error"); }
  };
  const save = async () => {
    if (!design || saveIdx == null) return;
    const p = await api.saveDesign(design.id, saveIdx, name || "Designed voice");
    toast("Voice saved. Analyzing it now…");
    go({ page: "voices", id: p.id });
  };
  const running = design && (design.status === "queued" || design.status === "running");

  return (
    <div className="mx-auto max-w-4xl px-10 pb-16 pt-6">
      <PageHeader title="Voice" accent="design" subtitle="Describe a voice that doesn't exist yet: age, tone, accent, pace, mood. TakeTwo creates several takes; keep the one you like as a reusable voice." />

      <div className="card p-6">
        <label className="label">Describe the voice</label>
        <textarea className="field text-[15px]" rows={3} value={desc} onChange={(e) => setDesc(e.target.value)} placeholder="e.g. A deep, calm male documentary narrator, warm and measured…" />
        <div className="mt-3 flex flex-wrap gap-2">
          {EXAMPLES.map((e) => (
            <button key={e} onClick={() => setDesc(e)} className="chip h-7 border border-line bg-surface-2 px-3 text-fg-2 transition hover:border-line-strong hover:text-fg">{e.split(",")[0]}</button>
          ))}
        </div>
        <details className="mt-5">
          <summary className="cursor-pointer text-[13px] text-fg-2">Preview text & options</summary>
          <div className="mt-3 grid grid-cols-3 gap-4">
            <div className="col-span-3">
              <label className="label">What the preview says (optional, ~40–60 words works best)</label>
              <textarea className="field text-[13.5px]" rows={2} value={text} onChange={(e) => setText(e.target.value)} placeholder="Leave empty for a neutral narration sample." />
            </div>
            <div>
              <label className="label">Language</label>
              <Select value={lang} onChange={setLang} options={Object.entries(LANGS).map(([k, v]) => ({ value: k, label: v }))} />
            </div>
            <div>
              <label className="label">Variations</label>
              <Segmented value={count} onChange={setCount} options={[{ value: "2", label: "2" }, { value: "3", label: "3" }, { value: "4", label: "4" }]} />
            </div>
          </div>
        </details>
        <div className="mt-6 flex items-center gap-3">
          <button className="btn btn-ember ember btn-lg" disabled={desc.trim().length < 8 || !!running} onClick={run}>
            {running ? <Loader2 size={16} className="animate-spin" /> : <Wand2 size={16} />} Design voices
          </button>
          <span className="text-[12px] text-fg-3">First use downloads the voice designer (3.1 GB).</span>
        </div>
      </div>

      {design && (
        <div className="mt-8">
          {running && (
            <div className="card mb-4 p-5">
              <div className="mb-2 flex justify-between text-[13px]"><span className="flex items-center gap-2"><Loader2 size={14} className="animate-spin text-accent" /> {design.job?.message || "Queued…"}</span><span className="font-mono text-fg-3">{Math.round((design.job?.progress ?? 0) * 100)}%</span></div>
              <Progress value={design.job?.progress ?? 0} />
            </div>
          )}
          {design.status === "error" && <div className="card mb-4 border-danger/40 p-4 text-[13px] text-danger">{design.error}</div>}
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
            {design.previews.map((pv) => (
              <div key={pv.index} className="card animate-rise p-5">
                <div className="mb-4 flex items-center gap-3">
                  <VoiceOrb seed={design.id + pv.index} size={40} />
                  <div className="flex-1"><div className="text-[14px] font-medium">Take {pv.index + 1}</div><div className="text-[12px] text-fg-3">{pv.duration.toFixed(1)}s preview</div></div>
                  <button className="btn btn-primary btn-sm" onClick={() => { setSaveIdx(pv.index); setName(""); }}><Save size={13} /> Keep</button>
                </div>
                <Player src={api.designAudio(design.id, pv.file)} height={34} compact />
              </div>
            ))}
          </div>
          {design.status === "done" && (
            <button className="btn btn-secondary mt-4" onClick={run}><RefreshCw size={14} /> Try again with new takes</button>
          )}
        </div>
      )}

      <Modal open={saveIdx != null} onClose={() => setSaveIdx(null)} title="Save as a voice">
        <label className="label">Name</label>
        <input className="field" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Documentary narrator" onKeyDown={(e) => e.key === "Enter" && save()} />
        <p className="mt-2 text-[12px] text-fg-3">The take is analyzed like an uploaded voice, so every narration is quality-checked against it.</p>
        <div className="mt-5 flex justify-end gap-2">
          <button className="btn btn-ghost" onClick={() => setSaveIdx(null)}>Cancel</button>
          <button className="btn btn-primary" onClick={save}><Check size={14} /> Save voice</button>
        </div>
      </Modal>
    </div>
  );
}
