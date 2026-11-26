import { useEffect, useState } from "react";
import { AlertCircle, ArrowUp, Check, Clock, Loader2, X } from "lucide-react";
import { api, type UpdateState } from "../api";
import { Progress, useToast } from "./ui";

/** Sidebar card for a new TakeTwo release: available → queued → updating → installed (restart), or failed. */
export function UpdateCard() {
  const toast = useToast();
  const [u, setU] = useState<UpdateState | null>(null);
  const [notes, setNotes] = useState(false);
  const [later, setLater] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const busy = u?.job?.status === "queued" || u?.job?.status === "running";

  useEffect(() => {
    let live = true;
    const load = () => api.update().then((s) => live && setU(s)).catch(() => {});
    load();
    const t = setInterval(load, busy ? 3000 : 60000);
    return () => { live = false; clearInterval(t); };
  }, [busy]);

  const act = (p: Promise<UpdateState>) => p.then(setU).catch((e) => toast(e.message));

  const restart = async () => {
    setRestarting(true);
    try { await api.updateRestart(); } catch (e) { setRestarting(false); toast((e as Error).message); return; }
    await new Promise((r) => setTimeout(r, 1500));
    for (let i = 0; i < 120; i++) {  // the engine comes back on the same address
      try { await api.health(); location.reload(); return; } catch { await new Promise((r) => setTimeout(r, 1000)); }
    }
    setRestarting(false);
    toast("TakeTwo didn't come back. Quit and open it again.");
  };

  if (!u) return null;
  const v = u.check?.latest ?? "";
  const job = u.job;
  let body: React.ReactNode = null;

  if (restarting) {
    body = <Head icon={<Loader2 size={13} className="animate-spin" />}>Restarting TakeTwo…</Head>;
  } else if (u.installed && !later) {
    body = (<>
      <Head icon={<Check size={13} />} tone="ok"><Ver v={u.installed} /> is installed</Head>
      <p className="mt-1 text-fg-2">Restart TakeTwo to start using it.</p>
      <div className="mt-2.5 flex items-center gap-1.5">
        <button className="btn btn-primary btn-sm" onClick={restart}>Restart now</button>
        <button className="btn btn-ghost btn-sm" onClick={() => setLater(true)}>Later</button>
      </div>
    </>);
  } else if (job?.status === "queued") {
    body = (<>
      <Head icon={<Clock size={13} />}>Update to <Ver v={v} /> is queued</Head>
      <p className="mt-1 text-fg-2">It starts when the work ahead of it finishes. Nothing is interrupted.</p>
      <div className="mt-2.5"><button className="btn btn-secondary btn-sm" onClick={() => act(api.updateCancel())}>Cancel update</button></div>
    </>);
  } else if (job?.status === "running") {
    body = (<>
      <Head icon={<Loader2 size={13} className="animate-spin" />}>Updating to <Ver v={v} /></Head>
      <p className="mt-1 text-fg">{job.message}</p>
      <Progress value={job.progress} className="mt-2" />
      <p className="mt-2 text-fg-2">Your voices and narrations are not touched.</p>
    </>);
  } else if (job?.status === "error" && u.available) {
    body = (<>
      <Head icon={<AlertCircle size={13} />} tone="danger">Update didn't finish</Head>
      <p className="mt-1 text-fg-2">{job.error} You're still on {u.current}.</p>
      <div className="mt-2.5"><button className="btn btn-primary btn-sm" onClick={() => act(api.updateStart())}>Try again</button></div>
    </>);
  } else if (u.available && !u.dismissed) {
    const lines = (u.check?.notes ?? "").split("\n").map((l) => l.replace(/^[#*\-\s]+/, "").trim()).filter(Boolean);
    body = (<>
      <Head icon={<ArrowUp size={13} />}>
        New version <Ver v={v} />
        <button className="icon-btn -my-1 -mr-1.5 ml-auto h-7 w-7" aria-label="Hide until the next version"
          onClick={() => api.saveSettings({ update_dismissed: v }).then(() => setU({ ...u, dismissed: true }))}><X size={14} /></button>
      </Head>
      {lines[0] && <p className="mt-1 text-fg-2">{lines[0]}</p>}
      {notes && (
        <ul className="mt-2 list-disc space-y-1 border-t border-line pl-4 pt-2 text-fg-2">
          {lines.slice(1, 8).map((l, i) => <li key={i}>{l}</li>)}
          <li className="list-none -ml-4"><a className="text-[var(--ember-ink)] underline underline-offset-2" href={u.check?.url} target="_blank" rel="noreferrer">Full notes on GitHub</a></li>
        </ul>
      )}
      <div className="mt-2.5 flex items-center gap-1.5">
        <button className="btn btn-primary btn-sm" onClick={() => act(api.updateStart())}>Update</button>
        <button className="btn btn-ghost btn-sm" aria-expanded={notes} onClick={() => setNotes(!notes)}>What's new</button>
      </div>
    </>);
  }
  if (!body) return null;
  return <section aria-live="polite" className="no-drag mx-3 rounded-xl border border-line-strong bg-surface p-3 text-[12.5px] shadow-soft">{body}</section>;
}

function Head({ icon, tone, children }: { icon: React.ReactNode; tone?: "ok" | "danger"; children: React.ReactNode }) {
  const c = tone === "ok" ? "bg-ok/12 text-ok" : tone === "danger" ? "bg-danger/10 text-danger" : "bg-accent-soft text-accent";
  return (
    <div className="flex items-center gap-2 text-[13px] font-semibold text-fg">
      <span className={`grid h-[22px] w-[22px] shrink-0 place-items-center rounded-[7px] ${c}`}>{icon}</span>
      {children}
    </div>
  );
}

function Ver({ v }: { v: string }) {
  return <span className="font-serif text-[19px] font-medium italic leading-none text-[var(--ember-ink)]">{v.replace(/\.0$/, "")}</span>;
}
