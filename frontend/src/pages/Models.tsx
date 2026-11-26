import { useCallback, useEffect, useState } from "react";
import { ArrowUpCircle, Check, Download, ExternalLink, Loader2, Plus, Search, Star, Trash2 } from "lucide-react";
import { api, type Discovered, type Engine } from "../api";
import { PageHeader, Progress, Segmented, useToast } from "../components/ui";

export default function ModelsPage() {
  const toast = useToast();
  const [engines, setEngines] = useState<Engine[]>([]);
  const [lib, setLib] = useState<{ installed: string; latest: string | null; update_available: boolean } | null>(null);
  const [upg, setUpg] = useState<{ status: string; message: string } | null>(null);
  const [found, setFound] = useState<Discovered[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [sort, setSort] = useState<"lastModified" | "trendingScore" | "downloads">("lastModified");
  const [repo, setRepo] = useState("");

  const load = useCallback(() => api.engines().then(setEngines), []);
  useEffect(() => { load(); api.library().then(setLib).catch(() => null); api.upgradeStatus().then(setUpg); }, [load]);
  useEffect(() => {
    if (upg?.status !== "running") return;
    const t = setInterval(() => api.upgradeStatus().then(setUpg), 2000);
    return () => clearInterval(t);
  }, [upg?.status]);

  const discover = async (s = sort) => {
    setSearching(true);
    try { setFound(await api.discover(s)); } catch (e) { toast((e as Error).message, "error"); }
    setSearching(false);
  };
  const add = async (r: string) => {
    try { await api.addCustom(r); await load(); setFound((f) => f?.map((x) => (x.repo === r ? { ...x, added: true } : x)) ?? null); setRepo(""); toast("Model added"); }
    catch (e) { toast((e as Error).message, "error"); }
  };

  return (
    <div className="mx-auto max-w-5xl px-10 pb-16 pt-6">
      <PageHeader title="Models" subtitle="Free, open-weights voice models running on Apple Silicon (MLX). Each downloads once, then works fully offline." />

      <div className="card relative mb-8 flex flex-wrap items-center justify-between gap-4 overflow-hidden p-5">
        <div>
          <div className="text-[14px] font-medium">Engine library · mlx-audio <span className="font-mono">{lib?.installed ?? "…"}</span></div>
          <div className="mt-0.5 text-[12.5px] text-fg-3">
            New model architectures arrive through library updates. {lib?.update_available ? <span className="text-ember-ink">Version {lib.latest} is available.</span> : lib?.latest ? "You're up to date." : ""}
          </div>
          {upg && upg.status !== "idle" && <div className={`mt-1 text-[12.5px] ${upg.status === "error" ? "text-danger-ink" : "text-ok-ink"}`}>{upg.message}</div>}
        </div>
        <button className="btn btn-secondary" disabled={!lib?.update_available || upg?.status === "running"} onClick={async () => setUpg(await api.upgrade())}>
          {upg?.status === "running" ? <Loader2 size={15} className="animate-spin" /> : <ArrowUpCircle size={15} />} Update library
        </button>
      </div>

      <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
        {engines.map((e) => <EngineCard key={e.id} e={e} onChange={load} />)}
      </div>

      <div className="mt-12 flex flex-wrap items-end justify-between gap-3">
        <div>
          <h2 className="text-[16px] font-semibold">Discover new models</h2>
          <p className="mt-1 text-[13px] text-fg-3">Live from Hugging Face. Anything your engine library supports can be added in one click.</p>
        </div>
        <div className="flex items-center gap-2">
          <Segmented value={sort} onChange={(s) => { setSort(s); if (found) discover(s); }} options={[{ value: "lastModified", label: "Newest" }, { value: "trendingScore", label: "Trending" }, { value: "downloads", label: "Popular" }]} />
          <button className="btn btn-primary" onClick={() => discover()} disabled={searching}>{searching ? <Loader2 size={15} className="animate-spin" /> : <Search size={15} />} Check now</button>
        </div>
      </div>
      <div className="mt-4 flex gap-2">
        <input className="field" placeholder="Or paste any Hugging Face repo, e.g. mlx-community/IndexTTS-2.5-fp16" value={repo} onChange={(e) => setRepo(e.target.value)} />
        <button className="btn btn-secondary" disabled={!repo.includes("/")} onClick={() => add(repo.trim())}><Plus size={15} /> Add</button>
      </div>
      {found && (
        <div className="card mt-4 max-h-[520px] divide-y divide-line overflow-y-auto">
          {found.map((m) => (
            <div key={m.repo} className="flex items-center gap-4 px-5 py-3">
              <div className="min-w-0 flex-1">
                <a href={`https://huggingface.co/${m.repo}`} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1.5 text-[13.5px] hover:text-ember-ink">{m.repo.replace("mlx-community/", "")} <ExternalLink size={11} className="text-fg-3" /></a>
                <div className="text-[12px] text-fg-3">
                  {m.downloads.toLocaleString()} downloads · updated {m.updated?.slice(0, 10)} · {m.supported ? <span className="text-ok-ink">supported ({m.architecture})</span> : "needs a library update"}
                </div>
              </div>
              {m.added ? <span className="chip bg-surface-2 text-fg-3"><Check size={11} /> Added</span> : <button className="btn btn-secondary btn-sm" disabled={!m.supported} onClick={() => add(m.repo)}><Plus size={13} /> Add</button>}
            </div>
          ))}
          {!found.length && <div className="p-5 text-[13px] text-fg-3">No results.</div>}
        </div>
      )}
    </div>
  );
}

function EngineCard({ e, onChange }: { e: Engine; onChange: () => void }) {
  const [prog, setProg] = useState<{ status: string; progress: number; error?: string } | null>(e.download?.status === "running" ? { status: "running", progress: 0 } : null);
  useEffect(() => {
    if (prog?.status !== "running") return;
    const t = setInterval(async () => {
      const p = await api.downloadProgress(e.id);
      setProg(p);
      if (p.status !== "running") onChange();
    }, 1000);
    return () => clearInterval(t);
  }, [prog?.status, e.id, onChange]);

  return (
    <div className="card flex flex-col p-5">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2 text-[14px] font-medium">
            {e.name}
            {e.recommended && <span className="chip bg-accent-soft text-ember-ink"><Star size={10} fill="currentColor" /> Recommended</span>}
            {e.kind === "design" && <span className="chip bg-surface-2 text-fg-2">Voice Design</span>}
            {e.loaded && <span className="chip bg-ok/12 text-ok-ink">In memory</span>}
            {e.custom && <span className="chip bg-surface-2 text-fg-3">Custom</span>}
          </div>
          <p className="mt-1.5 text-[12.5px] leading-relaxed text-fg-3">{e.description}</p>
        </div>
        {e.custom && <button className="icon-btn" title="Remove" aria-label="Remove" onClick={async () => { await api.removeCustom(e.id); onChange(); }}><Trash2 size={15} /></button>}
      </div>
      <div className="mt-auto flex items-center justify-between gap-3 pt-4">
        <div className="min-w-0 truncate text-[12px] text-fg-3">{e.license}{e.size_gb ? ` · ${e.size_gb} GB` : ""}</div>
        {e.downloaded ? (
          <span className="chip bg-ok/12 text-ok-ink"><Check size={11} /> Installed</span>
        ) : prog?.status === "running" ? (
          <div className="flex w-40 items-center gap-2"><Progress value={prog.progress} className="flex-1" /><span className="font-mono text-[12px] text-fg-3">{Math.round(prog.progress * 100)}%</span></div>
        ) : (
          <button className="btn btn-secondary btn-sm" onClick={async () => { await api.downloadEngine(e.id); setProg({ status: "running", progress: 0 }); }}><Download size={13} /> Download</button>
        )}
      </div>
      {prog?.status === "error" && <div className="mt-2 text-[12px] text-danger-ink">{prog.error}</div>}
    </div>
  );
}
