import { useId } from "react";

// TakeTwo mark: one waveform, the first half your recording (muted), the second half the cloned voice (ember).
export function Logo({ size = 32 }: { size?: number }) {
  const g = useId();
  return (
    <svg width={size} height={size} viewBox="0 0 40 40" aria-hidden="true" className="shrink-0">
      <defs>
        <linearGradient id={g} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#ffc061" /><stop offset=".48" stopColor="#ff7a2f" /><stop offset="1" stopColor="#e4486e" />
        </linearGradient>
      </defs>
      <rect width="40" height="40" rx="11" fill="#1d1a17" />
      <rect x=".5" y=".5" width="39" height="39" rx="10.5" fill="none" stroke="#fff" strokeOpacity=".09" />
      <path d="M8 18v4M11.5 14v12M15 16.5v7M18.5 12v16" stroke="#f4efe9" strokeOpacity=".5" strokeWidth="2.3" strokeLinecap="round" />
      <path d="M21.5 12v16M25 15v10M28.5 11v18M32 17v6" stroke={`url(#${g})`} strokeWidth="2.3" strokeLinecap="round" />
    </svg>
  );
}
