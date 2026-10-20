//! Screenshot hotkey: a full-size PNG of what the recorder sees.
//!
//! Costs nothing until the key is pressed. Then the capture loop copies one frame
//! into a CPU-readable texture (a single GPU copy), reads it back on a later pass
//! without ever waiting for the GPU, and a background thread writes the PNG. The
//! game is never touched.

use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};

/// A screenshot someone asked for, and where its result goes.
pub struct Request {
    pub path: PathBuf,
    pub reply: SyncSender<std::result::Result<PathBuf, String>>,
    pub asked: Instant,
}

/// How the copied picture is stored.
#[derive(Clone, Copy)]
enum Layout {
    /// The captured frame itself (full size, BGRA).
    Bgra,
    /// The last recorded frame (clip size, NV12): used when the screen is still
    /// and no new frame comes.
    Nv12,
}

/// A copy on its way back from the GPU.
pub struct Pending {
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    layout: Layout,
    request: Request,
}

/// Give up on a picture the GPU hasn't handed back by then.
const READBACK_LIMIT: Duration = Duration::from_secs(2);

fn staging_like(device: &ID3D11Device, desc: &D3D11_TEXTURE2D_DESC, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..*desc
    };
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    t.ok_or_else(|| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, "no staging texture"))
}

/// Starts copying a captured frame (`width` x `height` of `texture`, BGRA).
pub fn begin_bgra(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    width: u32,
    height: u32,
    request: Request,
) -> std::result::Result<Pending, Request> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    let (w, h) = (width.min(desc.Width), height.min(desc.Height));
    let Ok(staging) = staging_like(device, &desc, w, h) else { return Err(request) };
    let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
    unsafe { ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, 0, Some(&region)) };
    Ok(Pending { staging, width: w, height: h, layout: Layout::Bgra, request })
}

/// Starts copying the last recorded frame (NV12, clip size).
pub fn begin_nv12(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    request: Request,
) -> std::result::Result<Pending, Request> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    let Ok(staging) = staging_like(device, &desc, desc.Width, desc.Height) else { return Err(request) };
    unsafe { ctx.CopyResource(&staging, texture) };
    Ok(Pending { staging, width: desc.Width, height: desc.Height, layout: Layout::Nv12, request })
}

/// Reads the copy back if the GPU has finished it (never waits). `Some` = not yet.
pub fn poll(ctx: &ID3D11DeviceContext, p: Pending) -> Option<Pending> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let r = unsafe { ctx.Map(&p.staging, 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut mapped)) };
    if r.is_err() {
        if p.request.asked.elapsed() > READBACK_LIMIT {
            let _ = p.request.reply.try_send(Err("the GPU didn't hand the picture back in time".into()));
            return None;
        }
        return Some(p); // still copying
    }
    let rows = match p.layout {
        Layout::Bgra => p.height,
        Layout::Nv12 => p.height + p.height / 2,
    };
    let pitch = mapped.RowPitch as usize;
    let bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * rows as usize) }.to_vec();
    unsafe { ctx.Unmap(&p.staging, 0) };
    let Pending { width, height, layout, request, .. } = p;
    // Converting and compressing take ~50-150 ms of CPU: never on the capture thread.
    std::thread::spawn(move || {
        let rgb = match layout {
            Layout::Bgra => bgra_to_rgb(&bytes, pitch, width, height),
            Layout::Nv12 => nv12_to_rgb(&bytes, pitch, width, height),
        };
        let result = write_png(&request.path, &rgb, width, height).map(|()| request.path.clone());
        let _ = request.reply.try_send(result);
    });
    None
}

fn bgra_to_rgb(src: &[u8], pitch: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h as usize {
        let row = &src[y * pitch..y * pitch + w as usize * 4];
        for px in row.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    out
}

/// BT.709 limited range (what the converter writes) to RGB.
fn nv12_to_rgb(src: &[u8], pitch: usize, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let uv = &src[pitch * h..];
    let mut out = Vec::with_capacity(w * h * 3);
    let clamp = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    for y in 0..h {
        for x in 0..w {
            let luma = (src[y * pitch + x] as f32 - 16.0) * 1.164;
            let i = (y / 2) * pitch + (x / 2) * 2;
            let (cb, cr) = (uv[i] as f32 - 128.0, uv[i + 1] as f32 - 128.0);
            out.push(clamp(luma + 1.793 * cr));
            out.push(clamp(luma - 0.213 * cb - 0.533 * cr));
            out.push(clamp(luma + 2.112 * cb));
        }
    }
    out
}

/// Writes an RGB picture as PNG (written under a temporary name, then renamed,
/// so the library never shows a half-written file).
pub fn write_png(path: &std::path::Path, rgb: &[u8], w: u32, h: u32) -> std::result::Result<(), String> {
    let tmp = path.with_extension("partial.png");
    let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    // Fast compression: a screenshot should land in well under a second.
    enc.set_compression(png::Compression::Fast);
    let written = enc
        .write_header()
        .and_then(|mut wr| wr.write_image_data(rgb))
        .map_err(|e| e.to_string());
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_survive_conversion_and_png() {
        // A 2x2 BGRA picture with padding at the end of each row.
        let pitch = 16;
        let mut bgra = vec![0u8; pitch * 2];
        let px = [[0u8, 0, 255, 255], [0, 255, 0, 255], [255, 0, 0, 255], [255, 255, 255, 255]]; // red, green, blue, white
        for (i, p) in px.iter().enumerate() {
            let (x, y) = (i % 2, i / 2);
            bgra[y * pitch + x * 4..y * pitch + x * 4 + 4].copy_from_slice(p);
        }
        let rgb = bgra_to_rgb(&bgra, pitch, 2, 2);
        assert_eq!(rgb, vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
        let path = std::env::temp_dir().join("archy-shot-test.png");
        write_png(&path, &rgb, 2, 2).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn nv12_grey_and_white() {
        // 2x2 NV12: Y=235 (white) and Y=16 (black), neutral chroma.
        let pitch = 4;
        let mut nv12 = vec![0u8; pitch * 3];
        nv12[0] = 235;
        nv12[1] = 16;
        nv12[pitch] = 235;
        nv12[pitch + 1] = 16;
        nv12[pitch * 2] = 128;
        nv12[pitch * 2 + 1] = 128;
        let rgb = nv12_to_rgb(&nv12, pitch, 2, 2);
        assert_eq!(&rgb[0..3], &[255, 255, 255]);
        assert_eq!(&rgb[3..6], &[0, 0, 0]);
    }
}
