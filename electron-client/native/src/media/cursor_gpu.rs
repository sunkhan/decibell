//! GPU cursor compositing for capture_dxgi: alpha-blends the pointer
//! shape onto a BGRA frame texture with one tiny draw (a screen-space
//! quad through a 3-line pixel shader). Nothing here waits on the GPU —
//! the previous CPU blend needed a staging `Map`, which on a GPU-bound
//! game stalled behind the game's queued frames (and, under the device's
//! multithread protection, stalled the encoder thread with it), capping
//! a 60 fps stream around 30.

#![cfg(target_os = "windows")]

use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext, ID3D11PixelShader,
    ID3D11RenderTargetView, ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE,
    D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD,
    D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_ZERO, D3D11_BUFFER_DESC, D3D11_COLOR_WRITE_ENABLE_ALL,
    D3D11_COMPARISON_NEVER, D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_RENDER_TARGET_BLEND_DESC,
    D3D11_SAMPLER_DESC, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows_core::BOOL;

use super::cursor_blend::{cursor_to_bgra, CursorImage};

/// Quad from SV_VertexID (no vertex buffer / input layout), positioned
/// by a pixel rect in the constant buffer; the rasterizer clips a cursor
/// that hangs off the frame edge.
const HLSL: &str = r#"
cbuffer C : register(b0) { float4 rect; float4 frame; };
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vs(uint id : SV_VertexID) {
    float2 uv = float2(id & 1, id >> 1);
    float2 px = rect.xy + uv * rect.zw;
    float2 ndc = float2(px.x / frame.x * 2.0 - 1.0, 1.0 - px.y / frame.y * 2.0);
    VSOut o; o.pos = float4(ndc, 0.0, 1.0); o.uv = uv; return o;
}
Texture2D tex : register(t0);
SamplerState smp : register(s0);
float4 ps(VSOut i) : SV_Target { return tex.Sample(smp, i.uv); }
"#;

pub struct CursorCompositor {
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    blend: ID3D11BlendState,
    sampler: ID3D11SamplerState,
    cb: ID3D11Buffer,
    /// Uploaded shape: view + size in pixels.
    shape: Option<(ID3D11ShaderResourceView, u32, u32)>,
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let hr = unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const _,
            HLSL.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = hr {
        let msg = errors
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer() as *const u8,
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return Err(format!("D3DCompile: {e:?} {msg}"));
    }
    let blob = code.ok_or("D3DCompile returned no bytecode")?;
    Ok(unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
    }
    .to_vec())
}

impl CursorCompositor {
    pub fn new(device: &ID3D11Device) -> Result<Self, String> {
        let vs_code = compile(s!("vs"), s!("vs_4_0"))?;
        let ps_code = compile(s!("ps"), s!("ps_4_0"))?;

        let mut vs: Option<ID3D11VertexShader> = None;
        unsafe { device.CreateVertexShader(&vs_code, None, Some(&mut vs)) }
            .map_err(|e| format!("CreateVertexShader: {e:?}"))?;
        let mut ps: Option<ID3D11PixelShader> = None;
        unsafe { device.CreatePixelShader(&ps_code, None, Some(&mut ps)) }
            .map_err(|e| format!("CreatePixelShader: {e:?}"))?;

        // Straight-alpha src-over.
        let mut bd = D3D11_BLEND_DESC::default();
        bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: BOOL(1),
            SrcBlend: D3D11_BLEND_SRC_ALPHA,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ONE,
            DestBlendAlpha: D3D11_BLEND_ZERO,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut blend: Option<ID3D11BlendState> = None;
        unsafe { device.CreateBlendState(&bd, Some(&mut blend)) }
            .map_err(|e| format!("CreateBlendState: {e:?}"))?;

        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: f32::MAX,
        };
        let mut sampler: Option<ID3D11SamplerState> = None;
        unsafe { device.CreateSamplerState(&sd, Some(&mut sampler)) }
            .map_err(|e| format!("CreateSamplerState: {e:?}"))?;

        let cbd = D3D11_BUFFER_DESC {
            ByteWidth: 32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut cb: Option<ID3D11Buffer> = None;
        unsafe { device.CreateBuffer(&cbd, None, Some(&mut cb)) }
            .map_err(|e| format!("CreateBuffer (cursor cb): {e:?}"))?;

        Ok(Self {
            device: device.clone(),
            vs: vs.ok_or("CreateVertexShader returned None")?,
            ps: ps.ok_or("CreatePixelShader returned None")?,
            blend: blend.ok_or("CreateBlendState returned None")?,
            sampler: sampler.ok_or("CreateSamplerState returned None")?,
            cb: cb.ok_or("CreateBuffer returned None")?,
            shape: None,
        })
    }

    /// Upload a (new) pointer shape. Shapes change rarely (hover over a
    /// link, text field…), so a fresh immutable-ish texture per change
    /// is fine.
    pub fn set_shape(&mut self, image: &CursorImage) -> Result<(), String> {
        self.shape = None;
        let w = image.width as u32;
        let h = image.visual_height as u32;
        if w == 0 || h == 0 {
            return Ok(());
        }
        let bgra = cursor_to_bgra(image);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: bgra.as_ptr() as *const _,
            SysMemPitch: w * 4,
            SysMemSlicePitch: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }
            .map_err(|e| format!("CreateTexture2D (cursor): {e:?}"))?;
        let tex = tex.ok_or("CreateTexture2D (cursor) returned None")?;
        let mut srv: Option<ID3D11ShaderResourceView> = None;
        unsafe { self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)) }
            .map_err(|e| format!("CreateShaderResourceView (cursor): {e:?}"))?;
        self.shape = Some((srv.ok_or("CreateShaderResourceView returned None")?, w, h));
        Ok(())
    }

    /// Blend the current shape onto `target` (a BGRA render-target
    /// texture of `frame_w`×`frame_h`) at pixel position (x, y). Queues
    /// GPU work only — never waits.
    pub fn draw(
        &self,
        context: &ID3D11DeviceContext,
        target: &ID3D11Texture2D,
        frame_w: u32,
        frame_h: u32,
        x: i32,
        y: i32,
    ) -> Result<(), String> {
        let Some((srv, w, h)) = self.shape.as_ref() else {
            return Ok(());
        };
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        unsafe { self.device.CreateRenderTargetView(target, None, Some(&mut rtv)) }
            .map_err(|e| format!("CreateRenderTargetView: {e:?}"))?;
        let rtv = rtv.ok_or("CreateRenderTargetView returned None")?;

        let cb_data: [f32; 8] = [
            x as f32,
            y as f32,
            *w as f32,
            *h as f32,
            frame_w as f32,
            frame_h as f32,
            0.0,
            0.0,
        ];
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: frame_w as f32,
            Height: frame_h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        unsafe {
            context.UpdateSubresource(&self.cb, 0, None, cb_data.as_ptr() as *const _, 0, 0);
            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            context.RSSetViewports(Some(&[viewport]));
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.VSSetShader(&self.vs, None);
            context.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            context.PSSetShader(&self.ps, None);
            context.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
            context.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            context.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            context.Draw(4, 0);
            // Leave nothing bound to the ring texture — it's about to be
            // read by the encoder's video processor.
            context.OMSetRenderTargets(None, None);
            context.PSSetShaderResources(0, Some(&[None]));
        }
        Ok(())
    }
}
