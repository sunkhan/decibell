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
        let egl_display = match unsafe { egl.get_display(khronos_egl::DEFAULT_DISPLAY) } {
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

        let config_attribs: [khronos_egl::Int; 5] = [
            khronos_egl::RENDERABLE_TYPE, khronos_egl::OPENGL_ES2_BIT,
            khronos_egl::SURFACE_TYPE, 0,
            khronos_egl::NONE,
        ];
        let config = match egl.choose_first_config(egl_display, &config_attribs) {
            Ok(Some(c)) => c,
            _ => {
                eprintln!("[gpu] eglChooseConfig failed");
                unsafe { (cuda.cu_ctx_destroy)(cu_ctx) };
                return None;
            }
        };

        let ctx_attribs: [khronos_egl::Int; 3] = [
            khronos_egl::CONTEXT_CLIENT_VERSION, 2,
            khronos_egl::NONE,
        ];
        let egl_context = match egl.create_context(egl_display, config, None, &ctx_attribs) {
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
            egl.get_proc_address(name)
                .map_or(std::ptr::null(), |p| p as *const std::ffi::c_void)
        });

        // Load glEGLImageTargetTexture2DOES extension
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

        // 4. Map -> get CUarray
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

        // 6. cuMemcpy2D: CUarray -> CUdeviceptr (GPU-to-GPU, <0.1ms for 1080p)
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
    match fmt {
        VideoFormat::BGRA => u32::from_le_bytes(*b"AR24"), // DRM_FORMAT_ARGB8888
        VideoFormat::BGRx => u32::from_le_bytes(*b"XR24"), // DRM_FORMAT_XRGB8888
        VideoFormat::RGBA => u32::from_le_bytes(*b"AB24"), // DRM_FORMAT_ABGR8888
        VideoFormat::RGBx => u32::from_le_bytes(*b"XB24"), // DRM_FORMAT_XBGR8888
        _ => 0,
    }
}
