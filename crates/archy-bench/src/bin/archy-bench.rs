//! `archy-bench`: does recording cost the game FPS?
//!
//! Runs the test scene alone, then again while `archy-record` captures it, and
//! compares average FPS and 1% lows. Everything after `--` goes to archy-record.
//!
//!   archy-bench --seconds 15 --load 60 -- --codec hevc --convert shader --eco

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const WARMUP: f64 = 3.0;

struct Scene {
    avg: f64,
    low1: f64,
}

fn parse_scene(out: &str) -> Option<Scene> {
    let line = out.lines().find(|l| l.starts_with("RESULT"))?;
    let field = |k: &str| -> Option<f64> {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(k)?.strip_prefix('=')?.parse().ok())
    };
    Some(Scene {
        avg: field("avg_fps")?,
        low1: field("low1_fps")?,
    })
}

fn bin(name: &str) -> PathBuf {
    let mut p = std::env::current_exe().expect("current exe");
    p.set_file_name(format!("{name}.exe"));
    p
}

fn scene_cmd(seconds: f64, scene_args: &[String]) -> Command {
    let mut c = Command::new(bin("archy-testscene"));
    c.args([
        "--seconds",
        &(seconds + WARMUP).to_string(),
        "--warmup",
        &WARMUP.to_string(),
    ])
    .args(scene_args)
    .stdout(Stdio::piped());
    c
}

fn main() {
    let all: Vec<String> = std::env::args().skip(1).collect();
    let split = all.iter().position(|a| a == "--").unwrap_or(all.len());
    let (mine, record_args) = (&all[..split], all.get(split + 1..).unwrap_or(&[]));
    let mut seconds = 15.0;
    let mut scene_args = Vec::new();
    let mut it = mine.iter();
    while let Some(flag) = it.next() {
        let v = it.next().cloned().unwrap_or_default();
        match flag.as_str() {
            "--seconds" => seconds = v.parse().expect("--seconds needs a number"),
            "--load" | "--fps-cap" | "--width" | "--height" => scene_args.extend([flag.clone(), v]),
            other => {
                eprintln!("unknown flag {other}\nusage: archy-bench [--seconds N] [--load N] [--fps-cap N] -- <archy-record flags>");
                std::process::exit(2);
            }
        }
    }

    println!("[1/2] Test scene alone, {seconds} s...");
    let out = scene_cmd(seconds, &scene_args)
        .output()
        .expect("run test scene");
    let base =
        parse_scene(&String::from_utf8_lossy(&out.stdout)).expect("test scene gave no result");

    println!("[2/2] Test scene while recording, {seconds} s...");
    let scene = scene_cmd(seconds, &scene_args)
        .spawn()
        .expect("run test scene");
    thread::sleep(Duration::from_millis(1500)); // let the window appear
    let out_file = std::env::temp_dir().join("archy-bench.mp4");
    let rec = Command::new(bin("archy-record"))
        .args([
            "--window",
            "Archy Test Scene",
            "--seconds",
            &(seconds + 1.0).ceil().to_string(),
        ])
        .arg("--out")
        .arg(&out_file)
        .args(record_args)
        .stdout(Stdio::piped())
        .spawn()
        .expect("run archy-record");
    let scene_out = scene.wait_with_output().expect("test scene");
    let rec_out = rec.wait_with_output().expect("archy-record");
    let with = parse_scene(&String::from_utf8_lossy(&scene_out.stdout))
        .expect("test scene gave no result");

    let pct = |a: f64, b: f64| (b - a) / a * 100.0;
    println!();
    println!("                 avg FPS   1% low");
    println!("  not recording  {:>7.1}  {:>7.1}", base.avg, base.low1);
    println!("  recording      {:>7.1}  {:>7.1}", with.avg, with.low1);
    println!(
        "  change         {:>+6.1}%  {:>+6.1}%",
        pct(base.avg, with.avg),
        pct(base.low1, with.low1)
    );
    println!();
    for line in String::from_utf8_lossy(&rec_out.stdout).lines() {
        if line.starts_with("Encoder")
            || line.contains("Frames:")
            || line.contains("CPU")
            || line.contains("thread")
        {
            println!("  {}", line.trim());
        }
    }
}
