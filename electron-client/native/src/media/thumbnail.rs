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
//!   2. Copy the first mip whose longest edge is ≤ READBACK_MAX_EDGE into
//!      a small staging texture (≤ 640×640×4 bytes instead of the full
//!      source — 33 MB at 4K). A source that's already small enough is
//!      copied straight into staging.
//!
//! Then on later ticks:
//!   3. Map(DO_NOT_WAIT); DXGI_ERROR_WAS_STILL_DRAWING = not yet.
//!   4. Area-average down to THUMBNAIL_MAX_EDGE and JPEG-encode.
//!
//! Intermediates are recreated whenever the source size changes (window
//! resize, display mode change — capture textures may change size
//! mid-stream).

#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_RESOURCE_MISC_GENERATE_MIPS,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use super::gpu_pipeline::GpuDevice;

const THUMBNAIL_MAX_EDGE: u32 = 320;
const JPEG_QUALITY: u8 = 70;
/// Longest edge of the mip level that gets read back to the CPU.
const READBACK_MAX_EDGE: u32 = 640;
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
    target_w: u32,
    target_h: u32,
    /// Reusable scratch buffer for the downscaled BGRA bytes.
    scratch: Vec<u8>,
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
    /// completes (the JPEG, or why it failed), `None` otherwise.
    pub fn tick(
        &mut self,
        src: &ID3D11Texture2D,
        start: bool,
    ) -> Option<Result<Vec<u8>, String>> {
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
    fn poll(&mut self) -> Option<Result<Vec<u8>, String>> {
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
                let downscaled = downscale_mapped(state, &mapped);
                unsafe { self.gpu.context.Unmap(&state.staging, 0) };
                Some(downscaled.and_then(|()| {
                    let mut jpeg_buf = Vec::new();
                    Encoder::new(&mut jpeg_buf, JPEG_QUALITY)
                        .encode(
                            &state.scratch,
                            state.target_w as u16,
                            state.target_h as u16,
                            ColorType::Bgra,
                        )
                        .map_err(|e| format!("JPEG encode: {e:?}"))?;
                    Ok(jpeg_buf)
                }))
            }
        }
    }
}

/// Area-average the mapped staging texture into `state.scratch`. Must
/// run between Map and Unmap.
fn downscale_mapped(state: &mut TextureState, mapped: &D3D11_MAPPED_SUBRESOURCE) -> Result<(), String> {
    let row_pitch = mapped.RowPitch as usize;
    let row_bytes = state.read_w as usize * 4;
    if mapped.pData.is_null() || row_pitch < row_bytes {
        return Err(format!(
            "bad staging mapping (pitch {row_pitch} < {row_bytes} or null)"
        ));
    }
    // Only `row_bytes` are guaranteed on the last row, not a full pitch.
    let len = mapped_len(row_pitch, row_bytes, state.read_h as usize);
    // SAFETY: Map succeeded on a read_w × read_h BGRA staging texture:
    // rows 0..read_h-1 span `row_pitch` bytes each and the last row at
    // least `row_bytes`, valid until Unmap (after this returns).
    let src = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
    area_downscale_bgra(
        src,
        state.read_w as usize,
        state.read_h as usize,
        row_pitch,
        &mut state.scratch,
        state.target_w as usize,
        state.target_h as usize,
    );
    Ok(())
}

impl TextureState {
    fn create(device: &ID3D11Device, src_w: u32, src_h: u32) -> Result<Self, String> {
        let (target_w, target_h) = compute_target_size(src_w, src_h);
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
        let scratch = vec![0u8; (target_w as usize) * (target_h as usize) * 4];
        Ok(Self {
            src_w,
            src_h,
            mips,
            mip_level,
            read_w,
            read_h,
            staging,
            target_w,
            target_h,
            scratch,
        })
    }
}

/// Longest edge clamped to THUMBNAIL_MAX_EDGE, aspect ratio preserved.
fn compute_target_size(src_w: u32, src_h: u32) -> (u32, u32) {
    if src_w >= src_h {
        let target_w = src_w.min(THUMBNAIL_MAX_EDGE);
        let target_h = ((target_w as u64) * (src_h as u64) / (src_w.max(1) as u64)) as u32;
        (target_w.max(1), target_h.max(1))
    } else {
        let target_h = src_h.min(THUMBNAIL_MAX_EDGE);
        let target_w = ((target_h as u64) * (src_w as u64) / (src_h.max(1) as u64)) as u32;
        (target_w.max(1), target_h.max(1))
    }
}

/// First mip level whose longest edge is ≤ READBACK_MAX_EDGE.
fn pick_mip_level(w: u32, h: u32) -> u32 {
    let mut level = 0;
    while mip_dim(w, level).max(mip_dim(h, level)) > READBACK_MAX_EDGE && level < 15 {
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

/// Area-average (box) downscale of BGRA rows with `src_pitch` stride
/// into a tightly packed `dst_w × dst_h` buffer. The source is already
/// within 2× of the target (the GPU did the rest), so this is cheap.
/// Reads only `src_w * 4` bytes per row and never indexes out of bounds.
fn area_downscale_bgra(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    src_pitch: usize,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = ((dy + 1) * src_h / dst_h).max(y0 + 1).min(src_h);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = ((dx + 1) * src_w / dst_w).max(x0 + 1).min(src_w);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let row = y * src_pitch;
                let Some(px_row) = src.get(row + x0 * 4..row + x1 * 4) else {
                    continue;
                };
                for px in px_row.chunks_exact(4) {
                    acc[0] += px[0] as u32;
                    acc[1] += px[1] as u32;
                    acc[2] += px[2] as u32;
                    acc[3] += px[3] as u32;
                    n += 1;
                }
            }
            let d = (dy * dst_w + dx) * 4;
            if let (Some(out), true) = (dst.get_mut(d..d + 4), n > 0) {
                for (o, a) in out.iter_mut().zip(acc) {
                    *o = ((a + n / 2) / n) as u8;
                }
            }
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

    #[test]
    fn mip_level_reads_back_at_most_640() {
        assert_eq!(pick_mip_level(640, 480), 0);
        assert_eq!(pick_mip_level(1920, 1080), 2); // 480×270
        assert_eq!(pick_mip_level(3840, 2160), 3); // 480×270
        assert_eq!(pick_mip_level(2560, 1440), 2); // 640×360
        assert_eq!(pick_mip_level(1080, 1920), 2); // 270×480
        assert_eq!((mip_dim(1366, 2), mip_dim(768, 2)), (341, 192));
        assert_eq!(mip_dim(1, 5), 1);
    }

    #[test]
    fn mapped_len_excludes_last_row_padding() {
        assert_eq!(mapped_len(2048, 1920, 3), 2048 * 2 + 1920);
        assert_eq!(mapped_len(2048, 1920, 0), 0);
    }

    #[test]
    fn area_downscale_averages_and_respects_pitch() {
        // 4×2 source, pitch 20 (4 bytes padding, absent on the last row).
        let mut src = vec![0u8; 20 + 16];
        for x in 0..4 {
            let v = (x * 40) as u8;
            src[x * 4..x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            src[20 + x * 4..20 + x * 4 + 4].copy_from_slice(&[v + 20, v + 20, v + 20, 255]);
        }
        let mut dst = vec![0u8; 2 * 4];
        area_downscale_bgra(&src, 4, 2, 20, &mut dst, 2, 1);
        // Left: avg(0,40,20,60)=30, right: avg(80,120,100,140)=110.
        assert_eq!(dst, vec![30, 30, 30, 255, 110, 110, 110, 255]);
    }
}
