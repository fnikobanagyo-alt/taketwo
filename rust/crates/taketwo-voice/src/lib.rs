//! Models behind traits. Filled in from step S02 on (see rust/PLAN.md).
//! S02: only a link check that touches Candle (qwen_tts), whisper.cpp and ONNX Runtime.

pub mod encoder;
pub mod qwen;
pub mod whisper;

use std::ffi::{CStr, c_char};

unsafe extern "C" {
    fn whisper_version() -> *const c_char;
    fn whisper_print_system_info() -> *const c_char;
}

fn c_str(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// One line per library, each from a real call into it, so the linker cannot drop any of them.
pub fn link_report() -> String {
    use candle_core::{DType, Device};
    let candle = match qwen_tts::nn::mt_rng::MtBoxMullerRng::new(0).randn(&[2], &Device::Cpu, DType::F32) {
        Ok(t) => format!("ok, cpu tensor {:?}", t.dims()),
        Err(e) => format!("error: {e}"),
    };
    let (wv, wsys) = unsafe { (c_str(whisper_version()), c_str(whisper_print_system_info())) };
    format!(
        "candle (qwen_tts): {candle}; cuda available: {}; metal available: {}\n\
         whisper.cpp {wv} ({}): {}\n\
         onnx runtime: {}",
        candle_core::utils::cuda_is_available(),
        candle_core::utils::metal_is_available(),
        env!("TAKETWO_WHISPER_COMMIT"),
        wsys.trim(),
        ort::info().trim(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn link_report_names_all_three_libraries() {
        let r = super::link_report();
        assert!(r.contains("candle (qwen_tts): ok"), "{r}");
        assert!(r.contains("whisper.cpp 1."), "{r}");
        assert!(r.contains("ORT Build Info"), "{r}");
    }
}
