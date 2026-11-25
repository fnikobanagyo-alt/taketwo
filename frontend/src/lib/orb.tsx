// VoiceForge signature: every voice gets its own "ember orb", a deterministic
// gradient derived from the voice id. It breathes and spins while that voice speaks.

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
  return (
    <div
      className={`relative shrink-0 overflow-hidden rounded-full ${className}`}
      style={{
        width: size, height: size,
        boxShadow: active ? `0 0 ${size * 0.6}px -${size * 0.1}px ${c1}` : `inset 0 0 0 1px rgba(255,255,255,0.08)`,
        animation: active ? "orb-breathe 1.6s ease-in-out infinite" : undefined,
        transition: "box-shadow .3s",
      }}
    >
      <div
        className="absolute inset-[-25%]"
        style={{
          background: `conic-gradient(from ${rot}deg, ${c1}, ${c2}, ${c3}, ${c1})`,
          filter: `blur(${Math.max(3, size / 7)}px)`,
          animation: `orb-spin ${active ? 2.4 : 14}s linear infinite`,
        }}
      />
      <div className="absolute inset-0" style={{ background: `radial-gradient(circle at 32% 28%, rgba(255,255,255,.75), rgba(255,255,255,0) 42%)` }} />
      <div className="absolute inset-0" style={{ background: `radial-gradient(circle at 70% 80%, rgba(0,0,0,.28), rgba(0,0,0,0) 55%)` }} />
    </div>
  );
}
