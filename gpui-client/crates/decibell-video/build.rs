//! Linux: compiles the C shim against the FFmpeg prefix's headers (the prefix ffmpeg-sys-next
//! links, found through the same pkg-config path). The shim holds what touches FFmpeg's Vulkan,
//! CUDA and DRM structs (`AVVulkanDeviceContext`, `AVVkFrame`, `AVCUDADeviceContext`,
//! `AVDRMFrameDescriptor`) and CUDA's own (through nv-codec-headers' run-time loader, which the
//! prefix installs), so their layout comes from the headers of the FFmpeg that is linked and
//! not from hand-written Rust. Other platforms compile no C.
//! Also sets `cfg(vk)`, `cfg(d3d)` and `cfg(gpu)`.

fn main() {
    // `cfg(vk)`: the Vulkan backend (Linux, unless the `no-gpu` feature asks for the neutral half
    // alone).
    // `cfg(d3d)`: the Direct3D 11 backend (Windows, likewise). `cfg(gpu)`: either.
    println!("cargo::rustc-check-cfg=cfg(vk)");
    println!("cargo::rustc-check-cfg=cfg(d3d)");
    println!("cargo::rustc-check-cfg=cfg(gpu)");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let no_gpu = std::env::var_os("CARGO_FEATURE_NO_GPU").is_some();
    let linux = target_os == "linux";
    let vk = linux && !no_gpu;
    if vk {
        println!("cargo::rustc-cfg=vk");
    }
    if target_os == "windows" && !no_gpu {
        println!("cargo::rustc-cfg=d3d");
    }
    if (linux || target_os == "windows") && !no_gpu {
        println!("cargo::rustc-cfg=gpu");
    }
    #[cfg(target_os = "linux")]
    {
        for file in ["shim.c", "cuda.c", "drm.c"] {
            println!("cargo::rerun-if-changed=src/linux/{file}");
        }
        if !vk {
            return;
        }
        let avutil = pkg_config::Config::new()
            .statik(true)
            .cargo_metadata(false)
            .probe("libavutil")
            .expect("libavutil.pc (run gpui-client/tools/build-ffmpeg.sh)");
        let mut build = cc::Build::new();
        build
            .file("src/linux/shim.c")
            .file("src/linux/cuda.c")
            .file("src/linux/drm.c")
            .opt_level(2)
            .warnings(false);
        for path in &avutil.include_paths {
            build.include(path);
        }
        build.compile("decibell_video_shim");
    }
}
