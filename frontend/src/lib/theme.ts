export type ThemePref = "system" | "light" | "dark";
const KEY = "vf-theme";
const mq = window.matchMedia("(prefers-color-scheme: dark)");
const subs = new Set<() => void>();

export function getPref(): ThemePref {
  try { return (localStorage.getItem(KEY) as ThemePref) || "system"; } catch { return "system"; }
}

export function resolved(): "light" | "dark" {
  const p = getPref();
  return p === "system" ? (mq.matches ? "dark" : "light") : p;
}

export function apply() {
  document.documentElement.dataset.theme = resolved();
  subs.forEach((f) => f());
}

export function setPref(p: ThemePref) {
  try { localStorage.setItem(KEY, p); } catch { /* ignore */ }
  apply();
}

export function onThemeChange(f: () => void) { subs.add(f); return () => { subs.delete(f); }; }

mq.addEventListener("change", apply);
apply();
