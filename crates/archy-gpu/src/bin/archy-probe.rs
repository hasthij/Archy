//! `archy-probe`: prints what Archy can record with on this PC.

use archy_gpu::probe::{probe, Adapter};

fn main() {
    let adapters = match probe() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("probe failed: {e}");
            std::process::exit(1);
        }
    };
    if adapters.is_empty() {
        println!("No hardware GPU found. Archy would fall back to CPU encoding.");
        return;
    }
    for a in &adapters {
        print_adapter(a);
    }
}

fn print_adapter(a: &Adapter) {
    let mb = |b: usize| b / (1024 * 1024);
    println!(
        "[{}] {} ({}, {})",
        a.index,
        a.name,
        a.vendor(),
        if a.integrated { "integrated" } else { "dedicated" }
    );
    println!(
        "    VRAM {} MB dedicated, {} MB shared",
        mb(a.dedicated_vram),
        mb(a.shared_memory)
    );
    if a.displays.is_empty() {
        println!("    Displays: none (renders for another GPU's screen)");
    }
    for d in &a.displays {
        println!("    Display {}  {}x{}", d.device_name, d.width, d.height);
    }
    if a.encoders.is_empty() {
        println!("    Hardware encoders: none");
    }
    for e in &a.encoders {
        println!("    Encoder {:<5} {}", e.codec.to_string(), e.name);
    }
    match a.best_codec() {
        Some(c) => println!("    -> Best recording format: {c}"),
        None => println!("    -> Cannot record on this GPU"),
    }
    println!();
}
