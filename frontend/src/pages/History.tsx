import { useEffect, useState } from "react";
import { AlertTriangle, ArrowLeft, ChevronDown, FolderOpen, History, Loader2, Play, RotateCcw, Search, Star, Trash2 } from "lucide-react";
import { api, fmtTime, timeAgo, type Generation } from "../api";
import { usePlayer } from "../lib/player";
import GenerationView from "../components/GenerationView";
import { Empty, PageHeader, Segmented, useToast } from "../components/ui";
import { groupByScript } from "../lib/groups";
import { SourcePill, StatusPill } from "../components/StatusPill";
import { go } from "../App";

export default function HistoryPage({ id }: { id?: string }) {
  const [items, setItems] = useState<Generation[] | null>(null);
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState<"all" | "fav" | "review">("all");
  const [openId, setOpenId] = useState<string | null>(null);  // the narration that is rolled down
  const player = usePlayer();
  const toast = useToast();
  const [expanded, setExpanded] = useState<string | null>(null);
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

  const groups = groupByScript((items ?? []).filter((g) =>
    (filter === "all" || (filter === "fav" && g.favorite) || (filter === "review" && g.outcome === "needs_listen")) &&
    ((g.title ?? "") + g.text_preview + g.profile_name).toLowerCase().includes(q.toLowerCase())));
  const play = (g: Generation) => player.play({ src: api.audioUrl(g.id, "wav", Math.round(g.updated)), title: g.title || g.profile_name, subtitle: g.profile_name, seed: g.profile_id, download: api.audioUrl(g.id, "mp3") + "&download=true" });
  const reveal = async (g?: Generation) => {
    try { g ? await api.revealNarration(g.id) : await api.revealNarrations(); }
    catch (e) { toast(`Couldn't open the folder: ${(e as Error).message}`, "error"); }
  };
  const remove = async (g: Generation) => {
    try {
      await api.deleteGeneration(g.id);
      if (openId === g.id) setOpenId(null);
      load();
      toast(`Deleted “${g.title || "narration"}”`, "info", { action: { label: "Undo", run: async () => { await api.restoreGeneration(g.id); load(); } } });
    } catch (e) { toast((e as Error).message, "error"); }
  };

  return (
    <div className="mx-auto max-w-5xl px-10 pb-16 pt-6">
      <PageHeader title="History" subtitle="Every narration you've made, grouped by script, with its audio, subtitles and per-part review."
        actions={<button className="btn btn-secondary btn-sm" onClick={() => reveal()} title="Open the folder TakeTwo keeps narrations in"><FolderOpen size={14} /> Open folder</button>} />
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <div className="relative w-72">
          <Search size={15} className="absolute left-3 top-3 text-fg-3" aria-hidden="true" />
          <label htmlFor="history-search" className="sr-only">Search narrations</label>
          <input id="history-search" className="field !pl-10" placeholder="Search narrations" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        <Segmented value={filter} onChange={setFilter} options={[{ value: "all", label: "All" }, { value: "fav", label: "Favorites" }, { value: "review", label: "Needs review" }]} />
      </div>
      {loadError ? (
        <Empty icon={<AlertTriangle size={20} />} title="Couldn't load your narrations" body={loadError}
          action={<button className="btn btn-secondary btn-sm" onClick={load}><RotateCcw size={13} /> Try again</button>} />
      ) : items === null ? (
        <Loader2 className="animate-spin text-fg-3" aria-label="Loading" />
      ) : !groups.length ? (
        <Empty icon={<History size={20} />} title={items.length ? "Nothing matches" : "No narrations yet"} body={items.length ? "Try another search or filter." : "Narrate your first script from Text to Speech."} />
      ) : (
        <ul className="card divide-y divide-line overflow-hidden">
          {groups.map(({ key, latest, takes }) => (
            <li key={key}>
              <Row g={latest} takes={takes.length} open={openId === latest.id} expanded={expanded === key}
                onOpen={() => setOpenId(openId === latest.id ? null : latest.id)} onPlay={() => play(latest)} onDelete={() => remove(latest)} onReveal={() => reveal(latest)}
                onExpand={takes.length > 1 ? () => setExpanded(expanded === key ? null : key) : undefined} />
              {openId === latest.id && <div className="border-t border-line bg-surface-2/30"><GenerationView id={latest.id} bare /></div>}
              {expanded === key && (
                <ul className="border-t border-line bg-surface-2/30" aria-label="Earlier takes of this script">
                  {takes.slice(1).map((g) => (
                    <li key={g.id} className="border-t border-line first:border-t-0">
                      <Row g={g} sub open={openId === g.id} onOpen={() => setOpenId(openId === g.id ? null : g.id)} onPlay={() => play(g)} onDelete={() => remove(g)} onReveal={() => reveal(g)} />
                      {openId === g.id && <div className="border-t border-line"><GenerationView id={g.id} bare /></div>}
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Row({ g, takes, sub, open, expanded, onOpen, onPlay, onDelete, onReveal, onExpand }: {
  g: Generation; takes?: number; sub?: boolean; open: boolean; expanded?: boolean; onOpen: () => void; onPlay: () => void; onDelete: () => void; onReveal: () => void; onExpand?: () => void;
}) {
  const name = g.title || g.text_preview || "Narration";
  return (
    <div className={`flex items-center gap-3 px-5 py-3 ${sub ? "pl-10" : ""}`}>
      <button className="btn btn-secondary btn-sm" disabled={g.status !== "done"} aria-label={`Play ${name}`} onClick={onPlay}><Play size={13} fill="currentColor" /></button>
      <button className="min-w-0 flex-1 rounded text-left" onClick={onOpen} aria-expanded={open}>
        <span className="flex items-center gap-2">
          <span className="truncate text-[13.5px] font-medium">{sub ? `Take from ${timeAgo(g.created)}` : name}</span>
          {g.favorite && <Star size={12} className="shrink-0 fill-accent text-ember-ink" aria-label="Favorite" />}
          <ChevronDown size={13} aria-hidden="true" className={`shrink-0 text-fg-3 transition ${open ? "rotate-180" : ""}`} />
        </span>
        <span className="mt-1 flex flex-wrap items-center gap-1.5 text-[12px] text-fg-2">
          <StatusPill g={g} />
          {g.outcome === "needs_listen" && g.review_count ? <span>{g.review_count} part{g.review_count > 1 ? "s" : ""}</span> : null}
          <SourcePill g={g} />
          <span>{g.profile_name} · {timeAgo(g.created)}</span>
        </span>
      </button>
      {onExpand && <button className="btn btn-ghost btn-sm" aria-expanded={expanded} onClick={onExpand}>{takes} takes <ChevronDown size={13} className={expanded ? "rotate-180" : ""} /></button>}
      <span className="w-12 text-right font-mono text-[12px] text-fg-2">{fmtTime(g.duration)}</span>
      <button className="icon-btn" title="Show this narration's audio file in its folder" aria-label={`Show ${name} in its folder`} disabled={g.status !== "done"} onClick={onReveal}><FolderOpen size={15} /></button>
      <button className="icon-btn" title="Delete" aria-label={`Delete ${name}`} onClick={onDelete}><Trash2 size={15} /></button>
    </div>
  );
}
