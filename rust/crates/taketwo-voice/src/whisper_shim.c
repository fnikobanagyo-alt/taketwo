// Flat C API over whisper.cpp for taketwo-voice (step S13). Rust never mirrors whisper_full_params or
// whisper_context_params: every field is set here, against the submodule's own whisper.h.
#include "whisper.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct tt_whisper {
    struct whisper_context * ctx;
    int windows[1024]; // segment count when each window's encoder began (window boundaries)
    int n_windows;
    const volatile bool * abort_flag; // a Rust AtomicBool (same layout as bool); volatile, as MSVC C has no __atomic
} tt_whisper;

static void tt_quiet(enum ggml_log_level level, const char * text, void * user_data) {
    (void) level; (void) text; (void) user_data;
}

// heads: 0 = every head of the top 2 decoder layers (mlx-audio's default), 1 = the large-v3-turbo preset.
tt_whisper * tt_whisper_open(const char * path, int use_gpu, int heads, int quiet) {
    if (quiet) {
        whisper_log_set(tt_quiet, NULL);
    }
    struct whisper_context_params cp = whisper_context_default_params();
    cp.use_gpu = use_gpu != 0;
    cp.flash_attn = false; // DTW needs the softmaxed cross-attention, which flash attention never forms
    cp.gpu_device = 0;
    cp.dtw_token_timestamps = true;
    cp.dtw_aheads_preset = heads == 1 ? WHISPER_AHEADS_LARGE_V3_TURBO : WHISPER_AHEADS_N_TOP_MOST;
    cp.dtw_n_top = 2;
    cp.dtw_mem_size = (size_t) 512 * 1024 * 1024; // heads x tokens x frames f32 and its copies; overflow aborts
    struct whisper_context * ctx = whisper_init_from_file_with_params(path, cp);
    if (!ctx) {
        return NULL;
    }
    tt_whisper * w = calloc(1, sizeof(tt_whisper));
    w->ctx = ctx;
    return w;
}

void tt_whisper_close(tt_whisper * w) {
    if (w) {
        whisper_free(w->ctx);
        free(w);
    }
}

static bool tt_abort(void * user_data) {
    const tt_whisper * w = user_data;
    return w->abort_flag && *w->abort_flag;
}

static bool tt_encoder_begin(struct whisper_context * ctx, struct whisper_state * state, void * user_data) {
    tt_whisper * w = user_data;
    (void) ctx;
    if (w->n_windows < (int) (sizeof(w->windows) / sizeof(w->windows[0]))) {
        w->windows[w->n_windows++] = whisper_full_n_segments_from_state(state);
    }
    return !tt_abort(w);
}

// Mirrors mlx-audio's generate() as ml.transcribe calls it; rust/PROGRESS.md (S13) lists each option.
int tt_whisper_run(tt_whisper * w, const float * pcm, int n_samples, const char * language,
                   const volatile bool * abort_flag, int n_threads) {
    struct whisper_full_params p = whisper_full_default_params(WHISPER_SAMPLING_GREEDY);
    p.n_threads = n_threads;
    p.n_max_text_ctx = 16384;
    p.offset_ms = 0;
    p.duration_ms = 0;
    p.translate = false;
    p.no_context = true; // condition_on_previous_text=False
    p.no_timestamps = false;
    p.single_segment = false;
    p.print_special = false;
    p.print_progress = false;
    p.print_realtime = false;
    p.print_timestamps = false;
    p.token_timestamps = false; // the old heuristic timestamps; DTW is on in the context instead
    p.thold_pt = 0.01f;
    p.thold_ptsum = 0.01f;
    p.max_len = 0;
    p.split_on_word = false;
    p.max_tokens = 0;
    p.debug_mode = false;
    p.audio_ctx = 0;
    p.tdrz_enable = false;
    p.suppress_regex = NULL;
    p.initial_prompt = NULL;
    p.carry_initial_prompt = false;
    p.prompt_tokens = NULL;
    p.prompt_n_tokens = 0;
    p.language = language ? language : "auto";
    p.detect_language = false; // true would stop after detection
    p.suppress_blank = true;
    p.suppress_nst = true; // suppress_tokens="-1": non-speech tokens
    p.temperature = 0.0f;
    p.max_initial_ts = 1.0f;
    p.length_penalty = -1.0f; // None
    p.temperature_inc = 0.2f; // ladder 0.0, 0.2, ..., 1.0
    p.entropy_thold = 2.4f; // stands in for compression_ratio_threshold=2.4 (a different measure)
    p.logprob_thold = -1.0f;
    p.no_speech_thold = 0.6f;
    p.greedy.best_of = 1; // best_of=None: one sample per temperature
    p.beam_search.beam_size = -1;
    p.beam_search.patience = -1.0f;
    p.new_segment_callback = NULL;
    p.progress_callback = NULL;
    p.encoder_begin_callback = tt_encoder_begin;
    p.encoder_begin_callback_user_data = w;
    p.abort_callback = tt_abort;
    p.abort_callback_user_data = w;
    p.logits_filter_callback = NULL;
    p.grammar_rules = NULL;
    p.n_grammar_rules = 0;
    p.vad = false;
    p.vad_model_path = NULL;

    w->n_windows = 0;
    w->abort_flag = abort_flag;
    int rc = whisper_full(w->ctx, p, pcm, n_samples);
    w->abort_flag = NULL;
    return rc;
}

int tt_whisper_n_windows(const tt_whisper * w) { return w->n_windows; }
int tt_whisper_window_first_segment(const tt_whisper * w, int i) { return w->windows[i]; }
int tt_whisper_n_segments(const tt_whisper * w) { return whisper_full_n_segments(w->ctx); }
int64_t tt_whisper_segment_t0(const tt_whisper * w, int s) { return whisper_full_get_segment_t0(w->ctx, s); }
int64_t tt_whisper_segment_t1(const tt_whisper * w, int s) { return whisper_full_get_segment_t1(w->ctx, s); }
float tt_whisper_segment_no_speech(const tt_whisper * w, int s) { return whisper_full_get_segment_no_speech_prob(w->ctx, s); }
int tt_whisper_n_tokens(const tt_whisper * w, int s) { return whisper_full_n_tokens(w->ctx, s); }
int tt_whisper_token_id(const tt_whisper * w, int s, int t) { return whisper_full_get_token_id(w->ctx, s, t); }
float tt_whisper_token_p(const tt_whisper * w, int s, int t) { return whisper_full_get_token_data(w->ctx, s, t).p; }
float tt_whisper_token_plog(const tt_whisper * w, int s, int t) { return whisper_full_get_token_data(w->ctx, s, t).plog; }
int64_t tt_whisper_token_dtw_t0(const tt_whisper * w, int s, int t) { return whisper_full_get_token_dtw_t0(w->ctx, s, t); }
int64_t tt_whisper_token_dtw_t1(const tt_whisper * w, int s, int t) { return whisper_full_get_token_dtw_t1(w->ctx, s, t); }
const char * tt_whisper_token_text(const tt_whisper * w, int id) { return whisper_token_to_str(w->ctx, id); }
int tt_whisper_token_eot(const tt_whisper * w) { return whisper_token_eot(w->ctx); }
const char * tt_whisper_language(const tt_whisper * w) { return whisper_lang_str(whisper_full_lang_id(w->ctx)); }
