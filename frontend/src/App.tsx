import { useEffect, useState, type ReactNode } from "react";
import { AudioLines, Boxes, Captions, ChevronDown, History, Home, Info, Loader2, Mic2, Settings2, Sparkles, Wand2, X } from "lucide-react";
import { api, type Status } from "./api";
import { PlayerProvider, usePlayer } from "./lib/player";
import { Progress, ToastProvider, useToast } from "./components/ui";
import { onThemeChange } from "./lib/theme";
import { Logo } from "./lib/logo";
import { UpdateCard } from "./components/UpdateCard";
import HomePage from "./pages/Home";
import SpeechPage from "./pages/Speech";
import VoicesPage from "./pages/Voices";
import DesignPage from "./pages/Design";
import TranscribePage from "./pages/Transcribe";
import HistoryPage from "./pages/History";
import ModelsPage from "./pages/Models";
import SettingsPage from "./pages/Settings";

const PAGES = ["home", "speech", "voices", "design", "transcribe", "history", "models", "settings"] as const;
export type Page = (typeof PAGES)[number];
export type Route = { page: Page; id?: string };

function parse(): Route {
  const [page, id] = location.hash.replace(/^#\/?/, "").split("/");
  return { page: (PAGES as readonly string[]).includes(page) ? (page as Page) : "home", id };
}
export function go(r: Route) {
  location.hash = `/${r.page}${r.id ? "/" + r.id : ""}`;
}

const NAV: { group: string; items: { page: Page; label: string; icon: typeof Home }[] }[] = [
  { group: "", items: [{ page: "home", label: "Home", icon: Home }] },
  {
    group: "Create",
    items: [
      { page: "speech", label: "Text to Speech", icon: AudioLines },
      { page: "design", label: "Voice Design", icon: Wand2 },
      { page: "transcribe", label: "Speech to Text", icon: Captions },
    ],
  },
  {
    group: "Library",
    items: [
      { page: "voices", label: "Voices", icon: Mic2 },
      { page: "history", label: "History", icon: History },
    ],
  },
  {
    group: "Studio",
    items: [
      { page: "models", label: "Models", icon: Boxes },
      { page: "settings", label: "Settings", icon: Settings2 },
    ],
  },
];

export default function App() {
  return (
    <ToastProvider>
      <PlayerProvider>
        <Shell />
      </PlayerProvider>
    </ToastProvider>
  );
}

function DownloadToasts() {
  const toast = useToast();
  useEffect(() => {
    const f = (e: Event) => toast(<span>Saved <b>{(e as CustomEvent).detail}</b> to Downloads › TakeTwo</span>);
    window.addEventListener("vf-downloaded", f);
    return () => window.removeEventListener("vf-downloaded", f);
  }, [toast]);
  return null;
}

function Shell() {
  const [route, setRoute] = useState<Route>(parse());
  const [themeKey, force] = useState(0);
  const { track } = usePlayer();
  useEffect(() => {
    const f = () => setRoute(parse());
    window.addEventListener("hashchange", f);
    return () => window.removeEventListener("hashchange", f);
  }, []);
  // re-mount waveform players when the theme flips so their colors follow
  useEffect(() => onThemeChange(() => force((n) => n + 1)), []);

  return (
    <div className="app-shell flex h-full">
      <DownloadToasts />
      <Sidebar route={route} />
      <main className="app-main relative min-w-0 flex-1 overflow-y-auto" style={{ paddingBottom: track ? 76 : 0 }}>
        <div className="top-drag drag sticky top-0 z-30 h-3 w-full" />
        <div key={route.page + (route.id ?? "") + themeKey} className="animate-rise">
          {route.page === "home" && <HomePage />}
          {route.page === "speech" && <SpeechPage id={route.id} />}
          {route.page === "voices" && <VoicesPage id={route.id} />}
          {route.page === "design" && <DesignPage />}
          {route.page === "transcribe" && <TranscribePage id={route.id} />}
          {route.page === "history" && <HistoryPage id={route.id} />}
          {route.page === "models" && <ModelsPage />}
          {route.page === "settings" && <SettingsPage />}
        </div>
      </main>
    </div>
  );
}

function Sidebar({ route }: { route: Route }) {
  const desktop = !!window.vfDesktop?.isDesktop;
  return (
    <aside className="app-sidebar grain relative flex w-[232px] shrink-0 flex-col border-r border-line bg-panel">
      <div className={`drag flex items-center gap-2.5 px-5 ${desktop ? "pb-5 pt-12" : "py-5"}`}>
        <Logo size={32} />
        <div className="wordmark text-[20px] leading-none" aria-label="TakeTwo">Take<em>two</em></div>
      </div>

      <button onClick={() => go({ page: "speech" })} className="btn btn-ember ember no-drag mx-4 mb-4 h-10">
        <Sparkles size={15} /> New narration
      </button>

      <nav className="no-drag flex-1 space-y-5 overflow-y-auto px-3">
        {NAV.map((g) => (
          <div key={g.group || "top"}>
            {g.group && <div className="eyebrow mb-1.5 px-2.5">{g.group}</div>}
            <div className="space-y-0.5">
              {g.items.map(({ page, label, icon: Icon }) => {
                const on = route.page === page;
                return (
                  <NavItem key={page} on={on} onClick={() => go({ page })}>
                    <Icon size={16} strokeWidth={on ? 2.2 : 1.8} className={on ? "text-accent" : ""} />
                    {label}
                  </NavItem>
                );
              })}
            </div>
          </div>
        ))}
      </nav>
      <UpdateCard />
      <EngineStatus />
    </aside>
  );
}

function NavItem({ on, onClick, children }: { on: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={`flex h-9 w-full items-center gap-3 rounded-[10px] px-2.5 text-[13.5px] transition ${
        on ? "bg-surface font-medium text-fg shadow-soft" : "text-fg-2 hover:bg-surface-2 hover:text-fg"
      }`}
    >
      {children}
    </button>
  );
}

function EngineStatus() {
  const [s, setS] = useState<Status | null>(null);
  const [showQueue, setShowQueue] = useState(false);
  const load = () => api.status().then(setS).catch(() => setS(null));
  useEffect(() => {
    load();
    const t = setInterval(load, 2000);
    return () => clearInterval(t);
  }, []);
  const busy = s?.running;
  const waiting = (s?.queue ?? []).filter((q) => q.status === "queued");
  return (
    <div className="no-drag m-3 rounded-xl border border-line bg-surface/60 p-3">
      <div className="flex items-center gap-2 text-[12px]">
        {s === null ? (
          <span className="h-2 w-2 rounded-full bg-danger" />
        ) : busy ? (
          <Loader2 size={12} className="animate-spin text-accent" />
        ) : (
          <span className="relative flex h-2 w-2"><span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-ok opacity-40" /><span className="relative h-2 w-2 rounded-full bg-ok" /></span>
        )}
        <span className="font-medium text-fg">{s === null ? "Engine offline" : busy ? busy.label : "Ready"}</span>
        {busy && (
          <span className="ml-auto font-mono text-[11px] text-fg-3">
            {busy.total ? `${busy.done}/${busy.total}` : `${Math.round(busy.progress * 100)}%`}
          </span>
        )}
      </div>
      {busy?.title && <div className="mt-1.5 truncate text-[12px] font-medium text-fg-2">{busy.title}</div>}
      {busy && <Progress value={busy.progress} className="mt-2" />}
      <div className="mt-1.5 truncate text-[11.5px] text-fg-3">
        {s === null ? "Start TakeTwo to reconnect" : busy ? busy.message : s.loaded_engine ? `${s.loaded_engine} in memory` : "On-device · private"}
      </div>
      {(s?.duplicates ?? []).map((d) => (
        <div key={d.gid} className="mt-2 flex items-start gap-2 rounded-lg bg-accent-soft px-2.5 py-2 text-[11.5px] text-fg-2">
          <Info size={12} className="mt-0.5 shrink-0 text-accent" />
          <span className="min-w-0">Already in the queue, not added twice: <span className="text-fg">{d.title}</span></span>
        </div>
      ))}
      {waiting.length > 0 && (
        <div className="mt-2 border-t border-line pt-2">
          <button className="flex w-full items-center justify-between text-[11.5px] text-fg-2" onClick={() => setShowQueue((o) => !o)}>
            <span>{waiting.length} waiting</span>
            <ChevronDown size={12} className={showQueue ? "rotate-180 transition" : "transition"} />
          </button>
          {showQueue && waiting.map((q) => (
            <div key={q.id} className="mt-1.5 flex items-center gap-2 text-[11.5px]">
              <span className="min-w-0 flex-1 truncate text-fg-3">{q.title ?? q.label}</span>
              {q.gid && (
                <button className="icon-btn !h-6 !w-6" title="Stop this narration" onClick={async () => { await api.cancel(q.gid!); load(); }}>
                  <X size={12} />
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
