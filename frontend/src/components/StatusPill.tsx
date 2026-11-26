import type { Generation } from "../api";
import { statusWord } from "../lib/groups";

const TONE = { ok: "bg-ok/12 text-ok-ink", warn: "bg-warn/15 text-warn-ink", busy: "bg-accent-soft text-ember-ink", muted: "bg-surface-2 text-fg-2", danger: "bg-danger/10 text-danger-ink" };

export function StatusPill({ g }: { g: Generation }) {
  const s = statusWord(g);
  return <span className={`chip ${TONE[s.tone]}`}>{s.label}</span>;
}

export function SourcePill({ g }: { g: Generation }) {
  return g.source === "mcp" ? <span className="chip border border-line-strong text-fg-2" title="Made by an AI app through MCP">MCP</span> : null;
}
