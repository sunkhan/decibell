# DMA-BUF Zero-Copy Screen Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all CPU frame copies in the Linux PipeWire screen capture pipeline by keeping frame data on the GPU from capture through H.264 encoding. Support NVIDIA (CUDA+NVENC), AMD, and Intel (VA-API) GPUs. Zero changes to the Windows streaming pipeline.

**Architecture:** PipeWire negotiates DMA-BUF buffers instead of SHM. On NVIDIA, the DMA-BUF fd is imported via EGL → GL texture → CUDA registration → CUdeviceptr, then NVENC encodes from GPU memory. On AMD/Intel, the DMA-BUF fd is wrapped in an AVDRMFrameDescriptor, mapped to a VA-API surface via `av_hwframe_map`, then h264_vaapi encodes directly. A separate `gpu_receiver` channel carries DMA-BUF frames on Linux only — the existing `RawFrame` channel and all Windows code remain completely untouched. Falls back to existing SHM+CPU path when DMA-BUF or GPU APIs are unavailable.

**Tech Stack:** `pipewire-rs` (DMA-BUF negotiation), `khronos-egl` (EGL bindings, NVIDIA only), `gl` (OpenGL bindings, NVIDIA only), `libloading` (CUDA driver API dlopen, NVIDIA only), `ffmpeg-sys-next` (raw FFmpeg C API for hw_device_ctx/hw_frames_ctx/DRM frame descriptors)

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src-tauri/Cargo.toml` | Modify | Add `khronos-egl`, `gl`, `libloading` as Linux-only deps |
| `src-tauri/src/media/gpu_interop.rs` | Create | DMA-BUF import: NVIDIA (EGL→GL→CUDA) and AMD/Intel (DRM→VAAPI) backends |
| `src-tauri/src/media/capture.rs` | Modify | Add `DmaBufFrame` type, add `gpu_receiver` field to `CaptureOutput` (Linux-only) |
| `src-tauri/src/media/capture_pipewire.rs` | Modify | Remove `MAP_BUFFERS`, detect DMA-BUF vs SHM, send `DmaBufFrame` via gpu channel |
| `src-tauri/src/media/encoder.rs` | Modify | Add CUDA and VAAPI hw encoding paths, `encode_gpu_frame()` and `encode_vaapi_drm_frame()` |
| `src-tauri/src/media/video_pipeline.rs` | Modify | Accept optional gpu_receiver, init GPU context, route GPU vs CPU frames |
| `src-tauri/src/media/mod.rs` | Modify | Add `gpu_interop` module, update `VideoEngine::start` signature (Linux-only fields) |
| `src-tauri/src/commands/streaming.rs` | Modify | Pass `gpu_receiver` from `CaptureOutput` to `VideoEngine::start` |

**NOT modified:** `capture_wgc.rs`, `capture_dxgi.rs`, `capture_audio_wasapi.rs`, or any other Windows-specific file.

---

### Task 1: Add GPU Interop Dependencies

**Files:**
- Modify: `src-tauri/Cargo.toml:37-39`

- [ ] **Step 1: Add Linux-only dependencies**

Add `khronos-egl`, `gl`, and `libloading` to the Linux dependencies section:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = "0.9"
zbus = { version = "5", default-features = false, features = ["blocking"] }
libc = "0.2"
khronos-egl = { version = "6", features = ["dynamic"] }
gl = "0.14"
libloading = "0.8"
```

- [ ] **Step 2: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -5`
Expected: Compilation succeeds (new deps downloaded, no errors)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(deps): add EGL/GL/CUDA interop crates for DMA-BUF zero-copy"
```

---

### Task 2: Add DmaBufFrame Type and gpu_receiver to CaptureOutput

**Files:**
- Modify: `src-tauri/src/media/capture.rs`

This adds the GPU frame type and a Linux-only optional receiver field to `CaptureOutput`. The existing `RawFrame` and `Receiver<RawFrame>` are completely unchanged — Windows code sees no difference.

- [ ] **Step 1: Add DmaBufFrame struct**

Add after the `RawFrame` struct (after line 48), before the `AudioFrame` struct:

```rust
/// GPU-resident frame from PipeWire DMA-BUF capture (Linux only).
/// The fd is a dup'd DMA-BUF file descriptor — the kernel keeps the
/// underlying buffer alive via refcount even after PipeWire reclaims it.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBufFrame {
    /// DMA-BUF file descriptor (dup'd from PipeWire, closed on drop)
    pub fd: std::os::fd::OwnedFd,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes
    pub stride: u32,
    /// DRM fourcc format code (e.g. DRM_FORMAT_ARGB8888 for BGRA)
    pub drm_format: u32,
    /// DRM format modifier (DRM_FORMAT_MOD_INVALID if unknown)
    pub modifier: u64,
    pub timestamp_us: u64,
}
```

- [ ] **Step 2: Add gpu_receiver to CaptureOutput**

Add a `#[cfg]`-gated field to the existing `CaptureOutput` struct:

```rust
/// Result of starting a capture — the frame receiver plus the actual output dimensions.
pub struct CaptureOutput {
    pub receiver: std::sync::mpsc::Receiver<RawFrame>,
    pub width: u32,
    pub height: u32,
    /// Linux-only: optional DMA-BUF frame receiver for zero-copy GPU encoding.
    /// When PipeWire provides DMA-BUF buffers, frames arrive here instead of
    /// `receiver`. The video pipeline checks this first, falls back to `receiver`.
    #[cfg(target_os = "linux")]
    pub gpu_receiver: Option<std::sync::mpsc::Receiver<DmaBufFrame>>,
}
```

- [ ] **Step 3: Fix CaptureOutput construction sites on Linux**

In `capture_pipewire.rs`, line 66:
```rust
        Ok(CaptureOutput { receiver: rx, width, height, gpu_receiver: None })
```

This is a temporary change — Task 4 will set it to `Some(gpu_rx)` when DMA-BUF is available.

- [ ] **Step 4: Verify Windows CaptureOutput still compiles**

The Windows capture modules (`capture_wgc.rs:98`, `capture_dxgi.rs:485`) construct `CaptureOutput` without `gpu_receiver` — this is correct because the field doesn't exist on Windows due to `#[cfg(target_os = "linux")]`.

Run: `cd src-tauri && cargo check 2>&1 | tail -5`
Expected: Compiles on Linux. Windows cross-check: the `#[cfg]` gate means the field is absent from the Windows struct, so existing `CaptureOutput { receiver, width, height }` constructors in `capture_wgc.rs` and `capture_dxgi.rs` compile unchanged.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media/capture.rs src-tauri/src/media/capture_pipewire.rs
git commit -m "feat(capture): add DmaBufFrame type and gpu_receiver to CaptureOutput"
```

---

### Task 3: Create GPU Interop Module

**Files:**
- Create: `src-tauri/src/media/gpu_interop.rs`
- Modify: `src-tauri/src/media/mod.rs` (add module declaration)

All unsafe GPU code is isolated in this single file. Two backends:
- **NVIDIA (CudaBackend):** DMA-BUF fd → EGLImage → GL texture → CUDA register → CUarray → cuMemcpy2D → CUdeviceptr
- **VAAPI (VaapiBackend):** DMA-BUF fd → AVDRMFrameDescriptor → av_hwframe_map → VASurface

- [ ] **Step 1: Add module declaration in mod.rs**

Add after line 6 (`pub mod capture_pipewire;`):

```rust
#[cfg(target_os = "linux")]
pub mod gpu_interop;
```

- [ ] **Step 2: Create gpu_interop.rs**

```rust
//! EGL/GL/CUDA and VA-API interop for DMA-BUF zero-copy capture.
//!
//! All unsafe GPU code is isolated in this module. Two backends:
//!
//! - **NVIDIA (Cuda):** DMA-BUF → EGLImage → GL texture → CUDA → CUdeviceptr → NVENC
//! - **AMD/Intel (Vaapi):** DMA-BUF → AVDRMFrameDescriptor → av_hwframe_map → h264_vaapi
//!
//! Public API:
//! - `GpuContext::new()` → detects GPU, returns appropriate backend
//! - `GpuContext::import_dmabuf_cuda()` → NVIDIA: DMA-BUF → CUdeviceptr
//! - `GpuContext::fill_drm_frame()` → AMD/Intel: fills AVDRMFrameDescriptor for VA-API
//! - `GpuContext::backend()` → which GPU backend is active

use std::os::fd::AsRawFd;
use libloading::Library;

// ────────────────────────────────────────────────────────────────────────────
// CUDA Driver API types (subset needed for GL interop)
// ────────────────────────────────────────────────────────────────────────────

type CUresult = i32;
type CUdevice = i32;
type CUcontext = *mut std::ffi::c_void;
type CUdeviceptr = u64;
type CUarray = *mut std::ffi::c_void;
type CUgraphicsResource = *mut std::ffi::c_void;

const CUDA_SUCCESS: CUresult = 0;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: u32 = 1;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const CU_MEMORYTYPE_DEVICE: u32 = 2;
const CU_MEMORYTYPE_ARRAY: u32 = 3;

/// CUDA_MEMCPY2D — layout must match CUDA driver API struct exactly.
#[repr(C)]
struct CudaMemcpy2D {
    src_x_in_bytes: usize,
    src_y: usize,
    src_memory_type: u32,
    src_host: *const std::ffi::c_void,
    src_device: CUdeviceptr,
    src_array: CUarray,
    _src_reserved: usize, // reserved field in CUDA struct
    src_pitch: usize,
    dst_x_in_bytes: usize,
    dst_y: usize,
    dst_memory_type: u32,
    dst_host: *mut std::ffi::c_void,
    dst_device: CUdeviceptr,
    dst_array: CUarray,
    _dst_reserved: usize,
    dst_pitch: usize,
    width_in_bytes: usize,
    height: usize,
}

impl CudaMemcpy2D {
    fn array_to_device(
        src_array: CUarray,
        dst_device: CUdeviceptr,
        dst_pitch: usize,
        width_bytes: usize,
        height: usize,
    ) -> Self {
        CudaMemcpy2D {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CU_MEMORYTYPE_ARRAY,
            src_host: std::ptr::null(),
            src_device: 0,
            src_array,
            _src_reserved: 0,
            src_pitch: 0,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: CU_MEMORYTYPE_DEVICE,
            dst_host: std::ptr::null_mut(),
            dst_device,
            dst_array: std::ptr::null_mut(),
            _dst_reserved: 0,
            dst_pitch,
            width_in_bytes: width_bytes,
            height,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// CUDA function pointer table — loaded from libcuda.so.1 at runtime
// ────────────────────────────────────────────────────────────────────────────

struct CudaApi {
    _lib: Library,
    cu_init: unsafe extern "C" fn(u32) -> CUresult,
    cu_device_get: unsafe extern "C" fn(*mut CUdevice, i32) -> CUresult,
    cu_ctx_create: unsafe extern "C" fn(*mut CUcontext, u32, CUdevice) -> CUresult,
    cu_ctx_push: unsafe extern "C" fn(CUcontext) -> CUresult,
    cu_ctx_pop: unsafe extern "C" fn(*mut CUcontext) -> CUresult,
    cu_ctx_destroy: unsafe extern "C" fn(CUcontext) -> CUresult,
    cu_mem_alloc: unsafe extern "C" fn(*mut CUdeviceptr, usize) -> CUresult,
    cu_mem_free: unsafe extern "C" fn(CUdeviceptr) -> CUresult,
    cu_memcpy2d: unsafe extern "C" fn(*const CudaMemcpy2D) -> CUresult,
    cu_graphics_gl_register_image: unsafe extern "C" fn(
        *mut CUgraphicsResource, u32, u32, u32,
    ) -> CUresult,
    cu_graphics_map_resources: unsafe extern "C" fn(
        u32, *mut CUgraphicsResource, *mut std::ffi::c_void,
    ) -> CUresult,
    cu_graphics_sub_resource_get_mapped_array: unsafe extern "C" fn(
        *mut CUarray, CUgraphicsResource, u32, u32,
    ) -> CUresult,
    cu_graphics_unmap_resources: unsafe extern "C" fn(
        u32, *mut CUgraphicsResource, *mut std::ffi::c_void,
    ) -> CUresult,
    cu_graphics_unregister_resource: unsafe extern "C" fn(CUgraphicsResource) -> CUresult,
}

impl CudaApi {
    fn load() -> Option<Self> {
        let lib = unsafe { Library::new("libcuda.so.1") }.ok()?;
        unsafe {
            Some(CudaApi {
                cu_init: *lib.get(b"cuInit\0").ok()?,
                cu_device_get: *lib.get(b"cuDeviceGet\0").ok()?,
                cu_ctx_create: *lib.get(b"cuCtxCreate_v2\0").ok()?,
                cu_ctx_push: *lib.get(b"cuCtxPushCurrent_v2\0").ok()?,
                cu_ctx_pop: *lib.get(b"cuCtxPopCurrent_v2\0").ok()?,
                cu_ctx_destroy: *lib.get(b"cuCtxDestroy_v2\0").ok()?,
                cu_mem_alloc: *lib.get(b"cuMemAlloc_v2\0").ok()?,
                cu_mem_free: *lib.get(b"cuMemFree_v2\0").ok()?,
                cu_memcpy2d: *lib.get(b"cuMemcpy2D_v2\0").ok()?,
                cu_graphics_gl_register_image: *lib.get(b"cuGraphicsGLRegisterImage\0").ok()?,
                cu_graphics_map_resources: *lib.get(b"cuGraphicsMapResources\0").ok()?,
                cu_graphics_sub_resource_get_mapped_array: *lib.get(b"cuGraphicsSubResourceGetMappedArray\0").ok()?,
                cu_graphics_unmap_resources: *lib.get(b"cuGraphicsUnmapResources\0").ok()?,
                cu_graphics_unregister_resource: *lib.get(b"cuGraphicsUnregisterResource\0").ok()?,
                _lib: lib,
            })
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// EGL constants for DMA-BUF import
// ────────────────────────────────────────────────────────────────────────────

const EGL_LINUX_DMA_BUF_EXT: khronos_egl::Int = 0x3270;
const EGL_WIDTH: khronos_egl::Int = 0x3057;
const EGL_HEIGHT: khronos_egl::Int = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: khronos_egl::Int = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: khronos_egl::Int = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: khronos_egl::Int = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: khronos_egl::Int = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: khronos_egl::Int = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: khronos_egl::Int = 0x3444;
const EGL_NONE: khronos_egl::Int = 0x3038;

pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ffffffffffffff;

/// glEGLImageTargetTexture2DOES function pointer type
type GlEglImageTargetTexture2DOesFn = unsafe extern "C" fn(u32, *mut std::ffi::c_void);

// ────────────────────────────────────────────────────────────────────────────
// GPU Backend detection
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackendType {
    /// NVIDIA: EGL → GL → CUDA → NVENC (h264_nvenc)
    Cuda,
    /// AMD/Intel: DRM PRIME → VA-API surface → h264_vaapi
    Vaapi,
}

// ────────────────────────────────────────────────────────────────────────────
// NVIDIA CUDA backend
// ────────────────────────────────────────────────────────────────────────────

struct CudaBackend {
    cuda: CudaApi,
    cu_ctx: CUcontext,
    egl: khronos_egl::DynamicInstance<khronos_egl::EGL1_5>,
    egl_display: khronos_egl::Display,
    egl_context: khronos_egl::Context,
    gl_image_target_fn: GlEglImageTargetTexture2DOesFn,
    gl_tex: u32,
    /// Reusable CUDA linear buffer: (ptr, width, height)
    dev_buf: Option<(CUdeviceptr, u32, u32)>,
}

impl CudaBackend {
    fn new() -> Option<Self> {
        // Load CUDA driver API
        let cuda = CudaApi::load()?;
        let rc = unsafe { (cuda.cu_init)(0) };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuInit failed: {}", rc);
            return None;
        }
        let mut cu_dev: CUdevice = 0;
        if unsafe { (cuda.cu_device_get)(&mut cu_dev, 0) } != CUDA_SUCCESS {
            eprintln!("[gpu] cuDeviceGet failed");
            return None;
        }
        let mut cu_ctx: CUcontext = std::ptr::null_mut();
        if unsafe { (cuda.cu_ctx_create)(&mut cu_ctx, 0, cu_dev) } != CUDA_SUCCESS {
            eprintln!("[gpu] cuCtxCreate failed");
            return None;
        }

        // Load EGL dynamically
        let egl_lib = match unsafe { Library::new("libEGL.so.1") } {
            Ok(lib) => lib,
            Err(e) => {
                eprintln!("[gpu] Failed to load libEGL.so.1: {}", e);
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };
        let egl = match unsafe {
            khronos_egl::DynamicInstance::<khronos_egl::EGL1_5>::load_required_from(egl_lib)
        } {
            Ok(egl) => egl,
            Err(e) => {
                eprintln!("[gpu] EGL load failed: {}", e);
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };

        // Surfaceless EGL display
        let egl_display = match egl.get_display(khronos_egl::DEFAULT_DISPLAY) {
            Some(d) => d,
            None => {
                eprintln!("[gpu] eglGetDisplay failed");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };
        if egl.initialize(egl_display).is_err() {
            eprintln!("[gpu] eglInitialize failed");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        if egl.bind_api(khronos_egl::OPENGL_ES_API).is_err() {
            eprintln!("[gpu] eglBindAPI(GLES) failed");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        let config_attribs = [
            khronos_egl::RENDERABLE_TYPE, khronos_egl::OPENGL_ES2_BIT,
            khronos_egl::SURFACE_TYPE, 0,
            EGL_NONE,
        ];
        let config = match egl.choose_first_config(egl_display, &config_attribs) {
            Ok(Some(c)) => c,
            _ => {
                eprintln!("[gpu] eglChooseConfig failed");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };

        let ctx_attribs = [khronos_egl::CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        let no_ctx = khronos_egl::Context(std::ptr::null_mut());
        let egl_context = match egl.create_context(egl_display, config, no_ctx, &ctx_attribs) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[gpu] eglCreateContext failed: {}", e);
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };
        if egl.make_current(egl_display, None, None, Some(egl_context)).is_err() {
            eprintln!("[gpu] eglMakeCurrent failed");
            let _ = egl.destroy_context(egl_display, egl_context);
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        // Load GL function pointers (needed for glGenTextures, glBindTexture)
        gl::load_with(|name| {
            let c_name = std::ffi::CString::new(name).unwrap();
            egl.get_proc_address(c_name.as_c_str())
                .map_or(std::ptr::null(), |p| p as *const std::ffi::c_void)
        });

        // Load glEGLImageTargetTexture2DOES extension
        let gl_image_target_fn: GlEglImageTargetTexture2DOesFn = {
            let c_name = std::ffi::CString::new("glEGLImageTargetTexture2DOES").unwrap();
            match egl.get_proc_address(c_name.as_c_str()) {
                Some(p) => unsafe { std::mem::transmute(p) },
                None => {
                    eprintln!("[gpu] glEGLImageTargetTexture2DOES not available");
                    let _ = egl.destroy_context(egl_display, egl_context);
                    unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                    return None;
                }
            }
        };

        // Create reusable GL texture
        let mut gl_tex: u32 = 0;
        unsafe { gl::GenTextures(1, &mut gl_tex) };
        if gl_tex == 0 {
            eprintln!("[gpu] glGenTextures failed");
            let _ = egl.destroy_context(egl_display, egl_context);
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        eprintln!("[gpu] CUDA backend ready (device={})", cu_dev);

        Some(CudaBackend {
            cuda,
            cu_ctx,
            egl,
            egl_display,
            egl_context,
            gl_image_target_fn,
            gl_tex,
            dev_buf: None,
        })
    }

    /// Import DMA-BUF into CUDA device memory.
    /// Returns CUdeviceptr pointing to BGRA pixel data in GPU-linear memory.
    fn import_dmabuf(
        &mut self,
        fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
        modifier: u64,
    ) -> Option<CUdeviceptr> {
        // Ensure EGL + CUDA contexts are current
        self.egl.make_current(self.egl_display, None, None, Some(self.egl_context)).ok()?;
        unsafe { (self.cuda.cu_ctx_push)(self.cu_ctx) };

        // 1. Create EGLImage from DMA-BUF
        let mut attribs = vec![
            EGL_WIDTH, width as khronos_egl::Int,
            EGL_HEIGHT, height as khronos_egl::Int,
            EGL_LINUX_DRM_FOURCC_EXT, drm_format as khronos_egl::Int,
            EGL_DMA_BUF_PLANE0_FD_EXT, fd,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
            EGL_DMA_BUF_PLANE0_PITCH_EXT, stride as khronos_egl::Int,
        ];
        if modifier != DRM_FORMAT_MOD_INVALID {
            attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT);
            attribs.push((modifier & 0xFFFFFFFF) as khronos_egl::Int);
            attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT);
            attribs.push((modifier >> 32) as khronos_egl::Int);
        }
        attribs.push(EGL_NONE);

        let no_ctx = khronos_egl::Context(std::ptr::null_mut());
        let no_buf = khronos_egl::ClientBuffer(std::ptr::null_mut());
        let egl_image = match self.egl.create_image(
            self.egl_display, no_ctx, EGL_LINUX_DMA_BUF_EXT as u32, no_buf, &attribs,
        ) {
            Ok(img) => img,
            Err(e) => {
                eprintln!("[gpu] eglCreateImage failed: {}", e);
                self.pop_ctx();
                return None;
            }
        };

        // 2. Bind EGLImage to GL texture
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.gl_tex);
            (self.gl_image_target_fn)(gl::TEXTURE_2D, egl_image.as_ptr() as *mut std::ffi::c_void);
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        // 3. Register GL texture with CUDA
        let mut cu_resource: CUgraphicsResource = std::ptr::null_mut();
        let rc = unsafe {
            (self.cuda.cu_graphics_gl_register_image)(
                &mut cu_resource, self.gl_tex, GL_TEXTURE_2D, CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY,
            )
        };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuGraphicsGLRegisterImage failed: {}", rc);
            let _ = self.egl.destroy_image(self.egl_display, egl_image);
            self.pop_ctx();
            return None;
        }

        // 4. Map → get CUarray
        let rc = unsafe {
            (self.cuda.cu_graphics_map_resources)(1, &mut cu_resource, std::ptr::null_mut())
        };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuGraphicsMapResources failed: {}", rc);
            unsafe { (self.cuda.cu_graphics_unregister_resource)(cu_resource) };
            let _ = self.egl.destroy_image(self.egl_display, egl_image);
            self.pop_ctx();
            return None;
        }

        let mut cu_array: CUarray = std::ptr::null_mut();
        let rc = unsafe {
            (self.cuda.cu_graphics_sub_resource_get_mapped_array)(
                &mut cu_array, cu_resource, 0, 0,
            )
        };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuGraphicsSubResourceGetMappedArray failed: {}", rc);
            unsafe {
                (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource, std::ptr::null_mut());
                (self.cuda.cu_graphics_unregister_resource)(cu_resource);
            }
            let _ = self.egl.destroy_image(self.egl_display, egl_image);
            self.pop_ctx();
            return None;
        }

        // 5. Ensure device buffer matches dimensions
        let bpp = 4u32; // BGRA
        let pitch = width * bpp;
        let buf_size = (pitch * height) as usize;
        let dev_ptr = match self.ensure_dev_buffer(width, height, buf_size) {
            Some(p) => p,
            None => {
                unsafe {
                    (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource, std::ptr::null_mut());
                    (self.cuda.cu_graphics_unregister_resource)(cu_resource);
                }
                let _ = self.egl.destroy_image(self.egl_display, egl_image);
                self.pop_ctx();
                return None;
            }
        };

        // 6. cuMemcpy2D: CUarray → CUdeviceptr (GPU-to-GPU, <0.1ms for 1080p)
        let copy = CudaMemcpy2D::array_to_device(
            cu_array, dev_ptr, pitch as usize, (width * bpp) as usize, height as usize,
        );
        let rc = unsafe { (self.cuda.cu_memcpy2d)(&copy) };

        // Cleanup per-frame resources (dev_buf is reused across frames)
        unsafe {
            (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource, std::ptr::null_mut());
            (self.cuda.cu_graphics_unregister_resource)(cu_resource);
        }
        let _ = self.egl.destroy_image(self.egl_display, egl_image);
        self.pop_ctx();

        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuMemcpy2D failed: {}", rc);
            return None;
        }

        Some(dev_ptr)
    }

    fn ensure_dev_buffer(&mut self, width: u32, height: u32, size: usize) -> Option<CUdeviceptr> {
        if let Some((ptr, w, h)) = self.dev_buf {
            if w == width && h == height {
                return Some(ptr);
            }
            unsafe { (self.cuda.cu_mem_free)(ptr) };
            self.dev_buf = None;
        }
        let mut ptr: CUdeviceptr = 0;
        let rc = unsafe { (self.cuda.cu_mem_alloc)(&mut ptr, size) };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuMemAlloc({} bytes) failed: {}", size, rc);
            return None;
        }
        self.dev_buf = Some((ptr, width, height));
        Some(ptr)
    }

    fn push_ctx(&self) {
        unsafe { (self.cuda.cu_ctx_push)(self.cu_ctx) };
    }

    fn pop_ctx(&self) {
        unsafe {
            let mut _dummy: CUcontext = std::ptr::null_mut();
            (self.cuda.cu_ctx_pop)(&mut _dummy);
        }
    }
}

impl Drop for CudaBackend {
    fn drop(&mut self) {
        if let Some((ptr, _, _)) = self.dev_buf.take() {
            unsafe { (self.cuda.cu_mem_free)(ptr) };
        }
        unsafe { gl::DeleteTextures(1, &self.gl_tex) };
        let _ = self.egl.destroy_context(self.egl_display, self.egl_context);
        unsafe { (self.cuda.cu_ctx_destroy)(self.cu_ctx) };
    }
}

// ────────────────────────────────────────────────────────────────────────────
// VA-API backend (AMD/Intel) — uses FFmpeg's DRM→VAAPI hwframe mapping
// ────────────────────────────────────────────────────────────────────────────

struct VaapiBackend {
    /// FFmpeg DRM hw_device_ctx (AVBufferRef*)
    drm_device_ref: *mut ffmpeg_sys_next::AVBufferRef,
    /// FFmpeg VAAPI hw_device_ctx derived from DRM (AVBufferRef*)
    vaapi_device_ref: *mut ffmpeg_sys_next::AVBufferRef,
    /// VAAPI hw_frames_ctx (AVBufferRef*) — set after encoder init
    vaapi_frames_ref: *mut ffmpeg_sys_next::AVBufferRef,
}

impl VaapiBackend {
    fn new() -> Option<Self> {
        use ffmpeg_sys_next::*;

        // Try common DRI render node paths
        let render_node = if std::path::Path::new("/dev/dri/renderD128").exists() {
            "/dev/dri/renderD128"
        } else if std::path::Path::new("/dev/dri/renderD129").exists() {
            "/dev/dri/renderD129"
        } else {
            eprintln!("[gpu] No DRI render node found");
            return None;
        };

        unsafe {
            // Create DRM device context
            let render_cstr = std::ffi::CString::new(render_node).ok()?;
            let mut drm_device_ref: *mut AVBufferRef = std::ptr::null_mut();
            let rc = av_hwdevice_ctx_create(
                &mut drm_device_ref,
                AVHWDeviceType_AV_HWDEVICE_TYPE_DRM,
                render_cstr.as_ptr(),
                std::ptr::null_mut(),
                0,
            );
            if rc < 0 || drm_device_ref.is_null() {
                eprintln!("[gpu] av_hwdevice_ctx_create(DRM, {}) failed: {}", render_node, rc);
                return None;
            }

            // Derive VAAPI device from DRM device
            let mut vaapi_device_ref: *mut AVBufferRef = std::ptr::null_mut();
            let rc = av_hwdevice_ctx_create_derived(
                &mut vaapi_device_ref,
                AVHWDeviceType_AV_HWDEVICE_TYPE_VAAPI,
                drm_device_ref,
                0,
            );
            if rc < 0 || vaapi_device_ref.is_null() {
                eprintln!("[gpu] av_hwdevice_ctx_create_derived(VAAPI from DRM) failed: {}", rc);
                av_buffer_unref(&mut drm_device_ref);
                return None;
            }

            eprintln!("[gpu] VA-API backend ready (DRM: {})", render_node);

            Some(VaapiBackend {
                drm_device_ref,
                vaapi_device_ref,
                vaapi_frames_ref: std::ptr::null_mut(),
            })
        }
    }

    /// Initialize the VAAPI frames context for the given dimensions.
    /// Must be called before encoding.
    fn init_frames_ctx(&mut self, width: u32, height: u32) -> Result<(), String> {
        use ffmpeg_sys_next::*;
        unsafe {
            if !self.vaapi_frames_ref.is_null() {
                av_buffer_unref(&mut self.vaapi_frames_ref);
            }

            let frames_ref = av_hwframe_ctx_alloc(self.vaapi_device_ref);
            if frames_ref.is_null() {
                return Err("av_hwframe_ctx_alloc(VAAPI) failed".into());
            }
            let frames_ctx = (*frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AV_PIX_FMT_VAAPI;
            (*frames_ctx).sw_format = AV_PIX_FMT_NV12;
            (*frames_ctx).width = width as i32;
            (*frames_ctx).height = height as i32;
            (*frames_ctx).initial_pool_size = 8;

            let rc = av_hwframe_ctx_init(frames_ref);
            if rc < 0 {
                av_buffer_unref(&mut (frames_ref as *mut AVBufferRef));
                return Err(format!("av_hwframe_ctx_init(VAAPI) failed: {}", rc));
            }

            self.vaapi_frames_ref = frames_ref;
            Ok(())
        }
    }

    /// Map a DMA-BUF frame descriptor into a VAAPI surface AVFrame.
    /// The returned AVFrame has format=VAAPI and can be sent directly to h264_vaapi.
    ///
    /// # Safety
    /// The `drm_desc` must remain valid until the returned AVFrame is consumed by the encoder.
    fn map_dmabuf_to_vaapi(
        &self,
        drm_desc: &ffmpeg_sys_next::AVDRMFrameDescriptor,
        width: u32,
        height: u32,
    ) -> Option<ffmpeg_next::frame::Video> {
        use ffmpeg_sys_next::*;

        if self.vaapi_frames_ref.is_null() {
            eprintln!("[gpu] VAAPI frames context not initialized");
            return None;
        }

        unsafe {
            // Create DRM_PRIME source frame
            let drm_frame = av_frame_alloc();
            if drm_frame.is_null() {
                return None;
            }
            (*drm_frame).format = AV_PIX_FMT_DRM_PRIME;
            (*drm_frame).width = width as i32;
            (*drm_frame).height = height as i32;
            // data[0] points to the AVDRMFrameDescriptor
            (*drm_frame).data[0] = drm_desc as *const _ as *mut u8;

            // Allocate VAAPI destination frame from the frames pool
            let vaapi_frame = av_frame_alloc();
            if vaapi_frame.is_null() {
                av_frame_free(&mut (drm_frame as *mut AVFrame));
                return None;
            }
            (*vaapi_frame).format = AV_PIX_FMT_VAAPI;
            (*vaapi_frame).hw_frames_ctx = av_buffer_ref(self.vaapi_frames_ref);

            let rc = av_hwframe_get_buffer(self.vaapi_frames_ref, vaapi_frame, 0);
            if rc < 0 {
                eprintln!("[gpu] av_hwframe_get_buffer(VAAPI) failed: {}", rc);
                av_frame_free(&mut (drm_frame as *mut AVFrame));
                av_frame_free(&mut (vaapi_frame as *mut AVFrame));
                return None;
            }

            // Map DRM_PRIME → VAAPI (zero-copy on same GPU)
            let rc = av_hwframe_map(
                vaapi_frame,
                drm_frame,
                AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32,
            );

            // Free the DRM frame wrapper (the DMA-BUF fd is NOT closed — we dup'd it)
            (*drm_frame).data[0] = std::ptr::null_mut(); // prevent double-free
            av_frame_free(&mut (drm_frame as *mut AVFrame));

            if rc < 0 {
                eprintln!("[gpu] av_hwframe_map(DRM→VAAPI) failed: {}", rc);
                av_frame_free(&mut (vaapi_frame as *mut AVFrame));
                return None;
            }

            // Wrap raw AVFrame* in ffmpeg_next::frame::Video for safe management
            Some(ffmpeg_next::frame::Video::wrap(vaapi_frame))
        }
    }

    /// Get a reference to the VAAPI hw_device_ctx for encoder setup.
    fn vaapi_device_ref(&self) -> *mut ffmpeg_sys_next::AVBufferRef {
        self.vaapi_device_ref
    }

    /// Get a reference to the VAAPI hw_frames_ctx for encoder setup.
    fn vaapi_frames_ref(&self) -> *mut ffmpeg_sys_next::AVBufferRef {
        self.vaapi_frames_ref
    }
}

impl Drop for VaapiBackend {
    fn drop(&mut self) {
        use ffmpeg_sys_next::*;
        unsafe {
            if !self.vaapi_frames_ref.is_null() {
                av_buffer_unref(&mut self.vaapi_frames_ref);
            }
            if !self.vaapi_device_ref.is_null() {
                av_buffer_unref(&mut self.vaapi_device_ref);
            }
            if !self.drm_device_ref.is_null() {
                av_buffer_unref(&mut self.drm_device_ref);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// GpuContext — public API
// ────────────────────────────────────────────────────────────────────────────

/// GPU context for DMA-BUF zero-copy import.
/// Detects the best available GPU backend at creation time.
pub struct GpuContext {
    backend: GpuBackendInner,
}

enum GpuBackendInner {
    Cuda(CudaBackend),
    Vaapi(VaapiBackend),
}

// GpuContext is used on a single thread (the video pipeline thread)
unsafe impl Send for GpuContext {}

impl GpuContext {
    /// Try to initialize GPU interop. Returns None if no GPU backend is available.
    /// Tries NVIDIA (CUDA) first, then AMD/Intel (VAAPI).
    pub fn new() -> Option<Self> {
        eprintln!("[gpu] Probing GPU backends...");

        // Try CUDA first (NVIDIA)
        if let Some(cuda) = CudaBackend::new() {
            return Some(GpuContext { backend: GpuBackendInner::Cuda(cuda) });
        }

        // Try VAAPI (AMD/Intel)
        if let Some(vaapi) = VaapiBackend::new() {
            return Some(GpuContext { backend: GpuBackendInner::Vaapi(vaapi) });
        }

        eprintln!("[gpu] No GPU backend available, falling back to CPU encoding");
        None
    }

    pub fn backend_type(&self) -> GpuBackendType {
        match &self.backend {
            GpuBackendInner::Cuda(_) => GpuBackendType::Cuda,
            GpuBackendInner::Vaapi(_) => GpuBackendType::Vaapi,
        }
    }

    /// NVIDIA only: import DMA-BUF into CUDA device memory.
    /// Returns CUdeviceptr (u64) pointing to BGRA pixel data.
    pub fn import_dmabuf_cuda(
        &mut self,
        fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
        modifier: u64,
    ) -> Option<u64> {
        match &mut self.backend {
            GpuBackendInner::Cuda(cuda) => {
                cuda.import_dmabuf(fd, width, height, stride, drm_format, modifier)
            }
            _ => None,
        }
    }

    /// AMD/Intel only: fill an AVDRMFrameDescriptor and map to VAAPI surface.
    /// Returns an AVFrame with format=VAAPI ready for h264_vaapi.
    pub fn map_dmabuf_vaapi(
        &self,
        fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
        modifier: u64,
    ) -> Option<ffmpeg_next::frame::Video> {
        match &self.backend {
            GpuBackendInner::Vaapi(vaapi) => {
                // Build AVDRMFrameDescriptor on the stack
                let drm_desc = build_drm_descriptor(fd, width, height, stride, drm_format, modifier);
                vaapi.map_dmabuf_to_vaapi(&drm_desc, width, height)
            }
            _ => None,
        }
    }

    /// Initialize VAAPI frames context (must be called before map_dmabuf_vaapi).
    pub fn init_vaapi_frames(&mut self, width: u32, height: u32) -> Result<(), String> {
        match &mut self.backend {
            GpuBackendInner::Vaapi(vaapi) => vaapi.init_frames_ctx(width, height),
            _ => Err("Not a VAAPI backend".into()),
        }
    }

    /// Get VAAPI hw_device_ctx for encoder setup.
    pub fn vaapi_device_ref(&self) -> *mut ffmpeg_sys_next::AVBufferRef {
        match &self.backend {
            GpuBackendInner::Vaapi(vaapi) => vaapi.vaapi_device_ref(),
            _ => std::ptr::null_mut(),
        }
    }

    /// Get VAAPI hw_frames_ctx for encoder setup.
    pub fn vaapi_frames_ref(&self) -> *mut ffmpeg_sys_next::AVBufferRef {
        match &self.backend {
            GpuBackendInner::Vaapi(vaapi) => vaapi.vaapi_frames_ref(),
            _ => std::ptr::null_mut(),
        }
    }

    /// NVIDIA only: push CUDA context onto current thread's stack.
    pub fn push_cuda_ctx(&self) {
        if let GpuBackendInner::Cuda(cuda) = &self.backend {
            cuda.push_ctx();
        }
    }

    /// NVIDIA only: pop CUDA context from current thread's stack.
    pub fn pop_cuda_ctx(&self) {
        if let GpuBackendInner::Cuda(cuda) = &self.backend {
            cuda.pop_ctx();
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

/// Build an AVDRMFrameDescriptor for a single-plane DMA-BUF.
fn build_drm_descriptor(
    fd: i32,
    _width: u32,
    height: u32,
    stride: u32,
    drm_format: u32,
    modifier: u64,
) -> ffmpeg_sys_next::AVDRMFrameDescriptor {
    use ffmpeg_sys_next::*;
    let total_size = stride as usize * height as usize;
    AVDRMFrameDescriptor {
        nb_objects: 1,
        objects: [
            AVDRMObjectDescriptor {
                fd: fd as libc::c_int,
                size: total_size,
                format_modifier: modifier,
            },
            unsafe { std::mem::zeroed() },
            unsafe { std::mem::zeroed() },
            unsafe { std::mem::zeroed() },
        ],
        nb_layers: 1,
        layers: [
            AVDRMLayerDescriptor {
                format: drm_format,
                nb_planes: 1,
                planes: [
                    AVDRMPlaneDescriptor {
                        object_index: 0,
                        offset: 0,
                        pitch: stride as isize,
                    },
                    unsafe { std::mem::zeroed() },
                    unsafe { std::mem::zeroed() },
                    unsafe { std::mem::zeroed() },
                ],
            },
            unsafe { std::mem::zeroed() },
            unsafe { std::mem::zeroed() },
            unsafe { std::mem::zeroed() },
        ],
    }
}

/// Convert PipeWire SPA VideoFormat to DRM fourcc code.
pub fn spa_format_to_drm_fourcc(fmt: pipewire::spa::param::video::VideoFormat) -> u32 {
    use pipewire::spa::param::video::VideoFormat;
    // DRM fourcc = fourcc_code(a,b,c,d) = a | (b<<8) | (c<<16) | (d<<24)
    // u32::from_le_bytes matches this layout.
    //
    // DRM channel naming is MSB-first; memory layout on LE is reversed:
    //   DRM_FORMAT_ARGB8888 = [31:24]=A [23:16]=R [15:8]=G [7:0]=B
    //   Memory bytes: B G R A → matches PipeWire "BGRA" byte order
    match fmt {
        VideoFormat::BGRA => u32::from_le_bytes(*b"AR24"), // DRM_FORMAT_ARGB8888
        VideoFormat::BGRx => u32::from_le_bytes(*b"XR24"), // DRM_FORMAT_XRGB8888
        VideoFormat::RGBA => u32::from_le_bytes(*b"AB24"), // DRM_FORMAT_ABGR8888
        VideoFormat::RGBx => u32::from_le_bytes(*b"XB24"), // DRM_FORMAT_XBGR8888
        _ => 0,
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`
Expected: Compiles on Linux. The `ffmpeg_next::frame::Video::wrap()` call may need verification — if it doesn't exist, use `from_raw()` or create from raw pointer differently. Check and fix.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/media/gpu_interop.rs src-tauri/src/media/mod.rs
git commit -m "feat(gpu): add EGL/GL/CUDA and VA-API interop module for DMA-BUF zero-copy"
```

---

### Task 4: Modify PipeWire Capture for DMA-BUF Negotiation

**Files:**
- Modify: `src-tauri/src/media/capture_pipewire.rs`

Changes:
1. Create a second `SyncSender<DmaBufFrame>` channel for GPU frames
2. Remove `MAP_BUFFERS` flag from `stream.connect()` — lets PipeWire provide DMA-BUF
3. In the process callback: check `d.type_()` — DMA-BUF goes to `gpu_tx`, SHM goes to `tx`
4. Pass the `gpu_receiver` back through `CaptureOutput`

- [ ] **Step 1: Update imports**

Add at the top of the file, after existing imports:

```rust
use std::os::fd::FromRawFd;
use super::capture::DmaBufFrame;
```

- [ ] **Step 2: Create the DMA-BUF channel alongside the existing RawFrame channel**

In `start_capture()`, after the existing `sync_channel` (line 40):

```rust
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(4);
        let (gpu_tx, gpu_rx) = std::sync::mpsc::sync_channel::<DmaBufFrame>(4);
```

Update the `CaptureData` struct to hold both senders:

```rust
struct CaptureData {
    format: spa::param::video::VideoInfoRaw,
    tx: SyncSender<RawFrame>,
    gpu_tx: SyncSender<DmaBufFrame>,
    target_width: u32,
    target_height: u32,
    target_fps: u32,
    frame_count: u64,
    last_capture_us: u64,
    start: Instant,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    dim_tx: Option<std::sync::mpsc::SyncSender<(u32, u32)>>,
}
```

Update the `CaptureData` construction in `pipewire_capture_loop` to include `gpu_tx`:

```rust
    let data = CaptureData {
        format: Default::default(),
        tx,
        gpu_tx,
        target_width,
        target_height,
        target_fps: config.target_fps,
        frame_count: 0,
        last_capture_us: 0,
        start: Instant::now(),
        quit_mainloop: mainloop.downgrade(),
        dim_tx: Some(dim_tx),
    };
```

Update the `pipewire_capture_loop` function signature to accept `gpu_tx`:

```rust
fn pipewire_capture_loop(
    fd: OwnedFd,
    node_id: u32,
    tx: SyncSender<RawFrame>,
    gpu_tx: SyncSender<DmaBufFrame>,
    config: CaptureConfig,
    _dbus_conn: zbus::blocking::Connection,
    dim_tx: std::sync::mpsc::SyncSender<(u32, u32)>,
) -> Result<(), String> {
```

And update the call in `start_capture`:

```rust
                if let Err(e) = pipewire_capture_loop(pw_fd, node_id, tx, gpu_tx, config, dbus_conn, dim_tx) {
```

- [ ] **Step 3: Update CaptureOutput construction**

Change line 66 to return the gpu_receiver:

```rust
        Ok(CaptureOutput { receiver: rx, width, height, gpu_receiver: Some(gpu_rx) })
```

- [ ] **Step 4: Remove MAP_BUFFERS from stream.connect()**

Change line 585-591:

```rust
    // Don't use MAP_BUFFERS — allows PipeWire to provide DMA-BUF when available.
    // For SHM buffers, PipeWire maps them automatically (data() returns Some).
    // For DMA-BUF, data() returns None — we use fd() instead.
    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT,
        &mut params,
    )
```

- [ ] **Step 5: Update the process callback to handle DMA-BUF and SHM**

Replace the entire `.process(|stream, data| { ... })` callback body:

```rust
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            // Rate-limit: skip frames arriving faster than target FPS
            let now_us = data.start.elapsed().as_micros() as u64;
            let frame_interval_us = 1_000_000 / data.target_fps.max(1) as u64;
            if now_us.saturating_sub(data.last_capture_us) < frame_interval_us {
                drop(buffer);
                return;
            }

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            let d = &mut datas[0];
            let chunk_size = d.chunk().size() as usize;
            let stride = d.chunk().stride() as usize;
            let buf_type = d.type_();

            if chunk_size == 0 || stride == 0 {
                return;
            }

            let src_w = data.format.size().width;
            let src_h = data.format.size().height;
            let fmt = data.format.format();

            data.frame_count += 1;
            if data.frame_count <= 3 || data.frame_count % 120 == 0 {
                eprintln!(
                    "[capture] Frame {} ({:?} {}x{}, stride={}, chunk={} bytes, buf_type={:?}, {:.1}s)",
                    data.frame_count, fmt, src_w, src_h, stride, chunk_size,
                    buf_type, data.start.elapsed().as_secs_f64()
                );
            }

            let is_bgra = fmt == spa::param::video::VideoFormat::BGRA
                || fmt == spa::param::video::VideoFormat::BGRx;
            let is_rgba = fmt == spa::param::video::VideoFormat::RGBA
                || fmt == spa::param::video::VideoFormat::RGBx;

            if !is_bgra && !is_rgba {
                if data.frame_count <= 3 {
                    eprintln!("[capture] Unsupported format {:?}, skipping", fmt);
                }
                return;
            }

            use pw::spa::buffer::DataType;

            if buf_type == DataType::DmaBuf {
                // ── DMA-BUF path: dup fd, send to GPU channel ──
                let raw_fd = d.fd();
                let dup_fd = unsafe { libc::dup(raw_fd) };
                if dup_fd < 0 {
                    eprintln!("[capture] dup(dmabuf fd={}) failed", raw_fd);
                    return;
                }
                let owned_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(dup_fd) };

                let drm_format = super::gpu_interop::spa_format_to_drm_fourcc(fmt);
                if drm_format == 0 {
                    eprintln!("[capture] No DRM fourcc for {:?}", fmt);
                    return;
                }

                if data.frame_count <= 3 {
                    eprintln!("[capture] DMA-BUF frame: fd={}, {}x{}, stride={}, drm_fmt=0x{:08x}",
                        dup_fd, src_w, src_h, stride, drm_format);
                }

                let frame = DmaBufFrame {
                    fd: owned_fd,
                    width: src_w,
                    height: src_h,
                    stride: stride as u32,
                    drm_format,
                    modifier: super::gpu_interop::DRM_FORMAT_MOD_INVALID,
                    timestamp_us: now_us,
                };

                match data.gpu_tx.try_send(frame) {
                    Ok(()) => { data.last_capture_us = now_us; }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        eprintln!("[capture] GPU frame channel closed, stopping");
                        if let Some(ml) = data.quit_mainloop.upgrade() { ml.quit(); }
                    }
                }
            } else {
                // ── SHM path: copy data to Vec (existing behavior) ──
                let chunk_offset = d.chunk().offset() as usize;
                let Some(raw_data) = d.data() else { return };
                let raw_data = &raw_data[chunk_offset..][..chunk_size];

                let pixel_format = if is_bgra {
                    super::capture::PixelFormat::BGRA
                } else {
                    super::capture::PixelFormat::RGBA
                };

                let frame = RawFrame {
                    data: raw_data.to_vec(),
                    width: src_w,
                    height: src_h,
                    stride,
                    pixel_format,
                    timestamp_us: now_us,
                };

                match data.tx.try_send(frame) {
                    Ok(()) => { data.last_capture_us = now_us; }
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        eprintln!("[capture] Frame channel closed, stopping");
                        if let Some(ml) = data.quit_mainloop.upgrade() { ml.quit(); }
                    }
                }
            }
        })
```

- [ ] **Step 6: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`
Expected: Compiles. Warnings about unused `gpu_receiver` are fine (consumed in later tasks).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/media/capture_pipewire.rs
git commit -m "feat(capture): negotiate DMA-BUF from PipeWire, send DmaBufFrame via gpu channel"
```

---

### Task 5: Add GPU Encoding Paths to Encoder

**Files:**
- Modify: `src-tauri/src/media/encoder.rs`

Adds two GPU encoding methods:
1. **NVIDIA:** `init_cuda_hw()` + `encode_cuda_frame(dev_ptr)` — NVENC reads from CUdeviceptr
2. **VAAPI:** `init_vaapi_hw()` + `encode_vaapi_frame(vaapi_frame)` — h264_vaapi reads from VASurface

The existing CPU encoding paths (`encode_bgra_frame`, `encode_nv12_frame`) are completely unchanged.

- [ ] **Step 1: Add GPU state fields to H264Encoder**

Add after the `bgra_scaler` field (line 193):

```rust
    /// NVIDIA CUDA: FFmpeg hw_device_ctx (AVBufferRef*). When set, NVENC
    /// reads frames from GPU memory instead of system memory.
    #[cfg(target_os = "linux")]
    cuda_hw_device_ref: *mut std::ffi::c_void,
    /// NVIDIA CUDA: hw_frames_ctx (AVBufferRef*) for frame pool.
    #[cfg(target_os = "linux")]
    cuda_hw_frames_ref: *mut std::ffi::c_void,

    /// VA-API: whether this encoder instance uses h264_vaapi with HW frames.
    #[cfg(target_os = "linux")]
    is_vaapi_hw: bool,
```

Initialize them in the constructor's struct literal:

```rust
            #[cfg(target_os = "linux")]
            cuda_hw_device_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            cuda_hw_frames_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            is_vaapi_hw: false,
```

- [ ] **Step 2: Add NVIDIA CUDA hw encoding init**

Add after `find_hw_encoder()`:

```rust
    /// Initialize CUDA hardware frame encoding for NVENC.
    /// After this, `encode_cuda_frame()` accepts CUdeviceptr from GPU memory.
    #[cfg(target_os = "linux")]
    pub fn init_cuda_hw(&mut self) -> Result<(), String> {
        use ffmpeg_sys_next::*;

        unsafe {
            // Create CUDA hw_device_ctx
            let mut hw_device_ref: *mut AVBufferRef = std::ptr::null_mut();
            let rc = av_hwdevice_ctx_create(
                &mut hw_device_ref,
                AVHWDeviceType_AV_HWDEVICE_TYPE_CUDA,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if rc < 0 || hw_device_ref.is_null() {
                return Err(format!("av_hwdevice_ctx_create(CUDA) failed: {}", rc));
            }

            // Create hw_frames_ctx
            let frames_ref = av_hwframe_ctx_alloc(hw_device_ref);
            if frames_ref.is_null() {
                av_buffer_unref(&mut hw_device_ref);
                return Err("av_hwframe_ctx_alloc(CUDA) failed".into());
            }

            let frames_ctx = (*frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AV_PIX_FMT_CUDA;
            // NVENC accepts BGRA as sw_format — GPU handles BGRA→NV12 internally
            (*frames_ctx).sw_format = if self.supports_bgra_input {
                AV_PIX_FMT_BGRA
            } else {
                AV_PIX_FMT_NV12
            };
            (*frames_ctx).width = self.target_width as i32;
            (*frames_ctx).height = self.target_height as i32;
            (*frames_ctx).initial_pool_size = 4;

            let rc = av_hwframe_ctx_init(frames_ref);
            if rc < 0 {
                av_buffer_unref(&mut (frames_ref as *mut AVBufferRef));
                av_buffer_unref(&mut hw_device_ref);
                return Err(format!("av_hwframe_ctx_init(CUDA) failed: {}", rc));
            }

            self.cuda_hw_device_ref = hw_device_ref as *mut std::ffi::c_void;
            self.cuda_hw_frames_ref = frames_ref as *mut std::ffi::c_void;

            eprintln!("[encoder] CUDA hw_frames_ctx initialized ({}x{})",
                self.target_width, self.target_height);
            Ok(())
        }
    }

    /// Encode a frame from CUDA device memory (NVIDIA zero-copy path).
    /// `dev_ptr` is a CUdeviceptr to BGRA pixel data in GPU-linear memory.
    #[cfg(target_os = "linux")]
    pub fn encode_cuda_frame(
        &mut self,
        dev_ptr: u64,
        width: u32,
        height: u32,
    ) -> Result<Option<EncodedFrame>, String> {
        use ffmpeg_sys_next::*;

        if self.cuda_hw_frames_ref.is_null() {
            return Err("CUDA encoding not initialized".into());
        }

        unsafe {
            // Allocate a CUDA AVFrame from the hw_frames pool
            let frame = av_frame_alloc();
            if frame.is_null() {
                return Err("av_frame_alloc failed".into());
            }

            (*frame).format = AV_PIX_FMT_CUDA;
            (*frame).width = width as i32;
            (*frame).height = height as i32;
            (*frame).hw_frames_ctx = av_buffer_ref(self.cuda_hw_frames_ref as *const AVBufferRef);

            let rc = av_hwframe_get_buffer(
                self.cuda_hw_frames_ref as *mut AVBufferRef, frame, 0,
            );
            if rc < 0 {
                av_frame_free(&mut (frame as *mut AVFrame));
                return Err(format!("av_hwframe_get_buffer(CUDA) failed: {}", rc));
            }

            // Point the CUDA frame's data[0] to our CUdeviceptr
            (*frame).data[0] = dev_ptr as *mut u8;
            let bpp = if self.supports_bgra_input { 4 } else { 1 }; // BGRA=4, NV12 Y plane=1
            (*frame).linesize[0] = (width * bpp) as i32;

            // Set PTS and keyframe flags
            (*frame).pts = self.frame_count as i64;
            if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
                (*frame).pict_type = AV_PICTURE_TYPE_I;
                self.force_next_keyframe = false;
            } else {
                (*frame).pict_type = AV_PICTURE_TYPE_NONE;
            }
            self.frame_count += 1;

            // Wrap in ffmpeg_next for send_frame
            let mut wrapped = ffmpeg_next::frame::Video::wrap(frame);

            let send_result = self.encoder.send_frame(&wrapped);
            match send_result {
                Ok(()) => {}
                Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                    let _ = self.receive_one_packet();
                    self.encoder.send_frame(&wrapped)
                        .map_err(|e| format!("Send CUDA frame (retry): {}", e))?;
                }
                Err(e) => {
                    return Err(format!("Send CUDA frame: {}", e));
                }
            }

            Ok(self.receive_one_packet())
        }
    }

    /// Check if CUDA hw encoding is ready.
    #[cfg(target_os = "linux")]
    pub fn has_cuda_hw(&self) -> bool {
        !self.cuda_hw_frames_ref.is_null()
    }
```

- [ ] **Step 3: Add VAAPI encoding method**

```rust
    /// Create a new H264Encoder specifically for VA-API with hw_device_ctx.
    /// Used when GPU context detected VAAPI backend (AMD/Intel).
    #[cfg(target_os = "linux")]
    pub fn new_vaapi(
        config: &EncoderConfig,
        vaapi_device_ref: *mut ffmpeg_sys_next::AVBufferRef,
        vaapi_frames_ref: *mut ffmpeg_sys_next::AVBufferRef,
    ) -> Result<Self, String> {
        use ffmpeg_sys_next::*;

        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let codec = ffmpeg_next::encoder::find_by_name("h264_vaapi")
            .ok_or("h264_vaapi encoder not found")?;

        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder().video()
            .map_err(|e| format!("VAAPI encoder context: {}", e))?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_gop(config.fps * config.keyframe_interval_secs);
        context.set_max_b_frames(0);
        context.set_format(ffmpeg_next::format::Pixel::VAAPI);

        // Set hw_device_ctx and hw_frames_ctx on the raw AVCodecContext
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).hw_device_ctx = av_buffer_ref(vaapi_device_ref);
            (*ctx_ptr).hw_frames_ctx = av_buffer_ref(vaapi_frames_ref);
        }

        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("rc_mode", "CBR");

        let encoder = context.open_with(opts)
            .map_err(|e| format!("Open h264_vaapi encoder: {}", e))?;

        eprintln!("[encoder] h264_vaapi opened: {}x{} @ {}fps, {}kbps (DMA-BUF zero-copy)",
            config.width, config.height, config.fps, config.bitrate_kbps);

        // NV12 frame is unused in VAAPI path but required by struct
        let nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12, config.width, config.height,
        );

        Ok(H264Encoder {
            encoder,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
            nv12_frame,
            scaler: None,
            supports_bgra_input: false,
            bgra_frame: None,
            bgra_scaler: None,
            #[cfg(target_os = "linux")]
            cuda_hw_device_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            cuda_hw_frames_ref: std::ptr::null_mut(),
            #[cfg(target_os = "linux")]
            is_vaapi_hw: true,
        })
    }

    /// Encode a VA-API hardware frame (AMD/Intel zero-copy path).
    /// The `vaapi_frame` must have format=VAAPI with a valid VASurface.
    #[cfg(target_os = "linux")]
    pub fn encode_vaapi_frame(
        &mut self,
        vaapi_frame: &mut ffmpeg_next::frame::Video,
    ) -> Result<Option<EncodedFrame>, String> {
        vaapi_frame.set_pts(Some(self.frame_count as i64));

        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            vaapi_frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        } else {
            vaapi_frame.set_kind(ffmpeg_next::picture::Type::None);
        }
        self.frame_count += 1;

        match self.encoder.send_frame(vaapi_frame) {
            Ok(()) => {}
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                let _ = self.receive_one_packet();
                self.encoder.send_frame(vaapi_frame)
                    .map_err(|e| format!("Send VAAPI frame (retry): {}", e))?;
            }
            Err(e) => return Err(format!("Send VAAPI frame: {}", e)),
        }

        Ok(self.receive_one_packet())
    }

    /// Check if this encoder uses VA-API hardware frames.
    #[cfg(target_os = "linux")]
    pub fn is_vaapi_hw(&self) -> bool {
        self.is_vaapi_hw
    }
```

- [ ] **Step 4: Add Drop cleanup for CUDA resources**

```rust
impl Drop for H264Encoder {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use ffmpeg_sys_next::*;
            unsafe {
                if !self.cuda_hw_frames_ref.is_null() {
                    av_buffer_unref(&mut (self.cuda_hw_frames_ref as *mut AVBufferRef));
                }
                if !self.cuda_hw_device_ref.is_null() {
                    av_buffer_unref(&mut (self.cuda_hw_device_ref as *mut AVBufferRef));
                }
            }
        }
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`

Check for `Video::wrap()` — if it doesn't exist in ffmpeg-next 8.x, use:
```rust
// Alternative: construct from raw pointer
let wrapped = ffmpeg_next::frame::Video::from(ffmpeg_next::Frame::wrap(frame));
```
Or check the actual API and fix accordingly.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/media/encoder.rs
git commit -m "feat(encoder): add CUDA and VA-API hardware frame encoding paths"
```

---

### Task 6: Update Video Pipeline to Handle GPU Frames

**Files:**
- Modify: `src-tauri/src/media/video_pipeline.rs`

The video pipeline runs on a dedicated thread. It now:
1. Accepts an optional `gpu_receiver` (Linux only)
2. Initializes `GpuContext` on this thread (no AppState locking)
3. Checks `gpu_receiver` first each iteration — if GPU frame available, import and encode on GPU
4. Falls back to `frame_rx` (RawFrame) for SHM frames
5. All GPU operations happen on this thread — no cross-thread GPU context issues

- [ ] **Step 1: Update function signature**

```rust
pub fn run_video_send_pipeline(
    frame_rx: std::sync::mpsc::Receiver<RawFrame>,
    #[cfg(target_os = "linux")]
    gpu_frame_rx: Option<std::sync::mpsc::Receiver<super::capture::DmaBufFrame>>,
    control_rx: std::sync::mpsc::Receiver<VideoPipelineControl>,
    event_tx: std::sync::mpsc::Sender<VideoPipelineEvent>,
    socket: Arc<UdpSocket>,
    sender_id: String,
    config: EncoderConfig,
    target_fps: u32,
) {
```

- [ ] **Step 2: Initialize GPU context and encoder**

Replace the encoder initialization section (lines 112-121) with:

```rust
    // Try GPU zero-copy path (Linux only). All GPU init happens on this thread.
    #[cfg(target_os = "linux")]
    let mut gpu_ctx = super::gpu_interop::GpuContext::new();

    #[cfg(target_os = "linux")]
    let using_gpu = gpu_ctx.is_some() && gpu_frame_rx.is_some();

    // Create encoder — use VAAPI-specific constructor when VAAPI backend is active
    #[cfg(target_os = "linux")]
    let mut encoder = if let Some(ref mut gpu) = gpu_ctx {
        match gpu.backend_type() {
            super::gpu_interop::GpuBackendType::Vaapi => {
                // Initialize VAAPI frames context first
                if let Err(e) = gpu.init_vaapi_frames(config.width, config.height) {
                    eprintln!("[video-send] VAAPI frames init failed: {}", e);
                    // Fall through to standard encoder
                    match H264Encoder::new(&config) {
                        Ok(e) => e,
                        Err(e) => {
                            let _ = event_tx.send(VideoPipelineEvent::Error(e));
                            return;
                        }
                    }
                } else {
                    match H264Encoder::new_vaapi(
                        &config,
                        gpu.vaapi_device_ref(),
                        gpu.vaapi_frames_ref(),
                    ) {
                        Ok(e) => {
                            eprintln!("[video-send] VA-API zero-copy encoding enabled");
                            e
                        }
                        Err(e) => {
                            eprintln!("[video-send] VAAPI encoder failed ({}), falling back", e);
                            match H264Encoder::new(&config) {
                                Ok(e) => e,
                                Err(e) => {
                                    let _ = event_tx.send(VideoPipelineEvent::Error(e));
                                    return;
                                }
                            }
                        }
                    }
                }
            }
            super::gpu_interop::GpuBackendType::Cuda => {
                // Standard encoder (h264_nvenc) + CUDA hw init
                match H264Encoder::new(&config) {
                    Ok(mut e) => {
                        match e.init_cuda_hw() {
                            Ok(()) => eprintln!("[video-send] CUDA zero-copy encoding enabled"),
                            Err(err) => eprintln!("[video-send] CUDA hw init failed ({}), CPU fallback", err),
                        }
                        e
                    }
                    Err(e) => {
                        let _ = event_tx.send(VideoPipelineEvent::Error(e));
                        return;
                    }
                }
            }
        }
    } else {
        match H264Encoder::new(&config) {
            Ok(e) => e,
            Err(e) => {
                let _ = event_tx.send(VideoPipelineEvent::Error(e));
                return;
            }
        }
    };

    // Windows: standard encoder, no GPU context
    #[cfg(not(target_os = "linux"))]
    let mut encoder = match H264Encoder::new(&config) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VideoPipelineEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(VideoPipelineEvent::Started);
    eprintln!("[video-send] Pipeline started, target {}fps", target_fps);
```

- [ ] **Step 3: Update the main loop to try GPU frames first**

Replace the frame receive + encode section in the main loop:

```rust
        // ── Receive frame: try GPU channel first, then CPU channel ──
        let mut got_gpu_frame = false;
        #[cfg(target_os = "linux")]
        let mut gpu_frame_opt: Option<super::capture::DmaBufFrame> = None;

        #[cfg(target_os = "linux")]
        if let Some(ref gpu_rx) = gpu_frame_rx {
            match gpu_rx.try_recv() {
                Ok(gf) => {
                    gpu_frame_opt = Some(gf);
                    got_gpu_frame = true;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if !got_gpu_frame {
            // CPU frame path (RawFrame)
            match frame_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(f) => {
                    last_frame = Some(f);
                    have_new_frame = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if last_frame_time.elapsed() >= repeat_interval && last_frame.is_some() {
                        have_new_frame = false;
                    } else {
                        continue;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if have_new_frame || got_gpu_frame {
            last_frame_time = Instant::now();
        }

        // ── Encode ──
        let encode_result;

        #[cfg(target_os = "linux")]
        if let Some(ref dmabuf) = gpu_frame_opt {
            // GPU zero-copy encode
            encode_result = if let Some(ref mut gpu) = gpu_ctx {
                use std::os::fd::AsRawFd;
                match gpu.backend_type() {
                    super::gpu_interop::GpuBackendType::Cuda => {
                        if encoder.has_cuda_hw() {
                            match gpu.import_dmabuf_cuda(
                                dmabuf.fd.as_raw_fd(),
                                dmabuf.width, dmabuf.height,
                                dmabuf.stride, dmabuf.drm_format, dmabuf.modifier,
                            ) {
                                Some(dev_ptr) => {
                                    gpu.push_cuda_ctx();
                                    let r = encoder.encode_cuda_frame(dev_ptr, dmabuf.width, dmabuf.height);
                                    gpu.pop_cuda_ctx();
                                    r
                                }
                                None => {
                                    eprintln!("[video-send] CUDA import failed, skipping frame");
                                    continue;
                                }
                            }
                        } else {
                            continue;
                        }
                    }
                    super::gpu_interop::GpuBackendType::Vaapi => {
                        match gpu.map_dmabuf_vaapi(
                            dmabuf.fd.as_raw_fd(),
                            dmabuf.width, dmabuf.height,
                            dmabuf.stride, dmabuf.drm_format, dmabuf.modifier,
                        ) {
                            Some(mut vaapi_frame) => {
                                encoder.encode_vaapi_frame(&mut vaapi_frame)
                            }
                            None => {
                                eprintln!("[video-send] VAAPI map failed, skipping frame");
                                continue;
                            }
                        }
                    }
                }
            } else {
                continue;
            };
        } else

        // CPU encode path (RawFrame) — also used on Windows
        {
            let frame = last_frame.as_ref().unwrap();

            // Generate thumbnail periodically (only for CPU frames — GPU frames aren't CPU-accessible)
            let now_for_thumb = Instant::now();
            if now_for_thumb.duration_since(last_thumbnail_time) >= thumbnail_interval {
                last_thumbnail_time = now_for_thumb;
                if let Some(jpeg) = frame_to_jpeg_thumbnail(frame) {
                    eprintln!("[video-send] Thumbnail generated: {} bytes", jpeg.len());
                    let _ = event_tx.send(VideoPipelineEvent::ThumbnailReady(jpeg));
                }
            }

            encode_result = match frame.pixel_format {
                PixelFormat::NV12 => {
                    encoder.encode_nv12_frame(&frame.data, frame.width, frame.height)
                }
                PixelFormat::BGRA | PixelFormat::RGBA => {
                    let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                    encoder.encode_bgra_frame(
                        &frame.data, frame.width, frame.height, frame.stride, is_bgra,
                    )
                }
            };
        }
```

The rest of the function (packetize + send, flush) stays exactly the same.

- [ ] **Step 4: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`

The `#[cfg]`-gated blocks must compile correctly. The key pattern is:

```rust
#[cfg(target_os = "linux")]
if condition { ... } else
{ /* CPU path — runs on all platforms */ }
```

If this `else` chaining doesn't work syntactically with `#[cfg]`, restructure as:

```rust
#[cfg(target_os = "linux")]
let encode_result = if got_gpu_frame { /* GPU path */ } else { /* CPU path */ };
#[cfg(not(target_os = "linux"))]
let encode_result = { /* CPU path */ };
```

Fix any compilation issues.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media/video_pipeline.rs
git commit -m "feat(pipeline): route DMA-BUF frames to GPU encode, RawFrame to CPU encode"
```

---

### Task 7: Update VideoEngine and mod.rs

**Files:**
- Modify: `src-tauri/src/media/mod.rs`

Update `VideoEngine::start()` to accept the optional `gpu_receiver` and pass it to the pipeline thread.

- [ ] **Step 1: Update VideoEngine::start signature**

Change the method signature (around line 416):

```rust
    pub fn start(
        frame_rx: std::sync::mpsc::Receiver<capture::RawFrame>,
        #[cfg(target_os = "linux")]
        gpu_frame_rx: Option<std::sync::mpsc::Receiver<capture::DmaBufFrame>>,
        socket: Arc<UdpSocket>,
        sender_id: String,
        config: encoder::EncoderConfig,
        target_fps: u32,
        app: AppHandle,
        thumbnail_write_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
        thumbnail_channel_id: Option<String>,
    ) -> Self {
```

- [ ] **Step 2: Pass gpu_frame_rx to the pipeline thread**

Update the thread spawn (around line 428):

```rust
        let pipeline_thread = thread::Builder::new()
            .name("decibell-video".to_string())
            .spawn(move || {
                video_pipeline::run_video_send_pipeline(
                    frame_rx,
                    #[cfg(target_os = "linux")]
                    gpu_frame_rx,
                    control_rx,
                    event_tx,
                    socket,
                    sender_id,
                    config,
                    target_fps,
                );
            })
            .expect("spawn video pipeline thread");
```

- [ ] **Step 3: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -10`
Expected: Compiles. There will be an error in `streaming.rs` because the call site hasn't been updated yet.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/media/mod.rs
git commit -m "feat(mod): pass gpu_frame_rx to video pipeline thread"
```

---

### Task 8: Update streaming.rs Call Site

**Files:**
- Modify: `src-tauri/src/commands/streaming.rs`

Pass the `gpu_receiver` from `CaptureOutput` to `VideoEngine::start`. This is a one-line change on Linux, zero changes on Windows.

- [ ] **Step 1: Update VideoEngine::start call**

Change the call (around line 104):

```rust
    let video_engine = VideoEngine::start(
        capture_output.receiver,
        #[cfg(target_os = "linux")]
        capture_output.gpu_receiver,
        socket.clone(),
        sender_id.clone(),
        encoder_config,
        fps,
        app.clone(),
        thumbnail_write_tx,
        thumbnail_channel_id,
    );
```

On Windows, `capture_output` has no `gpu_receiver` field, so the `#[cfg]` line is entirely absent.

- [ ] **Step 2: Verify it compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -5`
Expected: Clean compilation on Linux.

- [ ] **Step 3: Verify Windows compilation is unaffected**

Check that all `#[cfg(target_os = "linux")]` guards are correct:

```bash
grep -n 'gpu_receiver\|gpu_frame_rx\|DmaBufFrame\|gpu_interop\|cuda_hw\|vaapi' \
    src-tauri/src/commands/streaming.rs \
    src-tauri/src/media/mod.rs \
    src-tauri/src/media/video_pipeline.rs \
    src-tauri/src/media/encoder.rs \
    src-tauri/src/media/capture.rs
```

Every line must be inside a `#[cfg(target_os = "linux")]` block. Verify none leak into Windows-compiled code.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/streaming.rs
git commit -m "feat(streaming): pass gpu_receiver to VideoEngine for DMA-BUF zero-copy"
```

---

### Task 9: Fix Video::wrap and FFmpeg API Compatibility

**Files:**
- Modify: `src-tauri/src/media/gpu_interop.rs` (if needed)
- Modify: `src-tauri/src/media/encoder.rs` (if needed)

The plan uses `ffmpeg_next::frame::Video::wrap(raw_ptr)` which may not exist in ffmpeg-next 8.x. Check and fix.

- [ ] **Step 1: Check if Video::wrap exists**

```bash
grep -rn "fn wrap\|pub fn wrap" ~/.cargo/registry/src/*/ffmpeg-next-8*/src/util/frame/ 2>/dev/null
```

If it doesn't exist, find the correct way to create a `Video` from a raw `*mut AVFrame`:

```bash
grep -rn "from_raw\|unsafe.*AVFrame\|as_mut_ptr\|from_ptr" ~/.cargo/registry/src/*/ffmpeg-next-8*/src/util/frame/ 2>/dev/null | head -20
```

- [ ] **Step 2: Fix all Video::wrap calls**

If `Video::wrap()` doesn't exist, the typical alternatives are:
- `ffmpeg_next::frame::Video::from(Frame::wrap(ptr))` — if `Frame::wrap` exists
- Manually construct via `unsafe` transmute from raw pointer
- Use `Video::empty()` and then set fields via `as_mut_ptr()`

Fix all occurrences in `gpu_interop.rs` and `encoder.rs`.

- [ ] **Step 3: Full build**

```bash
cd src-tauri && cargo build 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/media/
git commit -m "fix(encoder): fix FFmpeg frame API compatibility for ffmpeg-next 8.x"
```

---

### Task 10: Integration Test

**Files:** None (runtime verification)

- [ ] **Step 1: Build debug binary**

```bash
cd tauri-client && cargo tauri build --debug 2>&1 | tail -20
```

- [ ] **Step 2: Run and verify GPU backend detection**

Launch the app. Check terminal for:

```
[gpu] Probing GPU backends...
[gpu] CUDA backend ready (device=0)           ← NVIDIA
      OR
[gpu] VA-API backend ready (DRM: /dev/dri/renderD128)  ← AMD/Intel
```

- [ ] **Step 3: Start streaming and verify DMA-BUF path**

Join a voice channel, start streaming. Look for:

```
[capture] Frame 1 (... buf_type=DataType::DmaBuf ...)
[capture] DMA-BUF frame: fd=XX, 1920x1080, stride=7680, drm_fmt=0x34325241
[video-send] CUDA zero-copy encoding enabled   ← NVIDIA
[video-send] VA-API zero-copy encoding enabled  ← AMD/Intel
```

If you see `buf_type=DataType::MemPtr` instead, PipeWire is using SHM. The CPU fallback should still work — verify frames encode via the existing path.

- [ ] **Step 4: Measure FPS impact**

Open Dota 2 (or another game), note FPS without streaming, then start streaming:
- **Before (SHM):** ~80-90 FPS drop
- **Expected (DMA-BUF):** <10 FPS drop (similar to Vesktop)

- [ ] **Step 5: Debug common issues**

| Symptom | Cause | Fix |
|---------|-------|-----|
| `buf_type=MemPtr` not DmaBuf | Compositor doesn't offer DMA-BUF without MAP_BUFFERS | Try adding SPA param to prefer DMA-BUF, or check KWin settings |
| `eglCreateImage failed` | Modifier mismatch | Set modifier to DRM_FORMAT_MOD_INVALID |
| `cuGraphicsGLRegisterImage failed` | EGL/CUDA on different GPUs | Check single-GPU setup |
| `av_hwframe_map failed: -38` (ENOSYS) | FFmpeg built without DRM→VAAPI mapping | Rebuild FFmpeg with --enable-libdrm |
| Stream works but green/corrupted | Wrong DRM fourcc mapping | Log the actual SPA format and fourcc, verify against DRM docs |
| App freezes on stream start | GPU init on wrong thread | Verify all GPU init is in video pipeline thread, not Tokio runtime |

---

### Task 11: Performance Optimization — Cache EGL/CUDA Resources

**Files:**
- Modify: `src-tauri/src/media/gpu_interop.rs`

PipeWire reuses a small pool of DMA-BUF buffers (typically 4-8). Caching EGLImage + CUDA registration per fd avoids re-creating them every frame (~1-2ms overhead → ~0.1ms).

- [ ] **Step 1: Add import cache to CudaBackend**

```rust
use std::collections::HashMap;

struct CachedImport {
    egl_image: khronos_egl::Image,
    cu_resource: CUgraphicsResource,
}

// Add to CudaBackend:
    /// Cache of EGL images + CUDA registrations, keyed by DMA-BUF fd.
    /// PipeWire reuses fds from a small pool, so this avoids re-importing every frame.
    import_cache: HashMap<i32, CachedImport>,
```

- [ ] **Step 2: Refactor import_dmabuf to use cache**

Split the current `import_dmabuf` into cached and uncached paths:

```rust
    fn import_dmabuf(&mut self, fd: i32, width: u32, height: u32, stride: u32, drm_format: u32, modifier: u64) -> Option<CUdeviceptr> {
        self.egl.make_current(self.egl_display, None, None, Some(self.egl_context)).ok()?;
        unsafe { (self.cuda.cu_ctx_push)(self.cu_ctx) };

        // Check cache
        let (cu_resource, need_cleanup) = if let Some(cached) = self.import_cache.get(&fd) {
            (cached.cu_resource, false)
        } else {
            // Create EGLImage + register with CUDA (same code as before)
            let egl_image = /* ... create EGLImage ... */;
            let cu_resource = /* ... register GL texture ... */;
            self.import_cache.insert(fd, CachedImport { egl_image, cu_resource });
            (cu_resource, false)
        };

        // Map → CUarray → cuMemcpy2D (same as before)
        /* ... */

        // Only unmap (not unregister) — registration stays in cache
        unsafe {
            (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource_copy, std::ptr::null_mut());
        }
        self.pop_ctx();
        Some(dev_ptr)
    }
```

- [ ] **Step 3: Handle stale cache entries**

When a DMA-BUF fd is reused by PipeWire for a different buffer, `cuGraphicsMapResources` may fail. Detect this and evict:

```rust
        let map_rc = unsafe {
            (self.cuda.cu_graphics_map_resources)(1, &mut cu_resource_copy, std::ptr::null_mut())
        };
        if map_rc != CUDA_SUCCESS {
            // Stale cache entry — evict and re-import
            if let Some(old) = self.import_cache.remove(&fd) {
                unsafe { (self.cuda.cu_graphics_unregister_resource)(old.cu_resource) };
                let _ = self.egl.destroy_image(self.egl_display, old.egl_image);
            }
            // Retry with fresh import
            return self.import_dmabuf(fd, width, height, stride, drm_format, modifier);
        }
```

- [ ] **Step 4: Clean up cache on drop**

```rust
impl Drop for CudaBackend {
    fn drop(&mut self) {
        for (_, cached) in self.import_cache.drain() {
            unsafe { (self.cuda.cu_graphics_unregister_resource)(cached.cu_resource) };
            let _ = self.egl.destroy_image(self.egl_display, cached.egl_image);
        }
        // ... rest of existing cleanup
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media/gpu_interop.rs
git commit -m "perf(gpu): cache EGL/CUDA resources per DMA-BUF fd across frames"
```
