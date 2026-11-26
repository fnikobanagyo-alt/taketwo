import { useEffect, useState } from "react";
import { AlertTriangle, Check, Copy, Monitor, Moon, Plus, RotateCcw, Sun, Trash2 } from "lucide-react";
import { api, type Settings, type UpdateState } from "../api";
import { getPref, setPref, type ThemePref } from "../lib/theme";
import { Empty, PageHeader, Slider, Switch, useToast } from "../components/ui";

type NumKey = { [K in keyof Settings]: Settings[K] extends number ? K : never }[keyof Settings];

const QC: { key: NumKey; label: string; min: number; max: number; step: number; fmt: (v: number) => string; help: string }[] = [
  { key: "max_chunk_chars", label: "Part size (per model call)", min: 100, max: 400, step: 10, fmt: (v) => `${v} chars`,
    help: "Not a script limit. Scripts can be up to 10,000 words; they are split into parts of this size and joined seamlessly. 180–250 is the sweet spot. Bigger parts make models drift, pause or garble." },
  { key: "max_attempts", label: "Takes per part", min: 1, max: 8, step: 1, fmt: (v) => `${v}`, help: "How many times a part that fails a check is spoken again before the best take is kept and marked Needs review." },
  { key: "compare_takes", label: "Takes to compare", min: 1, max: 3, step: 1, fmt: (v) => (v === 1 ? "1 · fastest" : `${v} · closest match`),
    help: "Record this many clean takes of every part and keep the one that sounds most like the real voice. 2 gives a noticeably more exact clone at about twice the render time." },
  { key: "max_wer", label: "Word error tolerance", min: 0.02, max: 0.4, step: 0.01, fmt: (v) => `${Math.round(v * 100)}%`, help: "Whisper re-listens to every take. Above this share of wrong or missing words, the take is redone." },
  { key: "min_speaker_similarity", label: "Voice match threshold", min: 0.6, max: 0.98, step: 0.01, fmt: (v) => `${Math.round(v * 100)}%`, help: "Default only. Each narration's Similarity slider sets its own bar (70–90%)." },
  { key: "max_internal_silence", label: "Longest allowed pause", min: 0.4, max: 2.5, step: 0.1, fmt: (v) => `${v.toFixed(1)}s`, help: "Anything longer inside a part is treated as a dropout and redone." },
];
const MASTER: typeof QC = [
  { key: "sentence_pause_ms", label: "Pause between parts", min: 0, max: 1200, step: 25, fmt: (v) => `${v} ms`, help: "" },
  { key: "paragraph_pause_ms", label: "Pause between paragraphs", min: 0, max: 2500, step: 50, fmt: (v) => `${v} ms`, help: "" },
  { key: "target_lufs", label: "Loudness", min: -24, max: -12, step: 1, fmt: (v) => `${v} LUFS`, help: "-16 LUFS suits YouTube and podcasts; -14 is louder." },
];

export default function SettingsPage() {
  const toast = useToast();
  const [s, setS] = useState<Settings | null>(null);
  const [theme, setTheme] = useState<ThemePref>(getPref());
  const [err, setErr] = useState<string | null>(null);
  const load = () => { setErr(null); api.settings().then(setS).catch((e) => setErr((e as Error).message)); };
  useEffect(load, []);
  if (err)
    return (
      <div className="mx-auto max-w-3xl px-10 pb-16 pt-6">
        <Empty icon={<AlertTriangle size={20} />} title="Settings couldn't load" body={`${err}. TakeTwo's engine may still be starting, or it was closed.`}
          action={<button className="btn btn-secondary btn-sm" onClick={load}><RotateCcw size={13} /> Try again</button>} />
      </div>
    );
  if (!s) return <div className="mx-auto max-w-3xl px-10 pt-6 text-[13px] text-fg-2" role="status">Loading…</div>;

  const save = async (patch: Partial<Settings>) => { setS(await api.saveSettings(patch)); toast("Saved"); };
  const setPron = (list: Settings["pronunciations"]) => setS({ ...s, pronunciations: list });
  const rows = (items: typeof QC) => items.map((x) => (
    <div key={x.key} className="grid grid-cols-12 items-center gap-6 px-5 py-4">
      <div className="col-span-6">
        <div className="text-[13.5px] font-medium">{x.label}</div>
        {x.help && <div className="mt-0.5 text-[12px] leading-snug text-fg-3">{x.help}</div>}
      </div>
      <div className="col-span-4"><Slider value={s[x.key]} min={x.min} max={x.max} step={x.step} onChange={(v) => setS({ ...s, [x.key]: v })} onCommit={(v) => save({ [x.key]: v })} /></div>
      <div className="col-span-2 text-right font-mono text-[12.5px] text-fg-2">{x.fmt(s[x.key])}</div>
    </div>
  ));

  return (
    <div className="mx-auto max-w-4xl px-10 pb-16 pt-6">
      <PageHeader title="Settings" />

      <Section title="Appearance">
        <div className="flex items-center justify-between px-5 py-4">
          <div><div className="text-[13.5px] font-medium">Theme</div><div className="text-[12px] text-fg-3">Match your computer, or pick one.</div></div>
          <div className="flex gap-2">
            {([["system", Monitor, "System"], ["light", Sun, "Light"], ["dark", Moon, "Dark"]] as const).map(([v, Icon, l]) => (
              <button key={v} onClick={() => { setPref(v); setTheme(v); }} className={`btn btn-sm border ${theme === v ? "border-accent bg-accent-soft text-fg" : "border-line-strong text-fg-2 hover:bg-surface-2"}`}>
                <Icon size={14} /> {l}
              </button>
            ))}
          </div>
        </div>
      </Section>

      <Section title="Quality control" note="Applied to every part of every narration.">{rows(QC)}</Section>
      <Section title="Mastering">
        {rows(MASTER)}
        <div className="flex items-center justify-between px-5 py-4">
          <div><div className="text-[13.5px] font-medium">Remove citation markers</div><div className="text-[12px] text-fg-3">Strips [1], [2,3] and similar so they're never read aloud.</div></div>
          <Switch on={s.strip_citations} onChange={(v) => save({ strip_citations: v })} label="Remove citations" />
        </div>
      </Section>

      <Section title="Pronunciation dictionary" note="Swap hard words for how they should sound, before speaking. Whole words, case-insensitive.">
        <div className="space-y-2 p-5">
          {s.pronunciations.length === 0 && <div className="text-[12.5px] text-fg-3">e.g. Völuspá → Vuh-loo-spah · Fimbulwinter → Fim-bul-winter</div>}
          {s.pronunciations.map((p, i) => (
            <div key={i} className="flex items-center gap-2">
              <input className="field" placeholder="Written" value={p.from} onChange={(e) => setPron(s.pronunciations.map((q, k) => (k === i ? { ...q, from: e.target.value } : q)))} onBlur={() => save({ pronunciations: s.pronunciations })} />
              <span className="text-fg-3">→</span>
              <input className="field" placeholder="Spoken" value={p.to} onChange={(e) => setPron(s.pronunciations.map((q, k) => (k === i ? { ...q, to: e.target.value } : q)))} onBlur={() => save({ pronunciations: s.pronunciations })} />
              <button className="icon-btn" aria-label={`Remove ${p.from || "this word"}`} onClick={() => save({ pronunciations: s.pronunciations.filter((_, k) => k !== i) })}><Trash2 size={15} /></button>
            </div>
          ))}
          <button className="btn btn-ghost btn-sm -ml-2" onClick={() => setPron([...s.pronunciations, { from: "", to: "" }])}><Plus size={14} /> Add word</button>
        </div>
      </Section>

      <ConnectApps />

      <Updates on={s.check_updates} onChange={(v) => save({ check_updates: v })} />

      <p className="mt-10 text-center text-[12px] text-fg-3">TakeTwo (formerly VoiceForge) · everything runs on this computer · your voices never leave it</p>
    </div>
  );
}

function Updates({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  const toast = useToast();
  const [u, setU] = useState<UpdateState | null>(null);
  const [checking, setChecking] = useState(false);
  useEffect(() => { api.update().then(setU).catch(() => setU(null)); }, []);
  const checkNow = async () => {
    setChecking(true);
    try {
      const s = await api.updateCheck();
      setU(s);
      toast(s.available ? `TakeTwo ${s.check?.latest} is available: see the sidebar.` : s.check?.error ? "Couldn't reach GitHub. Try again later." : "You have the latest version.");
    } catch (e) { toast((e as Error).message); } finally { setChecking(false); }
  };
  const when = u?.check ? new Date(u.check.checked_at * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }) : "not yet";
  return (
    <Section title="Updates">
      <div className="flex items-start justify-between gap-6 px-5 py-4">
        <div>
          <div className="text-[13.5px] font-medium">Check for new versions</div>
          <p className="mt-0.5 max-w-xl text-[12.5px] text-fg-2">Once when TakeTwo opens, it asks GitHub whether a newer release exists. Nothing about you, your voices or your scripts is sent.</p>
        </div>
        <Switch on={on} onChange={onChange} label="Check for new versions" />
      </div>
      <div className="flex items-center justify-between gap-6 px-5 py-4">
        <div>
          <div className="text-[13.5px] font-medium">Version</div>
          <p className="mt-0.5 text-[12.5px] text-fg-2">TakeTwo {u?.current ?? "…"} · checked {when}</p>
        </div>
        <button className="btn btn-secondary btn-sm" disabled={checking} onClick={checkNow}>{checking ? "Checking…" : "Check now"}</button>
      </div>
    </Section>
  );
}

function ConnectApps() {
  const toast = useToast();
  const [data, setData] = useState<Awaited<ReturnType<typeof api.integrations>> | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => { api.integrations().then(setData).catch(() => setData(null)); }, []);
  if (!data) return null;
  return (
    <Section title="Connect to AI apps" note="Lets Claude list your voices and make narrations through TakeTwo (MCP). Everything stays on this computer.">
      {data.apps.map((a) => (
        <div key={a.id} className="px-5 py-4">
          <div className="flex items-center justify-between gap-4">
            <div className="text-[13.5px] font-medium">{a.name}</div>
            <div className="flex items-center gap-2">
              {a.connected
                ? <span className="chip bg-ok/12 text-ok-ink"><Check size={11} /> Connected</span>
                : <span className="chip bg-surface-2 text-fg-3">Not set up</span>}
              <button className="btn btn-ghost btn-sm" onClick={() => setOpen(open === a.id ? null : a.id)}>{open === a.id ? "Hide setup" : "Setup"}</button>
            </div>
          </div>
          {open === a.id && (
            <div className="mt-3">
              <p className="mb-2 text-[12px] text-fg-3">{a.how}</p>
              <div className="relative">
                <pre className="overflow-x-auto rounded-lg bg-surface-2 p-3 pr-12 font-mono text-[12px] leading-relaxed text-fg-2">{a.setup}</pre>
                <button className="icon-btn absolute right-2 top-2" title="Copy" aria-label="Copy"
                  onClick={() => navigator.clipboard.writeText(a.setup).then(() => toast("Copied"), () => toast("Copy failed", "error"))}>
                  <Copy size={14} />
                </button>
              </div>
            </div>
          )}
        </div>
      ))}
      {!data.script_found && <div className="px-5 py-3 text-[12px] text-warn-ink">The MCP server file (mcp/taketwo_mcp.py) is missing.</div>}
    </Section>
  );
}

function Section({ title, note, children }: { title: string; note?: string; children: React.ReactNode }) {
  return (
    <section className="mb-8">
      <h2 className="text-[15px] font-semibold">{title}</h2>
      {note && <p className="mt-0.5 text-[12.5px] text-fg-3">{note}</p>}
      <div className="card mt-3 divide-y divide-line">{children}</div>
    </section>
  );
}
