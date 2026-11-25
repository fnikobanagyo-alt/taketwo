import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { Check, ChevronDown, Pencil, X } from "lucide-react";

// ------------------------------------------------------------------ page header
export function PageHeader({ title, accent, subtitle, actions }: { title: string; accent?: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="mb-8 flex flex-wrap items-end justify-between gap-4">
      <div>
        <h1 className="display text-[40px] leading-[1.05] text-fg">
          {title} {accent && <em className="ember-text pr-1">{accent}</em>}
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

export function Switch({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return <button role="switch" aria-checked={on} className="switch" data-on={on} onClick={() => onChange(!on)} />;
}

// ------------------------------------------------------------------ select (custom popover)
export type Option = { value: string; label: ReactNode; hint?: ReactNode; icon?: ReactNode; disabled?: boolean; trailing?: ReactNode };

export function Select({ value, options, onChange, placeholder, className = "", menuWidth }: { value: string; options: Option[]; onChange: (v: string) => void; placeholder?: string; className?: string; menuWidth?: number }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useOutside(ref, () => setOpen(false));
  const cur = options.find((o) => o.value === value);
  return (
    <div ref={ref} className={`relative ${className}`}>
      <button type="button" className="field flex items-center gap-2.5 text-left" onClick={() => setOpen((o) => !o)}>
        {cur?.icon}
        <span className="min-w-0 flex-1 truncate">{cur?.label ?? <span className="text-fg-3">{placeholder}</span>}</span>
        {cur?.trailing}
        <ChevronDown size={15} className={`shrink-0 text-fg-3 transition ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <div className="animate-rise absolute left-0 z-50 mt-1.5 max-h-80 overflow-y-auto rounded-xl border border-line-strong bg-surface p-1.5 shadow-float" style={{ width: menuWidth ?? "100%" }}>
          {options.map((o) => (
            <button
              key={o.value} type="button" disabled={o.disabled}
              onClick={() => { onChange(o.value); setOpen(false); }}
              className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left text-[13px] transition hover:bg-surface-2 disabled:opacity-40 ${o.value === value ? "bg-surface-2" : ""}`}
            >
              {o.icon}
              <span className="min-w-0 flex-1">
                <span className="block truncate text-fg">{o.label}</span>
                {o.hint && <span className="block truncate text-[11.5px] text-fg-3">{o.hint}</span>}
              </span>
              {o.trailing}
              {o.value === value ? <Check size={14} className="shrink-0 text-accent" /> : <span className="w-3.5 shrink-0" />}
            </button>
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
          key={o.value} onClick={() => onChange(o.value)}
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
        <button className="icon-btn shrink-0" onClick={() => setEditing(true)} title="Edit"><Pencil size={15} /></button>
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
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-6 backdrop-blur-sm" onMouseDown={onClose}>
      <div className="animate-rise card w-full p-6 shadow-float" style={{ maxWidth: width }} onMouseDown={(e) => e.stopPropagation()}>
        <div className="mb-4 flex items-center justify-between">
          <h3 className="text-[16px] font-semibold">{title}</h3>
          <button className="icon-btn" onClick={onClose}><X size={16} /></button>
        </div>
        {children}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ toasts
type Toast = { id: number; text: ReactNode; tone: "ok" | "error" | "info" };
const ToastCtx = createContext<(text: ReactNode, tone?: Toast["tone"]) => void>(() => {});
export const useToast = () => useContext(ToastCtx);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<Toast[]>([]);
  const push = useCallback((text: ReactNode, tone: Toast["tone"] = "ok") => {
    const id = Date.now() + Math.random();
    setItems((x) => [...x, { id, text, tone }]);
    setTimeout(() => setItems((x) => x.filter((t) => t.id !== id)), 3500);
  }, []);
  return (
    <ToastCtx.Provider value={push}>
      {children}
      <div className="pointer-events-none fixed right-5 top-5 z-[60] flex flex-col items-end gap-2">
        {items.map((t) => (
          <div key={t.id} className="animate-rise pointer-events-auto flex items-center gap-2.5 rounded-xl border border-line-strong bg-surface px-4 py-2.5 text-[13px] shadow-float">
            <span className={`h-1.5 w-1.5 rounded-full ${t.tone === "error" ? "bg-danger" : t.tone === "info" ? "bg-fg-3" : "bg-ok"}`} />
            {t.text}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}
