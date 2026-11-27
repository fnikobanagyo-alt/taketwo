// Builds whisper.cpp (git submodule rust/third_party/whisper.cpp, pinned at the PR #4170 merge
// commit) as static libraries and links them. CUDA comes with the `cuda` feature.
use std::path::{Path, PathBuf};

fn main() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/whisper.cpp");
    if !src.join("CMakeLists.txt").exists() {
        panic!("whisper.cpp source missing at {}: run `git submodule update --init`", src.display());
    }
    for p in ["include", "src", "ggml/include", "ggml/src", "CMakeLists.txt"] {
        println!("cargo:rerun-if-changed={}", src.join(p).display());
    }
    println!("cargo:rerun-if-env-changed=CUDA_COMPUTE_CAP");
    let commit = std::process::Command::new("git")
        .args(["-C", src.to_str().unwrap(), "rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=TAKETWO_WHISPER_COMMIT={commit}");

    let cuda = std::env::var_os("CARGO_FEATURE_CUDA").is_some();
    let mut cfg = cmake::Config::new(&src);
    // Release always: a Debug build on MSVC uses the /MDd runtime, which Rust does not link.
    cfg.profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("WHISPER_BUILD_EXAMPLES", "OFF")
        .define("WHISPER_BUILD_TESTS", "OFF")
        .define("WHISPER_BUILD_SERVER", "OFF")
        .define("GGML_NATIVE", "OFF") // the exe must run on other CPUs than the build machine's
        .define("GGML_OPENMP", "OFF")
        .define("GGML_METAL", "OFF")
        .define("GGML_BLAS", "OFF")
        .define("GGML_CUDA", if cuda { "ON" } else { "OFF" });
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Static CRT (/MT): esaxx-rs (via tokenizers) and candle's moe kernels are built /MT, and MSVC
        // refuses to mix runtimes. CMP0091 NEW makes CMake honour the runtime for C, C++ and CUDA.
        cfg.static_crt(true)
            .define("CMAKE_POLICY_DEFAULT_CMP0091", "NEW")
            .define("CMAKE_MSVC_RUNTIME_LIBRARY", "MultiThreaded");
    }
    if cuda {
        let cap = std::env::var("CUDA_COMPUTE_CAP").unwrap_or_else(|_| "86".into());
        cfg.define("CMAKE_CUDA_ARCHITECTURES", cap);
    }
    let out = cfg.build();

    for dir in ["lib", "lib64"] {
        println!("cargo:rustc-link-search=native={}", out.join(dir).display());
    }
    let mut libs = vec!["whisper", "ggml", "ggml-cpu"];
    if cuda {
        libs.push("ggml-cuda");
    }
    libs.push("ggml-base");
    for lib in libs {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target == "macos" {
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-lib=framework=Accelerate"); // ggml-cpu uses vDSP on Apple
    } else if target == "linux" {
        println!("cargo:rustc-link-lib=stdc++");
    }
    if cuda {
        if let Some(root) = std::env::var_os("CUDA_PATH") {
            let root = Path::new(&root);
            for dir in ["lib/x64", "lib64", "lib"] {
                println!("cargo:rustc-link-search=native={}", root.join(dir).display());
            }
        }
        for lib in ["cudart", "cublas", "cublasLt", "cuda"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
