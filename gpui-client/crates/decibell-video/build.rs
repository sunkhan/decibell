//! Linux: compiles the C shim against the FFmpeg prefix's headers (the prefix ffmpeg-sys-next
//! links, found through the same pkg-config path). The shim holds what touches FFmpeg's Vulkan
//! structs (`AVVulkanDeviceContext`, `AVVkFrame`), so their layout comes from the headers of the
//! FFmpeg that is linked and not from hand-written Rust. Other platforms compile no C.
//! Also sets `cfg(vk)`.

fn main() {
    // `cfg(vk)`: the Vulkan backend (Linux, unless the `no-gpu` feature asks for the neutral half
    // alone).
    println!("cargo::rustc-check-cfg=cfg(vk)");
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    let vk = linux && std::env::var_os("CARGO_FEATURE_NO_GPU").is_none();
    if vk {
        println!("cargo::rustc-cfg=vk");
    }
    #[cfg(target_os = "linux")]
    {
        println!("cargo::rerun-if-changed=src/linux/shim.c");
        if !vk {
            return;
        }
        let avutil = pkg_config::Config::new()
            .statik(true)
            .cargo_metadata(false)
            .probe("libavutil")
            .expect("libavutil.pc (run gpui-client/tools/build-ffmpeg.sh)");
        let mut build = cc::Build::new();
        build.file("src/linux/shim.c").opt_level(2).warnings(false);
        for path in &avutil.include_paths {
            build.include(path);
        }
        build.compile("decibell_video_shim");
    }
}
