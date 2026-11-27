// The CUDA build on Windows (MSVC) delay-loads the NVIDIA driver DLL, so the program starts, and answers
// `--version`, on a PC without an NVIDIA driver; nvcuda.dll is loaded when the GPU is first used (Franz,
// lead decision 11). Read from the build's environment, not `cfg!`, which describes the build script's host.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let cuda = std::env::var_os("CARGO_FEATURE_CUDA").is_some();
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if cuda && msvc {
        // a dependency's build script cannot pass linker arguments to this exe, so they are here
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:nvcuda.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
