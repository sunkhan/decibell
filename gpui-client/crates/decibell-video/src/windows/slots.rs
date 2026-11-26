//! The textures a stream's pictures are handed to the renderer in.
//!
//! A hardware slot is one `NV12` texture (what a D3D11VA decoder's output slice can be copied
//! into: `CopySubresourceRegion` needs the same format on both sides) with two views of it,
//! `R8_UNORM` of the luma plane and `R8G8_UNORM` of the chroma plane. A software slot is an
//! `R8` and an `R8G8` texture, which every Direct3D 11 device has. The renderer sees two plane
//! views either way.
//!
//! Each slot has one long-lived `Arc<D3d11SurfaceFrame>`. Publishing a picture clones that Arc
//! into the surface; the renderer clones it while it issues the draws of a frame. The slot is
//! free to be written when the decoder holds the only reference: nothing is allocated per
//! picture and nobody waits for anybody.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui_windows::windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2D;
use gpui_windows::windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0,
    D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC, D3D11_USAGE, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    ID3D11ShaderResourceView, ID3D11Texture2D,
};
use gpui_windows::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_SAMPLE_DESC,
};
use gpui_windows::{D3d11SurfaceFrame, D3d11SurfacePlanes, SurfaceColor};

use super::{GpuDevice, LIVE_PLANES, d3d_error};
use crate::decode::{DecodeError, DecodeResult, LumaRow};

/// More than the renderer can hold at once: the newest published picture, plus the one a
/// frame being drawn samples, plus one to write, with spares (the Linux backends' number).
pub(crate) const SLOT_COUNT: usize = 5;

/// Counts a slot's two planes as alive until the last reference to its frame is gone.
struct LivePlanes;

impl LivePlanes {
    fn new() -> Self {
        LIVE_PLANES.fetch_add(2, Ordering::Relaxed);
        Self
    }
}

impl Drop for LivePlanes {
    fn drop(&mut self) {
        LIVE_PLANES.fetch_sub(2, Ordering::Relaxed);
    }
}

/// What a slot is made of.
pub(crate) enum SlotTextures {
    /// One `NV12` texture of even size.
    Nv12(ID3D11Texture2D),
    /// Luma (`R8`, picture size) and chroma (`R8G8`, half size, rounded up).
    Planes {
        luma: ID3D11Texture2D,
        chroma: ID3D11Texture2D,
    },
}

pub(crate) struct Slot {
    pub frame: Arc<D3d11SurfaceFrame>,
    pub textures: SlotTextures,
}

/// `value` rounded up to even: an `NV12` texture's width and height must be.
pub(crate) fn even(value: u32) -> u32 {
    value.next_multiple_of(2)
}

fn texture(
    device: &GpuDevice,
    format: DXGI_FORMAT,
    width: u32,
    height: u32,
    usage: D3D11_USAGE,
    bind_flags: u32,
    cpu_access: u32,
) -> DecodeResult<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: usage,
        BindFlags: bind_flags,
        CPUAccessFlags: cpu_access,
        MiscFlags: 0,
    };
    let mut texture = None;
    // Safety: a valid description and out pointer; the device is free-threaded.
    unsafe { device.device().CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|error| d3d_error(device, "CreateTexture2D", error))?;
    texture.ok_or_else(|| DecodeError::decoder("CreateTexture2D returned no texture"))
}

/// A view of `texture` (or of one plane of an `NV12` texture) read as `format`.
fn view(
    device: &GpuDevice,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
) -> DecodeResult<ID3D11ShaderResourceView> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV {
                MostDetailedMip: 0,
                MipLevels: 1,
            },
        },
    };
    let mut view = None;
    // Safety: a valid description and out pointer; the device is free-threaded.
    unsafe {
        device
            .device()
            .CreateShaderResourceView(texture, Some(&desc), Some(&mut view))
    }
    .map_err(|error| d3d_error(device, "CreateShaderResourceView", error))?;
    view.ok_or_else(|| DecodeError::decoder("CreateShaderResourceView returned no view"))
}

/// A stream's slots for one picture size.
pub(crate) struct SlotRing {
    /// The picture's size.
    pub size: (u32, u32),
    slots: Vec<Slot>,
    device: Arc<GpuDevice>,
}

impl SlotRing {
    /// `nv12`: slots a decoder's output is copied into; else slots planes are uploaded to.
    pub fn new(
        device: &Arc<GpuDevice>,
        width: u32,
        height: u32,
        color: SurfaceColor,
        nv12: bool,
    ) -> DecodeResult<Self> {
        if width == 0 || height == 0 {
            return Err(DecodeError::decoder("the decoder returned an empty picture"));
        }
        let bind = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        let mut slots = Vec::with_capacity(SLOT_COUNT);
        for _ in 0..SLOT_COUNT {
            let (textures, luma, chroma, texture_size) = if nv12 {
                let (texture_width, texture_height) = (even(width), even(height));
                let texture = texture(
                    device,
                    DXGI_FORMAT_NV12,
                    texture_width,
                    texture_height,
                    D3D11_USAGE_DEFAULT,
                    bind,
                    0,
                )
                .map_err(|error| DecodeError {
                    failure: error.failure,
                    message: format!(
                        "{} (the GPU cannot sample NV12 textures?)",
                        error.message
                    ),
                })?;
                let luma = view(device, &texture, DXGI_FORMAT_R8_UNORM)?;
                let chroma = view(device, &texture, DXGI_FORMAT_R8G8_UNORM)?;
                (
                    SlotTextures::Nv12(texture),
                    luma,
                    chroma,
                    (texture_width, texture_height),
                )
            } else {
                let luma_texture = texture(
                    device,
                    DXGI_FORMAT_R8_UNORM,
                    width,
                    height,
                    D3D11_USAGE_DEFAULT,
                    bind,
                    0,
                )?;
                let chroma_texture = texture(
                    device,
                    DXGI_FORMAT_R8G8_UNORM,
                    width.div_ceil(2),
                    height.div_ceil(2),
                    D3D11_USAGE_DEFAULT,
                    bind,
                    0,
                )?;
                let luma = view(device, &luma_texture, DXGI_FORMAT_R8_UNORM)?;
                let chroma = view(device, &chroma_texture, DXGI_FORMAT_R8G8_UNORM)?;
                (
                    SlotTextures::Planes {
                        luma: luma_texture,
                        chroma: chroma_texture,
                    },
                    luma,
                    chroma,
                    // The luma texture is the picture.
                    (width, height),
                )
            };
            slots.push(Slot {
                frame: Arc::new(D3d11SurfaceFrame {
                    planes: Arc::new(D3d11SurfacePlanes {
                        luma,
                        chroma,
                        size: (width, height),
                        texture_size,
                    }),
                    color,
                    device: device.device().clone(),
                    in_use: Some(Box::new(LivePlanes::new())),
                }),
                textures,
            });
        }
        Ok(Self {
            size: (width, height),
            slots,
            device: device.clone(),
        })
    }

    /// A slot nobody but us refers to, carrying `color`. `None`: the renderer holds them all.
    pub fn free(&mut self, color: SurfaceColor) -> Option<&Slot> {
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| Arc::strong_count(&slot.frame) == 1)?;
        if slot.frame.color != color {
            // The stream's colour description changed (rare: a new sequence header). The
            // planes stay; the frame that carries them is made again.
            let planes = slot.frame.planes.clone();
            slot.frame = Arc::new(D3d11SurfaceFrame {
                planes,
                color,
                device: self.device.device().clone(),
                in_use: Some(Box::new(LivePlanes::new())),
            });
        }
        Some(slot)
    }
}

/// Reads one row of a slot's luma plane back from the GPU (tests and the capability probe):
/// a copy into a CPU-readable texture and a `Map`, which waits for everything issued before it
/// (the decode and the copy into the slot included).
pub(crate) fn read_luma_row(
    device: &GpuDevice,
    slot: &Slot,
    width: u32,
    height: u32,
    row: u32,
    serial: u64,
) -> DecodeResult<LumaRow> {
    // An `NV12` copy region must be even on both axes: the two rows the wanted one is in.
    let (source, format, top, rows, copy_width) = match &slot.textures {
        SlotTextures::Nv12(texture) => (texture, DXGI_FORMAT_NV12, row & !1, 2, even(width)),
        SlotTextures::Planes { luma, .. } => (luma, DXGI_FORMAT_R8_UNORM, row, 1, width),
    };
    let staging = texture(
        device,
        format,
        copy_width,
        rows,
        D3D11_USAGE_STAGING,
        0,
        D3D11_CPU_ACCESS_READ.0 as u32,
    )?;
    let region = D3D11_BOX {
        left: 0,
        top,
        front: 0,
        right: copy_width,
        bottom: top + rows,
        back: 1,
    };
    let context = device.context();
    let _held = device.lock();
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // Safety: a copy between two textures of one format, the region inside the source (its
    // size is the even-rounded picture size) and the size of the destination; the mapping is
    // read within its row pitch and unmapped before the staging texture is dropped.
    unsafe {
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, 0, Some(&region));
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|error| d3d_error(device, "mapping the read-back texture", error))?;
        let result = if mapped.pData.is_null() || (mapped.RowPitch as usize) < width as usize {
            Err(DecodeError::decoder("the read-back texture mapped empty"))
        } else {
            // The luma plane comes first in a mapped NV12 texture, `RowPitch` bytes a row.
            let start = mapped
                .pData
                .cast::<u8>()
                .add((row - top) as usize * mapped.RowPitch as usize);
            Ok(std::slice::from_raw_parts(start, width as usize).to_vec())
        };
        context.Unmap(&staging, 0);
        result.map(|bytes| LumaRow {
            serial,
            width,
            height,
            row,
            bytes,
        })
    }
}
