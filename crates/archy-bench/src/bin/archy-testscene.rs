//! `archy-testscene`: a stand-in "game" for repeatable recording benchmarks.
//!
//! Renders an animated, detailed scene (every frame differs, like a game) into
//! a flip-model swap chain with an adjustable GPU load, and reports its own
//! average FPS and 1% low when it exits:
//!
//!   archy-testscene --seconds 20 --load 40 --fps-cap 0
//!   -> RESULT avg_fps=143.2 low1_fps=121.7 frames=2864

use std::time::{Duration, Instant};

use windows::core::{s, w, Result, PCSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D_DRIVER_TYPE_HARDWARE,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const TITLE: windows::core::PCWSTR = w!("Archy Test Scene");

const HLSL: &str = r#"
cbuffer Params : register(b0) { float time; float load; float2 size; };
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs(uint id : SV_VertexID) {
    V o; o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1); return o;
}
float hash(float2 p) { return frac(sin(dot(p, float2(12.9898, 78.233))) * 43758.5453); }
float4 ps(V i) : SV_Target {
    float2 p = i.uv * float2(size.x / size.y, 1) * 6;
    // Moving bands + fine per-pixel detail so the encoder sees game-like motion and texture.
    float3 c = 0.5 + 0.5 * cos(time * 1.3 + p.xyx * float3(1.0, 1.7, 2.3) + float3(0, 2, 4));
    c *= 0.85 + 0.15 * hash(floor(i.pos.xy / 2) + floor(time * 30));
    float2 q = p + float2(time * 0.8, sin(time) * 0.5);
    c = lerp(c, float3(0.95, 0.9, 0.2), step(length(frac(q) - 0.5), 0.12));
    // Adjustable shader cost to make the scene GPU-bound like a real game.
    float acc = 0;
    for (int k = 0; k < (int)load; k++) acc += sin(acc + p.x * k + time) * 0.0001;
    return float4(c + acc, 1);
}
"#;

struct Args {
    seconds: f64,
    load: f32,
    fps_cap: u32,
    width: u32,
    height: u32,
    warmup: f64,
}

fn parse_args() -> std::result::Result<Args, String> {
    let mut a = Args {
        seconds: 20.0,
        load: 0.0,
        fps_cap: 0,
        width: 1600,
        height: 900,
        warmup: 1.0,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let v = it.next().ok_or(format!("{flag} needs a value"))?;
        let num = |s: &str| s.parse::<f64>().map_err(|_| format!("bad number: {s}"));
        match flag.as_str() {
            "--seconds" => a.seconds = num(&v)?,
            "--load" => a.load = num(&v)? as f32,
            "--fps-cap" => a.fps_cap = num(&v)? as u32,
            "--width" => a.width = num(&v)? as u32,
            "--height" => a.height = num(&v)? as u32,
            "--warmup" => a.warmup = num(&v)?,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(a)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    unsafe {
        D3DCompile(
            HLSL.as_ptr() as *const _,
            HLSL.len(),
            None,
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            None,
        )?;
    }
    Ok(code.expect("no bytecode"))
}

fn bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

#[repr(C)]
struct Params {
    time: f32,
    load: f32,
    size: [f32; 2],
}

fn main() -> Result<()> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\nusage: archy-testscene [--seconds N] [--load N] [--fps-cap N] [--width W --height H] [--warmup S]");
            std::process::exit(2);
        }
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let instance: HMODULE = unsafe { GetModuleHandleW(None)? };
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("ArchyTestScene"),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("ArchyTestScene"),
            TITLE,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            40,
            40,
            args.width as i32,
            args.height as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };

    let (mut device, mut context) = (None, None);
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device: ID3D11Device = device.expect("no device");
    let context: ID3D11DeviceContext = context.expect("no context");
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1()? };
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..Default::default()
    };
    let swap: IDXGISwapChain1 =
        unsafe { factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)? };
    let back: ID3D11Texture2D = unsafe { swap.GetBuffer(0)? };
    let mut tdesc = D3D11_TEXTURE2D_DESC::default();
    unsafe { back.GetDesc(&mut tdesc) };
    let mut rtv = None;
    unsafe { device.CreateRenderTargetView(&back, None, Some(&mut rtv))? };
    drop(back);

    let (vs_code, ps_code) = (
        compile(s!("vs"), s!("vs_4_0"))?,
        compile(s!("ps"), s!("ps_4_0"))?,
    );
    let (mut vs, mut ps, mut cb) = (None, None, None);
    unsafe {
        device.CreateVertexShader(bytes(&vs_code), None, Some(&mut vs))?;
        device.CreatePixelShader(bytes(&ps_code), None, Some(&mut ps))?;
        let cbd = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        device.CreateBuffer(&cbd, None, Some(&mut cb))?;
    }
    let cb = cb.expect("no constant buffer");

    let start = Instant::now();
    let mut last = start;
    let mut frame_times: Vec<f64> = Vec::with_capacity(20_000);
    let cap = (args.fps_cap > 0).then(|| Duration::from_secs_f64(1.0 / args.fps_cap as f64));
    let mut next_due = start;
    'frames: while start.elapsed().as_secs_f64() < args.seconds {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                break 'frames;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let params = Params {
            time: start.elapsed().as_secs_f32(),
            load: args.load,
            size: [tdesc.Width as f32, tdesc.Height as f32],
        };
        unsafe {
            context.UpdateSubresource(&cb, 0, None, &params as *const _ as *const _, 0, 0);
            context.OMSetRenderTargets(Some(&[rtv.clone()]), None);
            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                Width: tdesc.Width as f32,
                Height: tdesc.Height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]));
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(vs.as_ref(), None);
            context.PSSetShader(ps.as_ref(), None);
            context.PSSetConstantBuffers(0, Some(&[Some(cb.clone())]));
            context.Draw(3, 0);
            let _ = swap.Present(0, DXGI_PRESENT(0));
        }
        if let Some(cap) = cap {
            next_due += cap;
            let now = Instant::now();
            if next_due > now {
                std::thread::sleep(next_due - now);
            } else {
                next_due = now;
            }
        }
        let now = Instant::now();
        // Skip warm-up: window creation, shader compile, and (in benchmarks) recorder start-up.
        if start.elapsed().as_secs_f64() > args.warmup {
            frame_times.push((now - last).as_secs_f64());
        }
        last = now;
    }

    let total: f64 = frame_times.iter().sum();
    let avg = frame_times.len() as f64 / total.max(1e-9);
    let mut sorted = frame_times.clone();
    sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
    // 1% low: average FPS over the slowest 1% of frames.
    let worst = &sorted[..(sorted.len() / 100).max(1)];
    let low1 = worst.len() as f64 / worst.iter().sum::<f64>().max(1e-9);
    println!(
        "RESULT avg_fps={avg:.1} low1_fps={low1:.1} frames={}",
        frame_times.len()
    );
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
