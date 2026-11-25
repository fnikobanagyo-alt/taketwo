import { useEffect, useState } from "react";
import { AlertTriangle, ArrowLeft, ChevronDown, History, Loader2, Play, RotateCcw, Search, Star, Trash2 } from "lucide-react";
import { api, fmtTime, timeAgo, type Generation } from "../api";
import VoiceOrb from "../lib/orb";
import { usePlayer } from "../lib/player";
import GenerationView from "../components/GenerationView";
import { Empty, PageHeader, Segmented } from "../components/ui";
import { go } from "../App";

export default function HistoryPage({ id }: { id?: string }) {
  const [items, setItems] = useState<Generation[] | null>(null);
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<"all" | "fav" | "review">("all");
  const [openId, setOpenId] = useState<string | null>(null);  // the narration that is rolled down
  const player = usePlayer();
  const [loadError, setLoadError] = useState<string | null>(null);
  const load = () => api.generations()
    .then((x) => { setItems(x); setLoadError(null); })
    .catch((e) => setLoadError((e as Error).message));
  useEffect(() => { load(); }, [id]);

  if (id)
    return (
      <div className="mx-auto max-w-5xl px-10 pb-16 pt-6">
        <button className="btn btn-ghost btn-sm -ml-2 mb-4" onClick={() => go({ page: "history" })}><ArrowLeft size={14} /> History</button>
        <GenerationView id={id} />
      </div>
    );

  const list = (items ?? []).filter((g) =>
    (filter === "all" || (filter === "fav" && g.favorite) || (filter === "review" && g.outcome === "needs_listen")) &&
    ((g.title ?? "") + g.text_preview + g.profile_name).toLowerCase().includes(q.toLowerCase()));

  return (
    <div className="mx-auto max-w-5xl px-10 pb-16 pt-6">
      <PageHeader title="History" subtitle="Every narration you've made, with its audio, subtitles and per-part quality report." />
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <div className="relative w-72">
          <Search size={15} className="absolute left-3 top-3 text-fg-3" />
          <input className="field !pl-10" placeholder="Search narrations" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        <Segmented value={filter} onChange={setFilter} options={[{ value: "all", label: "All" }, { value: "fav", label: "Favorites" }, { value: "review", label: "Needs review" }]} />
      </div>
      {loadError ? (
        <Empty icon={<AlertTriangle size={20} />} title="Couldn't load your narrations" body={loadError}
          action={<button className="btn btn-secondary btn-sm" onClick={load}><RotateCcw size={13} /> Try again</button>} />
      ) : items === null ? (
        <Loader2 className="animate-spin text-fg-3" />
      ) : !list.length ? (
        <Empty icon={<History size={20} />} title={items.length ? "Nothing matches" : "No narrations yet"} body={items.length ? "Try another search or filter." : "Generate your first narration from Text to Speech."} />
      ) : (
        <div className="card divide-y divide-line overflow-hidden">
          {list.map((g) => (
            <div key={g.id}>
            <div className="group flex items-center gap-4 px-5 py-3.5 transition hover:bg-surface-2/60">
              <button
                className="relative shrink-0" disabled={g.status !== "done"}
                onClick={() => player.play({ src: api.audioUrl(g.id, "wav", Math.round(g.updated)), title: g.title || g.profile_name, subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" })}
              >
                <VoiceOrb seed={g.profile_id} size={38} />
                {g.status === "done" && <span className="absolute inset-0 flex items-center justify-center rounded-full bg-black/45 text-white opacity-0 transition group-hover:opacity-100"><Play size={14} fill="currentColor" /></span>}
              </button>
              <button className="min-w-0 flex-1 text-left" onClick={() => setOpenId(openId === g.id ? null : g.id)} aria-expanded={openId === g.id}>
                <div className="flex items-center gap-2">
                  <span className="truncate text-[13.5px] font-medium">{g.title || g.text_preview}</span>
                  {g.favorite && <Star size={12} className="shrink-0 fill-accent text-accent" />}
                  <ChevronDown size={13} className={`shrink-0 text-fg-3 transition ${openId === g.id ? "rotate-180" : ""}`} />
                </div>
                <div className="mt-0.5 flex items-center gap-2 text-[12px] text-fg-3">
                  <span>{g.profile_name}</span>·<span>{g.engine}</span>·<span>{timeAgo(g.created)}</span>
                  {g.outcome === "needs_listen" ? <span className="flex items-center gap-1 text-warn"><AlertTriangle size={11} /> Done, needs a listen ({g.review_count})</span> : null}
                  {g.status !== "done" && <span className="text-accent">{g.status}</span>}
                </div>
              </button>
              <span className="font-mono text-[12px] text-fg-3">{fmtTime(g.duration)}</span>
              <button className="icon-btn opacity-0 transition group-hover:opacity-100" title="Delete"
                onClick={async () => { if (confirm("Delete this narration?")) { await api.deleteGeneration(g.id); if (openId === g.id) setOpenId(null); load(); } }}>
                <Trash2 size={15} />
              </button>
            </div>
            {openId === g.id && (
              <div className="border-t border-line bg-surface-2/30">
                <GenerationView id={g.id} bare />
              </div>
            )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
