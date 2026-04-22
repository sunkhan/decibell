//! EGL/GL/CUDA and VA-API interop for DMA-BUF zero-copy capture.
//!
//! All unsafe GPU code is isolated in this module. Two backends:
//!
//! - **NVIDIA (Cuda):** DMA-BUF -> EGLImage -> GL texture -> CUDA -> CUdeviceptr -> NVENC
//! - **AMD/Intel (Vaapi):** DMA-BUF -> AVDRMFrameDescriptor -> av_hwframe_map -> h264_vaapi
//!
//! Public API:
//! - `GpuContext::new()` -> detects GPU, returns appropriate backend
//! - `GpuContext::import_dmabuf_cuda()` -> NVIDIA: DMA-BUF -> CUdeviceptr
//! - `GpuContext::fill_drm_frame()` -> AMD/Intel: fills AVDRMFrameDescriptor for VA-API
//! - `GpuContext::backend()` -> which GPU backend is active

use std::collections::HashMap;
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
type CUexternalMemory = *mut std::ffi::c_void;

const CUDA_SUCCESS: CUresult = 0;
const CU_GRAPHICS_REGISTER_FLAGS_NONE: u32 = 0;
const CU_GRAPHICS_REGISTER_FLAGS_READ_ONLY: u32 = 1;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const CU_MEMORYTYPE_DEVICE: u32 = 2;
const CU_MEMORYTYPE_ARRAY: u32 = 3;
const CU_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD: u32 = 1;

// ── CUDA external memory descriptors (match C struct layout exactly) ──
#[repr(C)]
#[derive(Copy, Clone)]
struct CudaExternalMemoryWin32Handle {
    handle: *mut std::ffi::c_void,
    name: *const std::ffi::c_void,
}

#[repr(C)]
union CudaExternalMemoryHandle {
    fd: i32,
    _win32: CudaExternalMemoryWin32Handle,
    _nvsci: *const std::ffi::c_void,
}

#[repr(C)]
struct CudaExternalMemoryHandleDesc {
    type_: u32,
    handle: CudaExternalMemoryHandle,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}

#[repr(C)]
struct CudaExternalMemoryBufferDesc {
    offset: u64,
    size: u64,
    flags: u32,
    reserved: [u32; 16],
}

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

    fn device_to_device(
        src_device: CUdeviceptr,
        src_pitch: usize,
        dst_device: CUdeviceptr,
        dst_pitch: usize,
        width_bytes: usize,
        height: usize,
    ) -> Self {
        CudaMemcpy2D {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CU_MEMORYTYPE_DEVICE,
            src_host: std::ptr::null(),
            src_device,
            src_array: std::ptr::null_mut(),
            _src_reserved: 0,
            src_pitch,
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
    cu_import_external_memory: unsafe extern "C" fn(
        *mut CUexternalMemory, *const CudaExternalMemoryHandleDesc,
    ) -> CUresult,
    cu_external_memory_get_mapped_buffer: unsafe extern "C" fn(
        *mut CUdeviceptr, CUexternalMemory, *const CudaExternalMemoryBufferDesc,
    ) -> CUresult,
    cu_destroy_external_memory: unsafe extern "C" fn(CUexternalMemory) -> CUresult,
    cu_memcpy_dtoh: unsafe extern "C" fn(*mut std::ffi::c_void, CUdeviceptr, usize) -> CUresult,
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
                cu_import_external_memory: *lib.get(b"cuImportExternalMemory\0").ok()?,
                cu_external_memory_get_mapped_buffer: *lib.get(b"cuExternalMemoryGetMappedBuffer\0").ok()?,
                cu_destroy_external_memory: *lib.get(b"cuDestroyExternalMemory\0").ok()?,
                cu_memcpy_dtoh: *lib.get(b"cuMemcpyDtoH_v2\0").ok()?,
                _lib: lib,
            })
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// EGL constants for DMA-BUF import
// ────────────────────────────────────────────────────────────────────────────

const EGL_LINUX_DMA_BUF_EXT: khronos_egl::Enum = 0x3270;
const EGL_WIDTH: khronos_egl::Attrib = 0x3057;
const EGL_HEIGHT: khronos_egl::Attrib = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: khronos_egl::Attrib = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: khronos_egl::Attrib = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: khronos_egl::Attrib = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: khronos_egl::Attrib = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: khronos_egl::Attrib = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: khronos_egl::Attrib = 0x3444;

pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ffffffffffffff;

/// glEGLImageTargetTexture2DOES function pointer type
type GlEglImageTargetTexture2DOesFn = unsafe extern "C" fn(u32, *mut std::ffi::c_void);
/// glEGLImageTargetTexStorageEXT function pointer type (GL_EXT_EGL_image_storage).
/// This is the modern, immutable-storage variant — NVIDIA's desktop-GL path
/// accepts it reliably where the legacy `...Texture2DOES` throws
/// GL_INVALID_OPERATION on tiled-modifier DMA-BUFs.
type GlEglImageTargetTexStorageExtFn = unsafe extern "C" fn(u32, *mut std::ffi::c_void, *const i32);

// ────────────────────────────────────────────────────────────────────────────
// GPU Backend detection
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackendType {
    /// NVIDIA: EGL -> GL -> CUDA -> NVENC (h264_nvenc)
    Cuda,
    /// AMD/Intel: DRM PRIME -> VA-API surface -> h264_vaapi
    Vaapi,
}

// ────────────────────────────────────────────────────────────────────────────
// NVIDIA CUDA backend
// ────────────────────────────────────────────────────────────────────────────

#[allow(dead_code)]
struct CachedImport {
    egl_image: khronos_egl::Image,
    gl_tex: u32,
    cu_resource: CUgraphicsResource,
}

struct CudaBackend {
    cuda: CudaApi,
    cu_ctx: CUcontext,
    #[allow(dead_code)]
    egl: khronos_egl::DynamicInstance<khronos_egl::EGL1_5>,
    #[allow(dead_code)]
    egl_display: khronos_egl::Display,
    #[allow(dead_code)]
    egl_context: khronos_egl::Context,
    #[allow(dead_code)]
    gl_image_target_fn: GlEglImageTargetTexture2DOesFn,
    gl_image_target_storage_fn: Option<GlEglImageTargetTexStorageExtFn>,
    #[allow(dead_code)]
    gl_tex: u32,
    /// Reusable CUDA linear buffer: (ptr, width, height)
    dev_buf: Option<(CUdeviceptr, u32, u32)>,
    /// Legacy EGL-backed import cache (kept only for Drop cleanup on systems
    /// where the old path was exercised). New imports go through
    /// `ext_mem_cache` below — cuImportExternalMemory bypasses EGL entirely
    /// and works on NVIDIA + Wayland where EGL can't find the nvidia driver.
    #[allow(dead_code)]
    import_cache: HashMap<i32, CachedImport>,
    /// Cache of CUDA external-memory handles + their mapped linear device
    /// pointer, keyed by DMA-BUF fd. PipeWire recycles fds from a small pool,
    /// so this avoids re-importing every frame.
    ext_mem_cache: HashMap<i32, CachedExternalMem>,
}

struct CachedExternalMem {
    ext_mem: CUexternalMemory,
    src_dev_ptr: CUdeviceptr,
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

        // Get an EGL display explicitly backed by the NVIDIA device. Using
        // eglGetDisplay(EGL_DEFAULT_DISPLAY) delegates to libglvnd's default
        // dispatch, which on systems with mesa installed often picks mesa —
        // mesa's libEGL can't drive the NVIDIA proprietary driver and fails
        // with "driver (null)" / dri2 screen errors, leaving CUDA unable to
        // register the GL texture. Enumerating EGL devices and picking the
        // one advertising EGL_NV_device_cuda forces libglvnd to dispatch to
        // NVIDIA's libEGL, giving us a usable GL context for CUDA interop.
        const EGL_PLATFORM_DEVICE_EXT: khronos_egl::Enum = 0x313F;
        const EGL_EXTENSIONS_QUERY: i32 = 0x3055;

        type EglQueryDevicesExtFn = unsafe extern "C" fn(
            i32, *mut *mut std::ffi::c_void, *mut i32,
        ) -> u32;
        type EglQueryDeviceStringExtFn = unsafe extern "C" fn(
            *mut std::ffi::c_void, i32,
        ) -> *const std::os::raw::c_char;

        let query_devices: EglQueryDevicesExtFn = match egl.get_proc_address("eglQueryDevicesEXT") {
            Some(p) => unsafe { std::mem::transmute(p) },
            None => {
                eprintln!("[gpu] eglQueryDevicesEXT not available");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };
        let query_device_string: EglQueryDeviceStringExtFn = match egl.get_proc_address("eglQueryDeviceStringEXT") {
            Some(p) => unsafe { std::mem::transmute(p) },
            None => {
                eprintln!("[gpu] eglQueryDeviceStringEXT not available");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };

        let mut num_devices: i32 = 0;
        unsafe { query_devices(0, std::ptr::null_mut(), &mut num_devices) };
        if num_devices <= 0 {
            eprintln!("[gpu] eglQueryDevicesEXT reported 0 devices");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }
        let mut devices: Vec<*mut std::ffi::c_void> =
            vec![std::ptr::null_mut(); num_devices as usize];
        unsafe { query_devices(num_devices, devices.as_mut_ptr(), &mut num_devices) };

        let mut nvidia_device: *mut std::ffi::c_void = std::ptr::null_mut();
        for (i, &dev) in devices.iter().enumerate() {
            if dev.is_null() {
                continue;
            }
            let ext_ptr = unsafe { query_device_string(dev, EGL_EXTENSIONS_QUERY) };
            let ext_str = if ext_ptr.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(ext_ptr) }
                    .to_string_lossy()
                    .into_owned()
            };
            eprintln!("[gpu] EGL device[{}] extensions: {}", i, ext_str);
            if ext_str.contains("EGL_NV_device_cuda") {
                nvidia_device = dev;
                eprintln!("[gpu] Selected EGL device[{}] (NVIDIA CUDA-capable)", i);
                break;
            }
        }
        if nvidia_device.is_null() {
            eprintln!("[gpu] No NVIDIA CUDA-capable EGL device found");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        let egl_display = match unsafe {
            egl.get_platform_display(
                EGL_PLATFORM_DEVICE_EXT,
                nvidia_device as khronos_egl::NativeDisplayType,
                &[khronos_egl::ATTRIB_NONE],
            )
        } {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[gpu] eglGetPlatformDisplay(EGL_PLATFORM_DEVICE_EXT) failed: {:?}", e);
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };
        if egl.initialize(egl_display).is_err() {
            eprintln!("[gpu] eglInitialize on NVIDIA device failed");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        // Use desktop OpenGL, not OpenGL ES. On NVIDIA's
        // EGL_PLATFORM_DEVICE_EXT path, a GLES context + glEGLImageTargetTexture2DOES
        // errors with GL_INVALID_OPERATION when attaching DMA-BUFs with
        // MOD_INVALID / tiled modifiers — the GLES driver path on NVIDIA
        // doesn't support that combination. Desktop GL does.
        if egl.bind_api(khronos_egl::OPENGL_API).is_err() {
            eprintln!("[gpu] eglBindAPI(OPENGL) failed");
            unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
            return None;
        }

        let config_attribs: [khronos_egl::Int; 5] = [
            khronos_egl::RENDERABLE_TYPE, khronos_egl::OPENGL_BIT,
            khronos_egl::SURFACE_TYPE, 0,
            khronos_egl::NONE,
        ];
        let config = match egl.choose_first_config(egl_display, &config_attribs) {
            Ok(Some(c)) => c,
            _ => {
                eprintln!("[gpu] eglChooseConfig(OPENGL_BIT) failed");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };

        // Desktop GL: no CONTEXT_CLIENT_VERSION. CONTEXT_MAJOR_VERSION can be
        // used to request a specific version but default is fine for our use.
        let egl_context = match egl.create_context(egl_display, config, None, &[khronos_egl::NONE]) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[gpu] eglCreateContext(OPENGL) failed: {}", e);
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
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |p| p as *const std::ffi::c_void)
        });

        // Log GL version and extensions so we can see what's actually available
        // in this context (NVIDIA's EGL_PLATFORM_DEVICE + desktop GL context).
        unsafe {
            let version_ptr = gl::GetString(gl::VERSION) as *const std::os::raw::c_char;
            let renderer_ptr = gl::GetString(gl::RENDERER) as *const std::os::raw::c_char;
            let vendor_ptr = gl::GetString(gl::VENDOR) as *const std::os::raw::c_char;
            let version = if version_ptr.is_null() { "?".into() } else { std::ffi::CStr::from_ptr(version_ptr).to_string_lossy() };
            let renderer = if renderer_ptr.is_null() { "?".into() } else { std::ffi::CStr::from_ptr(renderer_ptr).to_string_lossy() };
            let vendor = if vendor_ptr.is_null() { "?".into() } else { std::ffi::CStr::from_ptr(vendor_ptr).to_string_lossy() };
            eprintln!("[gpu] GL_VERSION={} | GL_VENDOR={} | GL_RENDERER={}", version, vendor, renderer);

            // Use glGetStringi for extensions (core profile doesn't expose GL_EXTENSIONS as a single string).
            let mut num_ext: i32 = 0;
            gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut num_ext);
            let want = ["GL_OES_EGL_image", "GL_EXT_EGL_image_storage", "GL_ARB_texture_storage"];
            let mut found: Vec<&str> = Vec::new();
            for i in 0..num_ext {
                let ext_ptr = gl::GetStringi(gl::EXTENSIONS, i as u32) as *const std::os::raw::c_char;
                if ext_ptr.is_null() { continue; }
                let ext_str = std::ffi::CStr::from_ptr(ext_ptr).to_string_lossy();
                for w in &want {
                    if ext_str.as_ref() == *w && !found.contains(w) { found.push(*w); }
                }
            }
            eprintln!("[gpu] Relevant GL extensions present: {:?}", found);
        }

        // Load glEGLImageTargetTexture2DOES extension (legacy path).
        let gl_image_target_fn: GlEglImageTargetTexture2DOesFn = {
            match egl.get_proc_address("glEGLImageTargetTexture2DOES") {
                Some(p) => unsafe { std::mem::transmute(p) },
                None => {
                    eprintln!("[gpu] glEGLImageTargetTexture2DOES not available");
                    let _ = egl.destroy_context(egl_display, egl_context);
                    unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                    return None;
                }
            }
        };

        // Load glEGLImageTargetTexStorageEXT — the immutable-storage variant
        // from GL_EXT_EGL_image_storage. Preferred on NVIDIA desktop-GL where
        // the legacy OES entry point throws GL_INVALID_OPERATION for
        // tiled-modifier DMA-BUFs.
        let gl_image_target_storage_fn: Option<GlEglImageTargetTexStorageExtFn> =
            egl.get_proc_address("glEGLImageTargetTexStorageEXT")
                .map(|p| unsafe { std::mem::transmute(p) });

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
            gl_image_target_storage_fn,
            gl_tex,
            dev_buf: None,
            import_cache: HashMap::new(),
            ext_mem_cache: HashMap::new(),
        })
    }

    /// Import DMA-BUF into CUDA device memory via the EGL → GL → CUDA path.
    /// Returns CUdeviceptr pointing to BGRA pixel data in GPU-linear memory.
    ///
    /// This path works reliably on NVIDIA because our EGL display is now
    /// explicitly bound to the NVIDIA platform device (see CudaBackend::new).
    /// `cuImportExternalMemory` (the zero-EGL shortcut) refuses mesa/GBM-
    /// allocated DMA-BUF fds on NVIDIA's proprietary driver with
    /// CUDA_ERROR_UNKNOWN, so we can't use it here.
    ///
    /// EGLImage + CUDA registration are cached per fd since PipeWire reuses
    /// a small pool of DMA-BUF buffers. Only the map/copy/unmap runs per frame.
    fn import_dmabuf(
        &mut self,
        fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
        modifier: u64,
    ) -> Option<CUdeviceptr> {
        self.egl.make_current(self.egl_display, None, None, Some(self.egl_context)).ok()?;
        unsafe { (self.cuda.cu_ctx_push)(self.cu_ctx) };

        // Check cache or create new EGLImage + CUDA registration
        let cu_resource = if let Some(cached) = self.import_cache.get(&fd) {
            cached.cu_resource
        } else {
            // Create EGLImage from DMA-BUF
            let mut attribs: Vec<khronos_egl::Attrib> = vec![
                EGL_WIDTH, width as khronos_egl::Attrib,
                EGL_HEIGHT, height as khronos_egl::Attrib,
                EGL_LINUX_DRM_FOURCC_EXT, drm_format as khronos_egl::Attrib,
                EGL_DMA_BUF_PLANE0_FD_EXT, fd as khronos_egl::Attrib,
                EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
                EGL_DMA_BUF_PLANE0_PITCH_EXT, stride as khronos_egl::Attrib,
            ];
            if modifier != DRM_FORMAT_MOD_INVALID {
                attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT);
                attribs.push((modifier & 0xFFFFFFFF) as khronos_egl::Attrib);
                attribs.push(EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT);
                attribs.push((modifier >> 32) as khronos_egl::Attrib);
            }
            attribs.push(khronos_egl::ATTRIB_NONE);

            let no_ctx = unsafe { khronos_egl::Context::from_ptr(khronos_egl::NO_CONTEXT) };
            let no_buf = unsafe { khronos_egl::ClientBuffer::from_ptr(std::ptr::null_mut()) };
            let egl_image = match self.egl.create_image(
                self.egl_display, no_ctx, EGL_LINUX_DMA_BUF_EXT, no_buf, &attribs,
            ) {
                Ok(img) => img,
                Err(e) => {
                    eprintln!("[gpu] eglCreateImage failed: {}", e);
                    self.pop_ctx();
                    return None;
                }
            };

            // Create a dedicated GL texture for this fd. Immutable-storage
            // bindings via glEGLImageTargetTexStorageEXT can only be applied
            // to a fresh texture — reusing one across different EGLImages
            // yields GL_INVALID_OPERATION on NVIDIA desktop-GL.
            let mut tex: u32 = 0;
            unsafe { gl::GenTextures(1, &mut tex) };
            if tex == 0 {
                eprintln!("[gpu] glGenTextures failed inside import_dmabuf");
                let _ = self.egl.destroy_image(self.egl_display, egl_image);
                self.pop_ctx();
                return None;
            }

            let (gl_err, internal_fmt, used_storage): (u32, i32, bool) = unsafe {
                while gl::GetError() != gl::NO_ERROR {} // drain
                gl::BindTexture(gl::TEXTURE_2D, tex);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

                // Prefer the modern immutable-storage entry point
                // (glEGLImageTargetTexStorageEXT). It succeeds on NVIDIA's
                // desktop-GL path where the legacy glEGLImageTargetTexture2DOES
                // fails for tiled/MOD_INVALID DMA-BUFs.
                let used = if let Some(target_storage) = self.gl_image_target_storage_fn {
                    target_storage(
                        gl::TEXTURE_2D,
                        egl_image.as_ptr() as *mut std::ffi::c_void,
                        std::ptr::null(),
                    );
                    true
                } else {
                    (self.gl_image_target_fn)(gl::TEXTURE_2D, egl_image.as_ptr() as *mut std::ffi::c_void);
                    false
                };
                let err = gl::GetError();
                let mut ifmt: i32 = 0;
                gl::GetTexLevelParameteriv(gl::TEXTURE_2D, 0, gl::TEXTURE_INTERNAL_FORMAT, &mut ifmt);
                gl::BindTexture(gl::TEXTURE_2D, 0);
                (err, ifmt, used)
            };

            if gl_err != 0 && used_storage {
                // Storage path failed — delete the tainted texture, create
                // a fresh one, and retry with the legacy target function.
                eprintln!(
                    "[gpu] glEGLImageTargetTexStorageEXT failed (gl_err=0x{:x}), retrying with glEGLImageTargetTexture2DOES",
                    gl_err
                );
                unsafe {
                    gl::DeleteTextures(1, &tex);
                    gl::GenTextures(1, &mut tex);
                    gl::BindTexture(gl::TEXTURE_2D, tex);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
                    while gl::GetError() != gl::NO_ERROR {}
                    (self.gl_image_target_fn)(gl::TEXTURE_2D, egl_image.as_ptr() as *mut std::ffi::c_void);
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                }
            }

            let mut cu_resource: CUgraphicsResource = std::ptr::null_mut();
            let rc = unsafe {
                (self.cuda.cu_graphics_gl_register_image)(
                    &mut cu_resource, tex, GL_TEXTURE_2D, CU_GRAPHICS_REGISTER_FLAGS_NONE,
                )
            };
            if rc != CUDA_SUCCESS {
                eprintln!(
                    "[gpu] cuGraphicsGLRegisterImage failed: {} (gl_err=0x{:x}, tex_internal_format=0x{:x}, used_storage_ext={}, drm_fourcc=0x{:08x}, modifier=0x{:x})",
                    rc, gl_err, internal_fmt as u32, used_storage, drm_format, modifier
                );
                unsafe { gl::DeleteTextures(1, &tex) };
                let _ = self.egl.destroy_image(self.egl_display, egl_image);
                self.pop_ctx();
                return None;
            }

            eprintln!(
                "[gpu] Imported DMA-BUF fd={}: tex={}, internal_format=0x{:x}, via_storage_ext={}",
                fd, tex, internal_fmt as u32, used_storage
            );
            self.import_cache.insert(fd, CachedImport { egl_image, gl_tex: tex, cu_resource });
            cu_resource
        };

        let mut cu_resource_copy = cu_resource;
        let rc = unsafe {
            (self.cuda.cu_graphics_map_resources)(1, &mut cu_resource_copy, std::ptr::null_mut())
        };
        if rc != CUDA_SUCCESS {
            if let Some(old) = self.import_cache.remove(&fd) {
                unsafe { (self.cuda.cu_graphics_unregister_resource)(old.cu_resource) };
                let _ = self.egl.destroy_image(self.egl_display, old.egl_image);
                unsafe { gl::DeleteTextures(1, &old.gl_tex) };
            }
            self.pop_ctx();
            return self.import_dmabuf(fd, width, height, stride, drm_format, modifier);
        }

        let mut cu_array: CUarray = std::ptr::null_mut();
        let rc = unsafe {
            (self.cuda.cu_graphics_sub_resource_get_mapped_array)(
                &mut cu_array, cu_resource_copy, 0, 0,
            )
        };
        if rc != CUDA_SUCCESS {
            eprintln!("[gpu] cuGraphicsSubResourceGetMappedArray failed: {}", rc);
            unsafe {
                (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource_copy, std::ptr::null_mut());
            }
            self.pop_ctx();
            return None;
        }

        let bpp = 4u32;
        let pitch = width * bpp;
        let buf_size = (pitch * height) as usize;
        let dev_ptr = match self.ensure_dev_buffer(width, height, buf_size) {
            Some(p) => p,
            None => {
                unsafe {
                    (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource_copy, std::ptr::null_mut());
                }
                self.pop_ctx();
                return None;
            }
        };

        let copy = CudaMemcpy2D::array_to_device(
            cu_array, dev_ptr, pitch as usize, (width * bpp) as usize, height as usize,
        );
        let rc = unsafe { (self.cuda.cu_memcpy2d)(&copy) };

        // Sanity-check the first imported frame: NVIDIA's proprietary driver
        // will sometimes accept a mesa/GBM-allocated DMA-BUF (everything
        // returns success, texture binds, cuMemcpy2D succeeds) while silently
        // delivering zero-filled memory. Sample four pixels back to the CPU
        // and warn if they're all zero — the stream will encode successfully
        // but every frame will be black. Known-affected config:
        //   NVIDIA proprietary driver + Wayland + xdg-desktop-portal-gnome
        //   (mutter/Niri) with a mesa-allocated tiled DMA-BUF.
        // Workaround is environmental (switch to xdg-desktop-portal-wlr,
        // use an X11 session, or use a wlroots compositor that allocates
        // DMA-BUFs in a NVIDIA-friendly way).
        static FIRST_FRAME_CHECKED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !FIRST_FRAME_CHECKED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let offsets = [
                0usize,
                (pitch as usize) * (height as usize / 2) + (width as usize / 2) * 4,
                (pitch as usize) * (height as usize - 1),
                (pitch as usize * height as usize).saturating_sub(4),
            ];
            let mut bytes = [0u8; 16];
            for (i, &off) in offsets.iter().enumerate() {
                let mut pixel = [0u8; 4];
                unsafe {
                    (self.cuda.cu_memcpy_dtoh)(
                        pixel.as_mut_ptr() as *mut std::ffi::c_void,
                        dev_ptr + off as u64,
                        4,
                    );
                }
                bytes[i * 4..i * 4 + 4].copy_from_slice(&pixel);
            }
            let all_zero = bytes.iter().all(|&b| b == 0);
            if all_zero {
                eprintln!(
                    "[gpu] WARNING: DMA-BUF imported but CONTENT IS ALL ZEROS. \
                     This is the NVIDIA proprietary driver + mesa-allocated \
                     DMA-BUF interop failure (common on Wayland with \
                     xdg-desktop-portal-gnome). The stream will encode but \
                     every frame will be black. Workarounds: switch to \
                     xdg-desktop-portal-wlr, use an X11 session, or use a \
                     wlroots-based compositor with NVIDIA-friendly DMA-BUF \
                     allocation."
                );
            } else {
                eprintln!(
                    "[gpu] First frame sample OK (non-zero content): topleft={:?} center={:?}",
                    &bytes[0..4], &bytes[4..8]
                );
            }
        }

        unsafe {
            (self.cuda.cu_graphics_unmap_resources)(1, &mut cu_resource_copy, std::ptr::null_mut());
        }
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
        // Clean up cached external-memory imports (the active fast path).
        unsafe { (self.cuda.cu_ctx_push)(self.cu_ctx) };
        for (_, cached) in self.ext_mem_cache.drain() {
            unsafe { (self.cuda.cu_destroy_external_memory)(cached.ext_mem) };
        }
        // EGL/GL cache — active path on NVIDIA + Wayland.
        for (_, cached) in self.import_cache.drain() {
            unsafe { (self.cuda.cu_graphics_unregister_resource)(cached.cu_resource) };
            let _ = self.egl.destroy_image(self.egl_display, cached.egl_image);
            unsafe { gl::DeleteTextures(1, &cached.gl_tex) };
        }
        if let Some((ptr, _, _)) = self.dev_buf.take() {
            unsafe { (self.cuda.cu_mem_free)(ptr) };
        }
        let mut popped: CUcontext = std::ptr::null_mut();
        unsafe { (self.cuda.cu_ctx_pop)(&mut popped) };
        unsafe { gl::DeleteTextures(1, &self.gl_tex) };
        let _ = self.egl.destroy_context(self.egl_display, self.egl_context);
        let _ = self.egl.terminate(self.egl_display);
        unsafe { (self.cuda.cu_ctx_destroy)(self.cu_ctx) };
    }
}

// ────────────────────────────────────────────────────────────────────────────
// VA-API backend (AMD/Intel) — uses FFmpeg's DRM->VAAPI hwframe mapping
// ────────────────────────────────────────────────────────────────────────────

struct VaapiBackend {
    /// FFmpeg DRM hw_device_ctx (AVBufferRef*)
    drm_device_ref: *mut ffmpeg_next::sys::AVBufferRef,
    /// FFmpeg VAAPI hw_device_ctx derived from DRM (AVBufferRef*)
    vaapi_device_ref: *mut ffmpeg_next::sys::AVBufferRef,
    /// VAAPI hw_frames_ctx (AVBufferRef*) — set after encoder init
    vaapi_frames_ref: *mut ffmpeg_next::sys::AVBufferRef,
}

impl VaapiBackend {
    fn new() -> Option<Self> {
        use ffmpeg_next::sys::*;

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
                AVHWDeviceType::AV_HWDEVICE_TYPE_DRM,
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
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
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
        use ffmpeg_next::sys::*;
        unsafe {
            if !self.vaapi_frames_ref.is_null() {
                av_buffer_unref(&mut self.vaapi_frames_ref);
            }

            let frames_ref = av_hwframe_ctx_alloc(self.vaapi_device_ref);
            if frames_ref.is_null() {
                return Err("av_hwframe_ctx_alloc(VAAPI) failed".into());
            }
            let frames_ctx = (*frames_ref).data as *mut AVHWFramesContext;
            (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
            (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*frames_ctx).width = width as libc::c_int;
            (*frames_ctx).height = height as libc::c_int;
            (*frames_ctx).initial_pool_size = 8;

            let rc = av_hwframe_ctx_init(frames_ref);
            if rc < 0 {
                let mut frames_ref_mut = frames_ref;
                av_buffer_unref(&mut frames_ref_mut);
                return Err(format!("av_hwframe_ctx_init(VAAPI) failed: {}", rc));
            }

            self.vaapi_frames_ref = frames_ref;
            Ok(())
        }
    }

    /// Map a DMA-BUF into a VAAPI surface AVFrame.
    /// The returned AVFrame has format=VAAPI and can be sent directly to h264_vaapi.
    ///
    /// `av_hwframe_map` with DRM→VAAPI copies the VA surface handle into the
    /// destination frame, so the AVDRMFrameDescriptor only needs to live through
    /// the mapping call (not through encoding). We build it on the stack here.
    fn map_dmabuf_to_vaapi(
        &self,
        fd: i32,
        width: u32,
        height: u32,
        stride: u32,
        drm_format: u32,
        modifier: u64,
    ) -> Option<ffmpeg_next::frame::Video> {
        use ffmpeg_next::sys::*;

        if self.vaapi_frames_ref.is_null() {
            eprintln!("[gpu] VAAPI frames context not initialized");
            return None;
        }

        unsafe {
            // Build descriptor on the stack — av_hwframe_map copies the VA surface
            // handle so the descriptor only needs to survive through the map call.
            let drm_desc = build_drm_descriptor(fd, width, height, stride, drm_format, modifier);

            // Create DRM_PRIME source frame
            let drm_frame = av_frame_alloc();
            if drm_frame.is_null() {
                return None;
            }
            (*drm_frame).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*drm_frame).width = width as libc::c_int;
            (*drm_frame).height = height as libc::c_int;
            // data[0] points to the AVDRMFrameDescriptor
            (*drm_frame).data[0] = &drm_desc as *const _ as *mut u8;

            // Allocate VAAPI destination frame from the frames pool
            let vaapi_frame = av_frame_alloc();
            if vaapi_frame.is_null() {
                av_frame_free(&mut (drm_frame as *mut AVFrame));
                return None;
            }
            (*vaapi_frame).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;

            // av_hwframe_get_buffer sets hw_frames_ctx internally — don't set it manually
            // or the av_buffer_ref would leak when overwritten.
            let rc = av_hwframe_get_buffer(self.vaapi_frames_ref, vaapi_frame, 0);
            if rc < 0 {
                eprintln!("[gpu] av_hwframe_get_buffer(VAAPI) failed: {}", rc);
                av_frame_free(&mut (drm_frame as *mut AVFrame));
                av_frame_free(&mut (vaapi_frame as *mut AVFrame));
                return None;
            }

            // Map DRM_PRIME -> VAAPI (zero-copy on same GPU)
            let rc = av_hwframe_map(
                vaapi_frame,
                drm_frame,
                AV_HWFRAME_MAP_READ as libc::c_int | AV_HWFRAME_MAP_DIRECT as libc::c_int,
            );

            // Free the DRM frame wrapper (the DMA-BUF fd is NOT closed — we dup'd it)
            (*drm_frame).data[0] = std::ptr::null_mut(); // prevent double-free
            av_frame_free(&mut (drm_frame as *mut AVFrame));

            if rc < 0 {
                eprintln!("[gpu] av_hwframe_map(DRM->VAAPI) failed: {}", rc);
                av_frame_free(&mut (vaapi_frame as *mut AVFrame));
                return None;
            }

            // Wrap raw AVFrame* in ffmpeg_next::frame::Video for safe management
            Some(ffmpeg_next::frame::Video::wrap(vaapi_frame))
        }
    }

    /// Get a reference to the VAAPI hw_device_ctx for encoder setup.
    fn vaapi_device_ref(&self) -> *mut ffmpeg_next::sys::AVBufferRef {
        self.vaapi_device_ref
    }

    /// Get a reference to the VAAPI hw_frames_ctx for encoder setup.
    fn vaapi_frames_ref(&self) -> *mut ffmpeg_next::sys::AVBufferRef {
        self.vaapi_frames_ref
    }
}

impl Drop for VaapiBackend {
    fn drop(&mut self) {
        use ffmpeg_next::sys::*;
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
                vaapi.map_dmabuf_to_vaapi(fd, width, height, stride, drm_format, modifier)
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
    pub fn vaapi_device_ref(&self) -> *mut ffmpeg_next::sys::AVBufferRef {
        match &self.backend {
            GpuBackendInner::Vaapi(vaapi) => vaapi.vaapi_device_ref(),
            _ => std::ptr::null_mut(),
        }
    }

    /// Get VAAPI hw_frames_ctx for encoder setup.
    pub fn vaapi_frames_ref(&self) -> *mut ffmpeg_next::sys::AVBufferRef {
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

    /// NVIDIA only: returns the raw CUcontext pointer. The encoder needs to
    /// use the same CUDA context that our DMA-BUF imports allocate in —
    /// device pointers are context-scoped and NVENC rejects pointers from
    /// a foreign context with EINVAL ("Invalid argument").
    pub fn cuda_ctx_raw(&self) -> *mut std::ffi::c_void {
        if let GpuBackendInner::Cuda(cuda) = &self.backend {
            cuda.cu_ctx
        } else {
            std::ptr::null_mut()
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
) -> ffmpeg_next::sys::AVDRMFrameDescriptor {
    use ffmpeg_next::sys::*;
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
    //   Memory bytes: B G R A -> matches PipeWire "BGRA" byte order
    //
    // We deliberately map `BGRx`/`RGBx` to the *alpha* fourcc variants instead
    // of the "ignored" (X) variants. The pixel layout in memory is byte-for-
    // byte identical, but EGL interprets X-variants as 3-channel RGB and
    // creates the backing GL texture with GL_RGB internal format — which
    // `cuGraphicsGLRegisterImage` refuses (CUDA supports GL_RGBA / GL_R8 /
    // GL_RG8 / GL_RGBA8 etc., not GL_RGB). Reporting alpha lets EGL pick
    // GL_RGBA8, unblocking CUDA import. The alpha channel's value is
    // meaningless since the producer wrote X (padding), but CUDA treats
    // alpha as just another 8-bit channel — content is unchanged.
    match fmt {
        VideoFormat::BGRA | VideoFormat::BGRx => u32::from_le_bytes(*b"AR24"), // DRM_FORMAT_ARGB8888
        VideoFormat::RGBA | VideoFormat::RGBx => u32::from_le_bytes(*b"AB24"), // DRM_FORMAT_ABGR8888
        _ => 0,
    }
}
