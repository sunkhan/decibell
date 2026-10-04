//! Stream thumbnail generation.
//!
//! Mirrors the renderer-encoded path's `maybeCaptureThumbnail` in
//! StreamCapture.ts: produces a JPEG preview every few seconds from
//! the source BGRA frame. The community server relays these to
//! voice-channel participants who aren't actively watching the
//! stream so they see a poster image on the participant tile
//! instead of a black square.
//!
//! Non-blocking: the encoder thread calls `tick` once per frame, and
//! nothing here ever waits on the GPU (a game-loaded GPU queue would turn
//! any wait into a hitch in the stream).
//!
//! Pipeline, on a `start` tick:
//!   1. Copy the source into mip 0 of a GENERATE_MIPS texture and let
//!      the GPU build the chain (box-filtered halvings).
//!   2. Copy the smallest mip that is still at least THUMBNAIL_MAX_EDGE on
//!      its longest edge into a staging texture (≤ ~2× the thumbnail, not
//!      the full source — 33 MB at 4K). A source that's already small
//!      enough is copied straight into staging.
//!
//! Then on later ticks:
//!   3. Map(DO_NOT_WAIT); DXGI_ERROR_WAS_STILL_DRAWING = not yet.
//!   4. Copy the rows out and Unmap. The area-average down to
//!      THUMBNAIL_MAX_EDGE and the JPEG encode happen on the thumbnail
//!      worker (thumb_encode.rs), off the encoder thread — at 960 px they
//!      cost several ms, which here would be a late frame.
//!
//! Intermediates are recreated whenever the source size changes (window
//! resize, display mode change — capture textures may change size
//! mid-stream).

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use jpeg_encoder::ColorType;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_RESOURCE_MISC_GENERATE_MIPS,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use super::gpu_pipeline::GpuDevice;
use super::thumb_encode::{ThumbnailJob, THUMBNAIL_MAX_EDGE};

/// A readback that hasn't completed after this long is abandoned (its
/// staging texture is released; the GPU finishes with it on its own).
const READBACK_STALE_AFTER: Duration = Duration::from_secs(5);

pub struct ThumbnailGenerator {
    gpu: GpuDevice,
    state: Option<TextureState>,
    /// Set while a readback is queued in `state.staging`.
    pending_since: Option<Instant>,
}

struct TextureState {
    src_w: u32,
    src_h: u32,
    /// Mip-chain texture + the SRV GenerateMips needs. None when the
    /// source is small enough to read back at full size.
    mips: Option<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    /// Mip level copied into `staging` (0 when `mips` is None).
    mip_level: u32,
    read_w: u32,
    read_h: u32,
    staging: ID3D11Texture2D,
}

impl ThumbnailGenerator {
    pub fn new(gpu: GpuDevice) -> Self {
        Self {
            gpu,
            state: None,
            pending_since: None,
        }
    }

    /// Advance the thumbnail pipeline by one frame; never blocks.
    ///
    /// `src` is the current BGRA frame. `start = true` asks for a new
    /// thumbnail: a GPU downscale + readback of `src` is queued if none
    /// is in flight. Returns `Some` on the call where a queued readback
    /// completes (the read-back pixels for the thumbnail worker, or why it
    /// failed), `None` otherwise.
    pub fn tick(
        &mut self,
        src: &ID3D11Texture2D,
        start: bool,
    ) -> Option<Result<ThumbnailJob, String>> {
        let finished = if self.pending_since.is_some() {
            self.poll()
        } else {
            None
        };
        if start && self.pending_since.is_none() {
            if let Err(e) = self.begin(src) {
                if finished.is_some() {
                    log::warn!("[thumbnail] start failed: {e}");
                } else {
                    return Some(Err(e));
                }
            }
        }
        finished
    }

    /// Queue the GPU downscale + copy into staging. Never waits.
    fn begin(&mut self, src: &ID3D11Texture2D) -> Result<(), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.GetDesc(&mut desc) };
        if desc.Width == 0 || desc.Height == 0 {
            return Err("zero-sized source texture".to_string());
        }
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM || desc.SampleDesc.Count != 1 {
            return Err(format!(
                "unsupported source texture (format {:?}, {} samples)",
                desc.Format, desc.SampleDesc.Count
            ));
        }

        // (Re)build the intermediates when the source size changed.
        let stale = match &self.state {
            None => true,
            Some(s) => s.src_w != desc.Width || s.src_h != desc.Height,
        };
        if stale {
            self.state = None;
            self.state = Some(TextureState::create(
                &self.gpu.device,
                desc.Width,
                desc.Height,
            )?);
        }
        let Some(state) = self.state.as_ref() else {
            return Err("thumbnail state missing".to_string());
        };

        let ctx = &self.gpu.context;
        unsafe {
            match &state.mips {
                Some((mip_tex, srv)) => {
                    ctx.CopySubresourceRegion(mip_tex, 0, 0, 0, 0, src, 0, None);
                    ctx.GenerateMips(srv);
                    ctx.CopySubresourceRegion(
                        &state.staging,
                        0,
                        0,
                        0,
                        0,
                        mip_tex,
                        state.mip_level,
                        None,
                    );
                }
                None => ctx.CopySubresourceRegion(&state.staging, 0, 0, 0, 0, src, 0, None),
            }
            // Submit now so the copy is in flight by the next poll —
            // Map(DO_NOT_WAIT) on unsubmitted work would just keep saying
            // "still drawing". Flush doesn't wait for the GPU.
            ctx.Flush();
        }
        self.pending_since = Some(Instant::now());
        Ok(())
    }

    /// Try to complete the pending readback without waiting.
    fn poll(&mut self) -> Option<Result<ThumbnailJob, String>> {
        let since = self.pending_since?;
        let Some(state) = self.state.as_mut() else {
            self.pending_since = None;
            return None;
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let hr = unsafe {
            self.gpu.context.Map(
                &state.staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        };
        match hr {
            Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                if since.elapsed() >= READBACK_STALE_AFTER {
                    // Release the staging texture rather than poll it
                    // forever; the next start builds fresh intermediates.
                    self.state = None;
                    self.pending_since = None;
                    return Some(Err("thumbnail readback timed out".to_string()));
                }
                None
            }
            Err(e) => {
                self.pending_since = None;
                Some(Err(format!("Map thumbnail staging: {e:?}")))
            }
            Ok(()) => {
                self.pending_since = None;
                let copied = copy_mapped(state, &mapped);
                unsafe { self.gpu.context.Unmap(&state.staging, 0) };
                Some(copied.map(|pixels| ThumbnailJob {
                    pixels,
                    width: state.read_w as usize,
                    height: state.read_h as usize,
                    stride: state.read_w as usize * 4,
                    color: ColorType::Bgra,
                }))
            }
        }
    }
}

/// Copy the mapped staging texture into a tightly packed BGRA buffer. Must
/// run between Map and Unmap. A plain row memcpy (≤ ~8 MB); the scaling
/// work is the worker's.
fn copy_mapped(state: &TextureState, mapped: &D3D11_MAPPED_SUBRESOURCE) -> Result<Vec<u8>, String> {
    let row_pitch = mapped.RowPitch as usize;
    let row_bytes = state.read_w as usize * 4;
    let rows = state.read_h as usize;
    if mapped.pData.is_null() || row_pitch < row_bytes {
        return Err(format!(
            "bad staging mapping (pitch {row_pitch} < {row_bytes} or null)"
        ));
    }
    // Only `row_bytes` are guaranteed on the last row, not a full pitch.
    let len = mapped_len(row_pitch, row_bytes, rows);
    // SAFETY: Map succeeded on a read_w × read_h BGRA staging texture:
    // rows 0..read_h-1 span `row_pitch` bytes each and the last row at
    // least `row_bytes`, valid until Unmap (after this returns).
    let src = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
    let mut out = Vec::with_capacity(row_bytes * rows);
    for y in 0..rows {
        out.extend_from_slice(&src[y * row_pitch..y * row_pitch + row_bytes]);
    }
    Ok(out)
}

impl TextureState {
    fn create(device: &ID3D11Device, src_w: u32, src_h: u32) -> Result<Self, String> {
        let mip_level = pick_mip_level(src_w, src_h);
        let read_w = mip_dim(src_w, mip_level);
        let read_h = mip_dim(src_h, mip_level);
        let mips = if mip_level > 0 {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: src_w,
                Height: src_h,
                // Only as deep as the level we read back.
                MipLevels: mip_level + 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
                .map_err(|e| format!("CreateTexture2D (thumbnail mips): {e:?}"))?;
            let tex = tex.ok_or("CreateTexture2D (thumbnail mips) returned None")?;
            let mut srv: Option<ID3D11ShaderResourceView> = None;
            unsafe { device.CreateShaderResourceView(&tex, None, Some(&mut srv)) }
                .map_err(|e| format!("CreateShaderResourceView (thumbnail mips): {e:?}"))?;
            let srv = srv.ok_or("CreateShaderResourceView (thumbnail mips) returned None")?;
            Some((tex, srv))
        } else {
            None
        };
        let staging = create_staging_texture(device, read_w, read_h)?;
        Ok(Self {
            src_w,
            src_h,
            mips,
            mip_level,
            read_w,
            read_h,
            staging,
        })
    }
}

/// The smallest mip level whose longest edge is still ≥ THUMBNAIL_MAX_EDGE
/// (level 0 when the source is already that small). The CPU then only ever
/// averages down by less than 2×, from at most ~1920 px, so the readback
/// stays ≤ ~8 MB — 1080p reads back 960×540, 1440p 1280×720, 4K 960×540.
fn pick_mip_level(w: u32, h: u32) -> u32 {
    let mut level = 0;
    while level < 15 && mip_dim(w, level + 1).max(mip_dim(h, level + 1)) >= THUMBNAIL_MAX_EDGE {
        level += 1;
    }
    level
}

/// D3D11 mip dimension: max(1, floor(d / 2^level)).
fn mip_dim(d: u32, level: u32) -> u32 {
    d.checked_shr(level).unwrap_or(0).max(1)
}

/// Bytes of a mapping that are guaranteed readable: full pitch for every
/// row but the last, which only has its pixels.
fn mapped_len(row_pitch: usize, row_bytes: usize, rows: usize) -> usize {
    if rows == 0 {
        0
    } else {
        row_pitch * (rows - 1) + row_bytes
    }
}

fn create_staging_texture(device: &ID3D11Device, w: u32, h: u32) -> Result<ID3D11Texture2D, String> {
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
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
        .map_err(|e| format!("CreateTexture2D staging: {e:?}"))?;
    tex.ok_or_else(|| "CreateTexture2D returned None".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_level_is_the_smallest_still_at_least_the_thumbnail() {
        assert_eq!(pick_mip_level(640, 480), 0); // smaller than a thumbnail
        assert_eq!(pick_mip_level(1366, 768), 0); // mip 1 (683) would upscale
        assert_eq!(pick_mip_level(1920, 1080), 1); // 960×540
        assert_eq!(pick_mip_level(2560, 1440), 1); // 1280×720
        assert_eq!(pick_mip_level(3840, 2160), 2); // 960×540
        assert_eq!(pick_mip_level(5120, 1440), 2); // 1280×360
        assert_eq!(pick_mip_level(1080, 1920), 1); // 540×960
        assert_eq!((mip_dim(1366, 2), mip_dim(768, 2)), (341, 192));
        assert_eq!(mip_dim(1, 5), 1);
    }

    #[test]
    fn mapped_len_excludes_last_row_padding() {
        assert_eq!(mapped_len(2048, 1920, 3), 2048 * 2 + 1920);
        assert_eq!(mapped_len(2048, 1920, 0), 0);
    }
}
