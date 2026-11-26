import { createContext, useCallback, useContext, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Check, ChevronDown, Pencil, X } from "lucide-react";

// ------------------------------------------------------------------ page header
export function PageHeader({ title, accent, subtitle, actions }: { title: string; accent?: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="mb-8 flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 className="display text-[40px] leading-[1.05] text-fg">
          {title} {accent && <em className="ember-ink">{accent}</em>}
        </h1>
        {subtitle && <p className="mt-2 max-w-2xl text-[14px] leading-relaxed text-fg-2">{subtitle}</p>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </header>
  );
}

// ------------------------------------------------------------------ slider
export function Slider({ value, min, max, step, onChange, onCommit }: { value: number; min: number; max: number; step: number; onChange: (v: number) => void; onCommit?: (v: number) => void }) {
  const p = ((value - min) / (max - min)) * 100;
  return (
    <input
      type="range" className="slider" min={min} max={max} step={step} value={value}
      style={{ ["--p" as string]: `${p}%` }}
      onChange={(e) => onChange(+e.target.value)}
      onPointerUp={(e) => onCommit?.(+(e.target as HTMLInputElement).value)}
      onKeyUp={(e) => onCommit?.(+(e.target as HTMLInputElement).value)}
    />
  );
}

export function Switch({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label?: string }) {
  return <button role="switch" aria-checked={on} aria-label={label} className="switch" data-on={on} onClick={() => onChange(!on)} />;
}

// ------------------------------------------------------------------ select (custom popover)
export type Option = { value: string; label: ReactNode; hint?: ReactNode; icon?: ReactNode; disabled?: boolean; trailing?: ReactNode };

export function Select({ value, options, onChange, placeholder, className = "", menuWidth, label }: { value: string; options: Option[]; onChange: (v: string) => void; placeholder?: string; className?: string; menuWidth?: number; label?: string }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const id = useId();
  useOutside(ref, () => setOpen(false));
  const cur = options.find((o) => o.value === value);
  const focusOpt = (d: number) => {
    const els = [...(list.current?.querySelectorAll<HTMLButtonElement>("[role=option]:not(:disabled)") ?? [])];
    const at = els.indexOf(document.activeElement as HTMLButtonElement);
    els[at < 0 ? Math.max(0, els.findIndex((e) => e.dataset.value === value)) : (at + d + els.length) % els.length]?.focus();
  };
  useEffect(() => { if (open) requestAnimationFrame(() => focusOpt(0)); }, [open]); // eslint-disable-line react-hooks/exhaustive-deps
  const close = () => { setOpen(false); trigger.current?.focus(); };
  return (
    <div ref={ref} className={`relative ${className}`}>
      {/* the trailing control (such as a voice preview) sits beside the trigger, never inside it */}
      <div className="field flex items-center gap-2 !pr-2">
        <button ref={trigger} type="button" aria-haspopup="listbox" aria-expanded={open} aria-controls={id} aria-label={label}
          className="flex h-full min-w-0 flex-1 items-center gap-2.5 text-left"
          onClick={() => setOpen((o) => !o)} onKeyDown={(e) => { if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); setOpen(true); } }}>
          {cur?.icon}
          <span className="min-w-0 flex-1 truncate">{cur?.label ?? <span className="text-fg-3">{placeholder}</span>}</span>
        </button>
        {cur?.trailing}
        <ChevronDown size={15} aria-hidden="true" className={`pointer-events-none shrink-0 text-fg-3 transition ${open ? "rotate-180" : ""}`} />
      </div>
      {open && (
        <div ref={list} id={id} role="listbox" aria-label={label}
          onKeyDown={(e) => {
            if (e.key === "Escape") { e.preventDefault(); close(); }
            else if (e.key === "ArrowDown") { e.preventDefault(); focusOpt(1); }
            else if (e.key === "ArrowUp") { e.preventDefault(); focusOpt(-1); }
            else if (e.key === "Tab") setOpen(false);
          }}
          className="animate-rise absolute left-0 z-50 mt-1.5 max-h-80 overflow-y-auto rounded-xl border border-line-strong bg-surface p-1.5 shadow-float" style={{ width: menuWidth ?? "100%" }}>
          {options.map((o) => (
            <div key={o.value} className={`flex items-center gap-1 rounded-lg transition hover:bg-surface-2 ${o.value === value ? "bg-surface-2" : ""}`}>
              <button type="button" role="option" aria-selected={o.value === value} data-value={o.value} disabled={o.disabled}
                onClick={() => { onChange(o.value); close(); }}
                className="flex min-w-0 flex-1 items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] disabled:opacity-40">
                {o.icon}
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-fg">{o.label}</span>
                  {o.hint && <span className="block truncate text-[12px] text-fg-3">{o.hint}</span>}
                </span>
                {o.value === value ? <Check size={14} className="shrink-0 text-ember-ink" aria-hidden="true" /> : <span className="w-3.5 shrink-0" />}
              </button>
              {o.trailing && <span className="pr-1.5">{o.trailing}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function useOutside(ref: React.RefObject<HTMLElement | null>, fn: () => void) {
  useEffect(() => {
    const h = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) fn(); };
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [ref, fn]);
}

// ------------------------------------------------------------------ segmented
export function Segmented<T extends string>({ value, options, onChange }: { value: T; options: { value: T; label: ReactNode }[]; onChange: (v: T) => void }) {
  return (
    <div className="inline-flex rounded-[11px] bg-surface-2 p-[3px]">
      {options.map((o) => (
        <button
          key={o.value} onClick={() => onChange(o.value)} aria-pressed={value === o.value}
          className={`h-7 rounded-lg px-3 text-[12.5px] font-medium transition ${value === o.value ? "bg-surface text-fg shadow-soft" : "text-fg-3 hover:text-fg"}`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ------------------------------------------------------------------ inline rename (explicit buttons)
export function InlineEdit({ value, onSave, className = "", inputClass = "", placeholder, multiline }: { value: string; onSave: (v: string) => Promise<unknown> | void; className?: string; inputClass?: string; placeholder?: string; multiline?: boolean }) {
  const [editing, setEditing] = useState(false);
  const [v, setV] = useState(value);
  const [busy, setBusy] = useState(false);
  useEffect(() => setV(value), [value]);
  const save = async () => {
    if (!v.trim() && !multiline) return;
    setBusy(true);
    try { await onSave(v.trim()); setEditing(false); } finally { setBusy(false); }
  };
  const cancel = () => { setV(value); setEditing(false); };

  if (!editing)
    return (
      <div className={`group flex min-w-0 items-center gap-2 ${className}`}>
        <span className={`min-w-0 truncate ${inputClass} ${!value ? "text-fg-3" : ""}`}>{value || placeholder}</span>
        <button className="icon-btn shrink-0" onClick={() => setEditing(true)} title="Edit" aria-label="Edit"><Pencil size={15} /></button>
      </div>
    );
  const common = {
    autoFocus: true, value: v, placeholder,
    onChange: (e: React.ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => setV(e.target.value),
    onKeyDown: (e: React.KeyboardEvent) => { if (e.key === "Enter" && !multiline) save(); if (e.key === "Escape") cancel(); },
  };
  return (
    <div className={`flex min-w-0 items-center gap-2 ${className}`}>
      {multiline ? <textarea {...common} rows={2} className="field flex-1" /> : <input {...common} className={`field h-auto flex-1 py-1 ${inputClass}`} />}
      <button className="btn btn-primary btn-sm" disabled={busy} onClick={save}><Check size={14} /> Save</button>
      <button className="btn btn-ghost btn-sm" onClick={cancel}><X size={14} /> Cancel</button>
    </div>
  );
}

// ------------------------------------------------------------------ progress
export function Progress({ value, className = "" }: { value: number; className?: string }) {
  return (
    <div className={`h-1.5 overflow-hidden rounded-full bg-surface-3 ${className}`}>
      <div className="ember h-full rounded-full transition-all duration-500" style={{ width: `${Math.max(2, Math.min(100, value * 100))}%` }} />
    </div>
  );
}

export function Empty({ icon, title, body, action }: { icon: ReactNode; title: string; body?: ReactNode; action?: ReactNode }) {
  return (
    <div className="card flex flex-col items-center px-8 py-16 text-center">
      <div className="mb-4 flex h-12 w-12 items-center justify-center rounded-2xl bg-surface-2 text-fg-2">{icon}</div>
      <div className="text-[15px] font-medium">{title}</div>
      {body && <div className="mt-1.5 max-w-sm text-[13px] leading-relaxed text-fg-3">{body}</div>}
      {action && <div className="mt-5">{action}</div>}
    </div>
  );
}

// ------------------------------------------------------------------ modal
export function Modal({ open, onClose, title, children, width = 480 }: { open: boolean; onClose: () => void; title: string; children: ReactNode; width?: number }) {
  const box = useRef<HTMLDivElement>(null);
  const id = useId();
  useEffect(() => {
    if (!open) return;
    const back = document.activeElement as HTMLElement | null;
    const items = () => [...(box.current?.querySelectorAll<HTMLElement>('button:not(:disabled), [href], input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])') ?? [])];
    requestAnimationFrame(() => (items()[1] ?? items()[0])?.focus()); // first control after Close
    const k = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); onClose(); return; }
      if (e.key !== "Tab") return;
      const els = items();
      if (!els.length) return;
      const first = els[0], last = els[els.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
    };
    window.addEventListener("keydown", k);
    return () => { window.removeEventListener("keydown", k); back?.focus?.(); };
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-6 backdrop-blur-sm" onMouseDown={onClose}>
      <div ref={box} role="dialog" aria-modal="true" aria-labelledby={id} className="animate-rise card w-full p-6 shadow-float" style={{ maxWidth: width }} onMouseDown={(e) => e.stopPropagation()}>
        <div className="mb-4 flex items-center justify-between">
          <h3 id={id} className="text-[16px] font-semibold">{title}</h3>
          <button className="icon-btn" onClick={onClose} aria-label="Close"><X size={16} /></button>
        </div>
        {children}
      </div>
    </div>
  );
}

/** Ask before an action that cannot be undone. Resolves true when confirmed. */
export function useConfirm() {
  const [ask, setAsk] = useState<{ title: string; body: ReactNode; action: string; resolve: (v: boolean) => void } | null>(null);
  const confirm = useCallback((title: string, body: ReactNode, action = "Delete") => new Promise<boolean>((resolve) => setAsk({ title, body, action, resolve })), []);
  const done = (v: boolean) => { ask?.resolve(v); setAsk(null); };
  const dialog = (
    <Modal open={!!ask} onClose={() => done(false)} title={ask?.title ?? ""}>
      <div className="text-[13.5px] leading-relaxed text-fg-2">{ask?.body}</div>
      <div className="mt-6 flex justify-end gap-2">
        <button className="btn btn-secondary" onClick={() => done(false)}>Keep it</button>
        <button className="btn btn-primary" onClick={() => done(true)}>{ask?.action}</button>
      </div>
    </Modal>
  );
  return [confirm, dialog] as const;
}

// ------------------------------------------------------------------ toasts
type ToastOpts = { action?: { label: string; run: () => void }; ms?: number };
type Toast = { id: number; text: ReactNode; tone: "ok" | "error" | "info" } & ToastOpts;
const ToastCtx = createContext<(text: ReactNode, tone?: Toast["tone"], opts?: ToastOpts) => void>(() => {});
export const useToast = () => useContext(ToastCtx);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const drop = (id: number) => setItems((x) => x.filter((t) => t.id !== id));
  const push = useCallback((text: ReactNode, tone: Toast["tone"] = "ok", opts: ToastOpts = {}) => {
    const id = Date.now() + Math.random();
    setItems((x) => [...x, { id, text, tone, ...opts }]);
    setTimeout(() => drop(id), opts.ms ?? (opts.action ? 6000 : 3500));
  }, []);
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div role="status" aria-live="polite" className="pointer-events-none fixed right-5 top-5 z-[60] flex flex-col items-end gap-2">
        {items.map((t) => (
          <div key={t.id} className="animate-rise pointer-events-auto flex items-center gap-2.5 rounded-xl border border-line-strong bg-surface px-4 py-2.5 text-[13px] shadow-float">
            <span aria-hidden="true" className={`h-1.5 w-1.5 rounded-full ${t.tone === "error" ? "bg-danger" : t.tone === "info" ? "bg-fg-3" : "bg-ok"}`} />
            {t.text}
            {t.action && <button className="btn btn-ghost btn-sm -my-1 -mr-2 font-semibold text-fg" onClick={() => { t.action!.run(); drop(t.id); }}>{t.action.label}</button>}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}
