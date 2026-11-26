export type Param = { key: string; label: string; default: number; min: number; max: number; step: number; help: string };
export type Engine = {
  id: string; name: string; repo: string; family: string; description: string; license: string; size_gb: number;
  languages: string[]; needs_ref_text: boolean; recommended: boolean; params: Param[]; custom: boolean;
  downloaded: boolean; loaded: boolean; download?: { status: string; error?: string } | null; kind: "clone" | "design";
};
export type Variant = { index: number; file: string; start: number; end: number; duration: number; score: number; text: string; expressiveness?: number; delivery?: "expressive" | "balanced" | "calm"; pitch_range?: number };
export type VoiceSettings = { stability: number; similarity: number; style: number; mic_match: boolean; studio_finish: boolean };
export const DEFAULT_VS: VoiceSettings = { stability: 0.5, similarity: 0.75, style: 0, mic_match: true, studio_finish: true };
export type Job = { id: string; status: string; progress: number; message: string; error?: string | null; cancel?: boolean };
export type Profile = {
  id: string; name: string; language: string; status: "queued" | "analyzing" | "ready" | "error"; error?: string;
  original_file: string; original_duration?: number; truncated?: boolean; duration?: number; snr_db?: number;
  denoised?: boolean; self_similarity?: number; usable_speech_seconds?: number; speech_rate_wps?: number;
  variants?: Variant[]; active_variant?: number; ref_text_override?: Record<string, string>; tips?: string[];
  transcript?: string; default_engine?: string; created: number; job?: Job | null; description?: string; designed?: boolean; studio?: boolean; room_echo_db?: number; room_echo_after_db?: number | null;
  pipeline_version?: number;
  refresh_report?: { updated: number[]; kept: number[]; trimmed?: number[] };
  preview?: { file: string; engine: string; variant: number; created: number; duration: number } | null;
};
export type Attempt = { seed: number; temperature?: number; liveliness?: number; ref?: number; wer: number; sim: number; max_gap: number; dur: number; issues: string[]; score: number; secs: number; heard: string };
export type Chunk = {
  i: number; text: string; pause_after: string; status: "pending" | "running" | "ok" | "flagged";
  file?: string; audio_revision?: number; attempts?: Attempt[]; wer?: number; sim?: number; dur?: number; issues?: string[]; final_issues?: string[]; heard?: string; regen_count?: number;
};
export type Generation = {
  id: string; profile_id: string; profile_name: string; engine: string; language: string; text: string; text_preview?: string;
  status: string; progress: number; message: string; eta?: number; chunks: Chunk[]; duration?: number; file?: string;
  flagged?: number[]; retries?: number; render_seconds?: number; created: number; updated: number; job?: Job | null;
  title?: string; favorite?: boolean; speed?: number; voice_settings?: VoiceSettings; mic_eq?: number[] | null; final_scan?: { checked: number; attempted?: number[]; fixed: number[]; remaining?: number[] }; timeline?: { i: number; start: number; end: number }[];
  audio_rev?: number;
  queue?: { ahead: number; waiting_for: string[] };
  duplicate_notice?: { count: number; last: number } | null;
  /** Once done: "clean", or "needs_listen" when any part is doubtful. "done" alone never means every part passed. */
  outcome?: "clean" | "needs_listen" | null;
  review?: { part: number; text: string; issues: string[]; reasons: string[] }[];
  review_count?: number;
};
export type QueueItem = {
  id: string; kind: string; label: string; status: "running" | "queued"; progress: number; message: string;
  gid?: string; title?: string; done?: number; total?: number;
};
export type Status = {
  loaded_engine: string | null; running: QueueItem | null; queued: number; queue: QueueItem[]; max_script_words: number;
  duplicates?: { gid: string; title: string; count: number; last: number }[];
};
export type Transcript = {
  id: string; name: string; status: string; created: number; error?: string; text?: string; language?: string; duration?: number;
  words?: number; captions?: { start: number; end: number; text: string }[]; job?: Job | null;
};
export type Design = {
  id: string; description: string; text: string; language: string; count: number; status: string; error?: string;
  previews: { index: number; file: string; duration: number }[]; job?: Job | null;
};
export type UpdateState = {
  current: string; available: boolean; dismissed: boolean; installed: string | null;
  check: { checked_at: number; latest: string | null; notes: string; url: string; error: string | null } | null;
  job: { id: string; status: "queued" | "running" | "done" | "error" | "cancelled"; progress: number; message: string; error?: string | null } | null;
};
export type Settings = {
  default_engine: string; max_chunk_chars: number; max_attempts: number; compare_takes: number; max_wer: number; min_speaker_similarity: number;
  max_internal_silence: number; sentence_pause_ms: number; paragraph_pause_ms: number; target_lufs: number; seed: number;
  pronunciations: { from: string; to: string }[]; strip_citations: boolean; check_updates: boolean; update_dismissed: string;
};
export type Discovered = { repo: string; downloads: number; likes: number; updated: string; architecture: string | null; supported: boolean; added: boolean };

/** An API error that keeps the server's structured detail (for example a duplicate narration's id). */
export class ApiError extends Error {
  status: number;
  detail: { message?: string; duplicate_of?: string } | string | null;
  constructor(message: string, status: number, detail: ApiError["detail"]) {
    super(message);
    this.status = status;
    this.detail = detail;
  }
}

async function req<T>(method: string, url: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method };
  if (body instanceof FormData) init.body = body;
  else if (body !== undefined) {
    init.body = JSON.stringify(body);
    init.headers = { "Content-Type": "application/json" };
  }
  const r = await fetch(url, init);
  if (!r.ok) {
    let detail: ApiError["detail"] = null;
    try { detail = (await r.json()).detail ?? null; } catch { /* ignore */ }
    const msg = typeof detail === "string" ? detail : detail?.message ?? r.statusText;
    throw new ApiError(msg, r.status, detail);
  }
  return r.json();
}

export const api = {
  engines: () => req<Engine[]>("GET", "/api/engines"),
  downloadEngine: (id: string) => req("POST", `/api/engines/${id}/download`),
  downloadProgress: (id: string) => req<{ status: string; progress: number; bytes: number; error?: string }>("GET", `/api/engines/${id}/download`),
  addCustom: (repo: string) => req<{ id: string }>("POST", "/api/engines/custom", { repo }),
  removeCustom: (id: string) => req("DELETE", `/api/engines/custom/${id}`),
  library: () => req<{ installed: string; latest: string | null; update_available: boolean }>("GET", "/api/catalog/library"),
  upgrade: () => req<{ status: string; message: string }>("POST", "/api/catalog/upgrade"),
  upgradeStatus: () => req<{ status: string; message: string }>("GET", "/api/catalog/upgrade"),
  discover: (sort: string) => req<Discovered[]>("GET", `/api/catalog/discover?sort=${sort}`),

  profiles: () => req<Profile[]>("GET", "/api/profiles"),
  profile: (id: string) => req<Profile>("GET", `/api/profiles/${id}`),
  createProfile: (fd: FormData) => req<Profile>("POST", "/api/profiles", fd),
  patchProfile: (id: string, patch: Partial<{ name: string; description: string; active_variant: number; ref_text: string; default_engine: string }>) =>
    req<Profile>("PATCH", `/api/profiles/${id}`, patch),
  reanalyze: (id: string, denoise: boolean, target_window: number, studio?: boolean) => req("POST", `/api/profiles/${id}/reanalyze`, { denoise, target_window, studio }),
  deleteProfile: (id: string) => req("DELETE", `/api/profiles/${id}`),
  upgradeAll: () => req<{ queued: string[] }>("POST", "/api/profiles/upgrade-all", { mode: "refresh", only_outdated: true }),
  refreshProfile: (id: string) => req<Job>("POST", `/api/profiles/${id}/refresh`),
  pipelineVersion: () => req<{ version: number }>("GET", "/api/pipeline-version"),
  integrations: () => req<{ script_found: boolean; apps: { id: string; name: string; connected: boolean; how: string; setup: string }[] }>("GET", "/api/integrations"),
  profileAudio: (id: string, name: string) => `/api/profiles/${id}/audio/${name}`,

  generate: (body: { profile_id: string; text: string; engine?: string; language?: string; params?: Record<string, number>; speed?: number; voice_settings?: VoiceSettings }) =>
    req<Generation>("POST", "/api/generations", body),
  generations: () => req<Generation[]>("GET", "/api/generations"),
  generation: (id: string) => req<Generation>("GET", `/api/generations/${id}`),
  cancel: (id: string) => req("POST", `/api/generations/${id}/cancel`),
  resume: (id: string) => req("POST", `/api/generations/${id}/resume`),
  fixRemaining: (id: string) => req("POST", `/api/generations/${id}/fix-remaining`),
  regenerate: (id: string, i: number, text?: string) => req("POST", `/api/generations/${id}/chunks/${i}/regenerate`, { text }),
  deleteGeneration: (id: string) => req("DELETE", `/api/generations/${id}`),
  audioUrl: (id: string, fmt = "wav", v?: number) => `/api/generations/${id}/audio?fmt=${fmt}${v ? `&v=${v}` : ""}`,
  chunkUrl: (id: string, i: number, v?: number | string) => `/api/generations/${id}/chunks/${i}/audio?v=${encodeURIComponent(v ?? 0)}`,

  patchGeneration: (id: string, patch: Partial<{ title: string; favorite: boolean; speed: number }>) => req<Generation>("PATCH", `/api/generations/${id}`, patch),
  subtitlesUrl: (id: string, fmt = "srt") => `/api/generations/${id}/subtitles?fmt=${fmt}`,
  status: () => req<Status>("GET", "/api/status"),

  transcripts: () => req<Transcript[]>("GET", "/api/transcripts"),
  transcript: (id: string) => req<Transcript>("GET", `/api/transcripts/${id}`),
  createTranscript: (fd: FormData) => req<Transcript>("POST", "/api/transcripts", fd),
  deleteTranscript: (id: string) => req("DELETE", `/api/transcripts/${id}`),
  transcriptExport: (id: string, fmt: string) => `/api/transcripts/${id}/export?fmt=${fmt}`,
  transcriptAudio: (id: string) => `/api/transcripts/${id}/audio`,
  retryTranscript: (id: string) => req<Transcript>("POST", `/api/transcripts/${id}/retry`),

  createDesign: (body: { description: string; text?: string; language?: string; count?: number }) => req<Design>("POST", "/api/designs", body),
  design: (id: string) => req<Design>("GET", `/api/designs/${id}`),
  designAudio: (id: string, file: string) => `/api/designs/${id}/audio/${file}`,
  saveDesign: (id: string, index: number, name: string) => req<Profile>("POST", `/api/designs/${id}/save`, { index, name }),

  settings: () => req<Settings>("GET", "/api/settings"),
  update: () => req<UpdateState>("GET", "/api/update"),
  updateCheck: () => req<UpdateState>("POST", "/api/update/check"),
  updateStart: () => req<UpdateState>("POST", "/api/update/start"),
  updateCancel: () => req<UpdateState>("POST", "/api/update/cancel"),
  updateRestart: () => req<{ restarting: boolean }>("POST", "/api/update/restart"),
  health: () => req<{ ok: boolean; version: string }>("GET", "/api/health"),
  saveSettings: (patch: Partial<Settings>) => req<Settings>("PUT", "/api/settings", patch),
};

export const LANGS: Record<string, string> = {
  en: "English", es: "Spanish", fr: "French", de: "German", it: "Italian", pt: "Portuguese",
  ru: "Russian", zh: "Chinese", ja: "Japanese", ko: "Korean",
};

export function wordCount(t: string) {
  return (t.match(/[\p{L}\p{N}']+/gu) ?? []).length;
}

export function fmtDuration(s?: number) {
  if (s == null || !isFinite(s)) return "–";
  if (s < 60) return `${Math.round(s)}s`;
  const h = Math.floor(s / 3600);
  const m = Math.round((s % 3600) / 60);
  return h ? `${h}h ${m}m` : `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`;
}

export function fmtTime(s?: number) {
  if (s == null || !isFinite(s)) return "–";
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}

export function timeAgo(ts: number) {
  const d = Date.now() / 1000 - ts;
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  return new Date(ts * 1000).toLocaleDateString();
}

export const ISSUE_LABEL: Record<string, string> = {
  cut_off: "Cut-off ending",
  lead_burst: "Stray sound at the start",
  end_word_missing: "Last word missing",
  start_word_missing: "First word missing",
  processed_audio_missing: "Audio missing from final mix",
  processed_audio_cut_off: "Speech lost in final mix",
  ending_unverified: "Listen to sentence ending",
  odd_pause: "Odd pause",
  leak: "Stray next word",
  junk_tail: "Stray sound at the end",
  flat: "Too monotone",
  off_pitch: "Pitch off from the voice",
  long_silence: "Silence gap",
  too_long: "Too long / looping",
  too_short: "Cut short",
  voice_drift: "Voice drift",
  noise: "Noise burst",
  wrong_words: "Wrong words",
  mispronounced: "Mispronounced",
  clipping: "Clipping",
  empty: "No audio",
  trimmed_tail: "Tail trimmed",
  trimmed_head: "Head trimmed",
  final_check: "Failed the final check",
  web_artifact: "Web code in the script (such as &#39;)",
};
