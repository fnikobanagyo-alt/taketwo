// TakeTwo signature: every voice gets its own "ember orb", a deterministic
// gradient derived from the voice id. It is still when idle and breathes and spins only while that voice speaks.

function hash(s: string) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

// Curated palette families so every orb feels on-brand (warm-led, with cool counterpoints).
const FAMILIES: [number, number, number][] = [
  [28, 350, 45], // ember: orange, rose, gold
  [14, 330, 38], // coral dusk
  [40, 20, 200], // amber + teal
  [340, 280, 25], // rose, violet, orange
  [200, 260, 30], // ocean, indigo, ember
  [160, 45, 20], // jade, gold, flame
  [265, 330, 15], // violet dusk
  [5, 45, 300], // crimson, sun, orchid
];

export function orbColors(seed: string) {
  const h = hash(seed);
  const [a, b, c] = FAMILIES[h % FAMILIES.length];
  const j = ((h >> 8) % 20) - 10;
  return [`hsl(${a + j} 95% 62%)`, `hsl(${b + j} 85% 58%)`, `hsl(${c + j} 90% 70%)`, (h >> 16) % 360] as const;
}

export default function VoiceOrb({ seed, size = 40, active = false, className = "" }: { seed: string; size?: number; active?: boolean; className?: string }) {
  const [c1, c2, c3, rot] = orbColors(seed);
  const shine = `radial-gradient(circle at 32% 28%, rgba(255,255,255,.7), rgba(255,255,255,0) 42%), radial-gradient(circle at 70% 80%, rgba(0,0,0,.28), rgba(0,0,0,0) 55%)`;
  if (!active) {
    // Idle orbs are one painted layer: no blur, no animation (lists can hold dozens of them).
    return (
      <div aria-hidden="true" className={`shrink-0 rounded-full ${className}`}
        style={{ width: size, height: size, boxShadow: "inset 0 0 0 1px rgba(255,255,255,0.08)",
          background: `${shine}, radial-gradient(circle at 28% 72%, ${c3}, transparent 62%), radial-gradient(circle at 78% 30%, ${c2}, transparent 66%), ${c1}` }} />
    );
  }
  return (
    <div aria-hidden="true" className={`relative shrink-0 overflow-hidden rounded-full ${className}`}
      style={{ width: size, height: size, boxShadow: `0 ${size * 0.08}px ${size * 0.5}px -${size * 0.12}px ${c1}`, animation: "orb-breathe 1.6s ease-in-out infinite" }}>
      <div className="absolute inset-[-25%]"
        style={{ background: `conic-gradient(from ${rot}deg, ${c1}, ${c2}, ${c3}, ${c1})`, filter: `blur(${Math.max(3, size / 7)}px)`, animation: "orb-spin 2.4s linear infinite" }} />
      <div className="absolute inset-0" style={{ background: shine }} />
    </div>
  );
}
