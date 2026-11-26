import type { Generation } from "../api";

/** Narrations of the same script in the same voice, newest first; the newest take leads the group. */
export type ScriptGroup = { key: string; latest: Generation; takes: Generation[] };

const keyOf = (g: Generation) => `${g.profile_id}|${(g.text_preview ?? "").toLowerCase().replace(/\s+/g, " ").trim()}`;

export function groupByScript(gens: Generation[]): ScriptGroup[] {
  const map = new Map<string, ScriptGroup>();
  for (const g of [...gens].sort((a, b) => b.created - a.created)) {
    const k = keyOf(g);
    const grp = map.get(k);
    if (grp) grp.takes.push(g);
    else map.set(k, { key: k, latest: g, takes: [g] });
  }
  return [...map.values()];
}

/** The plain status word people see for a narration. */
export function statusWord(g: Generation): { label: string; tone: "ok" | "warn" | "busy" | "muted" | "danger" } {
  if (g.status === "done") return g.outcome === "needs_listen" ? { label: "Needs review", tone: "warn" } : { label: "Done", tone: "ok" };
  if (g.status === "running" || g.status === "queued") return { label: g.status === "queued" ? "Waiting" : "Speaking", tone: "busy" };
  if (g.status === "error") return { label: "Didn't finish", tone: "danger" };
  return { label: "Stopped", tone: "muted" };
}

export const DRAFT_KEY = "vf-draft";
export const DRAFT_TITLE_KEY = "vf-draft-title";
