//! Stream thumbnail generation.
//!
//! Mirrors the renderer-encoded path's `maybeCaptureThumbnail` in
//! StreamCapture.ts: produces a JPEG preview every few seconds from
//! the source BGRA frame. The community server relays these to
//! voice-channel participants who aren't actively watching the
//! stream so they see a poster image on the participant tile
//! instead of a black square.
//!
//! Pipeline:
//!   1. CopyResource the source BGRA → CPU-readable staging texture.
//!   2. Map the staging texture (blocks until GPU is done).
//!   3. Nearest-neighbour downscale to 320px max edge.
//!   4. JPEG-encode the downscaled BGRA.
//!
//! Staging texture is lazy-initialised on the first capture so we
//! pick up whatever dimensions WGC actually delivers (which may
//! differ from what the renderer requested when the user picked
//! "Source" resolution).

#![cfg(target_os = "windows")]

use jpeg_encoder::{ColorType, Encoder};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};

use super::gpu_pipeline::GpuDevice;

const THUMBNAIL_MAX_EDGE: u32 = 320;
const JPEG_QUALITY: u8 = 70;

pub struct ThumbnailGenerator {
    gpu: GpuDevice,
    state: Option<TextureState>,
}

struct TextureState {
    src_w: u32,
    src_h: u32,
    target_w: u32,
    target_h: u32,
    staging: ID3D11Texture2D,
    /// Reusable scratch buffer for the downscaled BGRA bytes.
    scratch: Vec<u8>,
}

impl ThumbnailGenerator {
    pub fn new(gpu: GpuDevice) -> Self {
        Self { gpu, state: None }
    }

    /// CPU-read the BGRA texture, nearest-neighbour downscale,
    /// JPEG-encode. Returns the JPEG bytes ready to ship.
    pub fn capture(&mut self, src: &ID3D11Texture2D) -> Result<Vec<u8>, String> {
        let (src_w, src_h) = unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            src.GetDesc(&mut desc);
            (desc.Width, desc.Height)
        };

        // Lazy init / re-init if the source dimensions changed (the
        // user resized the captured window mid-stream, etc.).
        let needs_init = match &self.state {
            None => true,
            Some(s) => s.src_w != src_w || s.src_h != src_h,
        };
        if needs_init {
            self.state = Some(TextureState::create(&self.gpu, src_w, src_h)?);
        }
        let state = self.state.as_mut().expect("just initialised");

        // GPU → CPU readback path: CopyResource into a STAGING texture
        // (which is CPU-mappable but not bindable as a shader resource
        // or render target). Map blocks until the GPU finishes the
        // copy. The 8 MB / 3 sec readback (at 1080p source) is well
        // under PCIe bandwidth — no measurable impact.
        let row_pitch: usize;
        let src_data_ptr: *const u8;
        unsafe {
            self.gpu.context.CopyResource(&state.staging, src);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.gpu
                .context
                .Map(&state.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| format!("Map staging: {e:?}"))?;
            row_pitch = mapped.RowPitch as usize;
            src_data_ptr = mapped.pData as *const u8;
        }

        // SAFETY: the slice is valid for the duration of Map; we
        // copy out before Unmap below.
        let total_len = row_pitch * state.src_h as usize;
        let src_slice = unsafe { std::slice::from_raw_parts(src_data_ptr, total_len) };

        nearest_neighbor_downscale_bgra(
            src_slice,
            state.src_w,
            state.src_h,
            row_pitch,
            &mut state.scratch,
            state.target_w,
            state.target_h,
        );

        unsafe {
            self.gpu.context.Unmap(&state.staging, 0);
        }

        let mut jpeg_buf = Vec::new();
        let encoder = Encoder::new(&mut jpeg_buf, JPEG_QUALITY);
        encoder
            .encode(
                &state.scratch,
                state.target_w as u16,
                state.target_h as u16,
                ColorType::Bgra,
            )
            .map_err(|e| format!("JPEG encode: {e:?}"))?;
        Ok(jpeg_buf)
    }
}

impl TextureState {
    fn create(gpu: &GpuDevice, src_w: u32, src_h: u32) -> Result<Self, String> {
        let (target_w, target_h) = compute_target_size(src_w, src_h);
        let staging = create_staging_texture(&gpu.device, src_w, src_h)?;
        let scratch = vec![0u8; (target_w as usize) * (target_h as usize) * 4];
        Ok(Self {
            src_w,
            src_h,
            target_w,
            target_h,
            staging,
            scratch,
        })
    }
}

/// Longest edge clamped to THUMBNAIL_MAX_EDGE, aspect ratio preserved.
fn compute_target_size(src_w: u32, src_h: u32) -> (u32, u32) {
    if src_w >= src_h {
        let target_w = src_w.min(THUMBNAIL_MAX_EDGE);
        let target_h = ((target_w as u64) * (src_h as u64) / (src_w.max(1) as u64)) as u32;
        (target_w, target_h.max(1))
    } else {
        let target_h = src_h.min(THUMBNAIL_MAX_EDGE);
        let target_w = ((target_h as u64) * (src_w as u64) / (src_h.max(1) as u64)) as u32;
        (target_w.max(1), target_h)
    }
}

fn create_staging_texture(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    w: u32,
    h: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut tex))
            .map_err(|e| format!("CreateTexture2D staging: {e:?}"))?;
    }
    tex.ok_or_else(|| "CreateTexture2D returned None".to_string())
}

/// Nearest-neighbour downscale. Quality is "fine for a 320×180
/// thumbnail" — at the 5–8× scale factor from 1080p / 1440p sources
/// the artefacts are imperceptible at the display size.
fn nearest_neighbor_downscale_bgra(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    src_row_pitch: usize,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    for dy in 0..dst_h {
        let sy = ((dy as u64) * (src_h as u64) / (dst_h.max(1) as u64)) as usize;
        let src_row_off = sy * src_row_pitch;
        let dst_row_off = (dy * dst_w * 4) as usize;
        for dx in 0..dst_w {
            let sx = ((dx as u64) * (src_w as u64) / (dst_w.max(1) as u64)) as usize;
            let src_off = src_row_off + sx * 4;
            let dst_off = dst_row_off + (dx as usize) * 4;
            dst[dst_off..dst_off + 4].copy_from_slice(&src[src_off..src_off + 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_size_landscape() {
        assert_eq!(compute_target_size(1920, 1080), (320, 180));
        assert_eq!(compute_target_size(3840, 2160), (320, 180));
    }

    #[test]
    fn target_size_portrait() {
        assert_eq!(compute_target_size(1080, 1920), (180, 320));
    }

    #[test]
    fn target_size_square() {
        assert_eq!(compute_target_size(1000, 1000), (320, 320));
    }

    #[test]
    fn target_size_already_smaller() {
        assert_eq!(compute_target_size(160, 120), (160, 120));
    }
}
