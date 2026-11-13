//! Discord Rich Presence over Discord's local named pipe. Nothing leaves the PC.
//!
//! One idle thread; the app only sends it a message when the wanted status
//! changes, so the cost while gaming is zero. Discord not running, closing, or
//! restarting is fine: a failed write drops the pipe and it reconnects later.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;



const CLIENT_ID: &str = "1570755301219504220";

/// What the status should say.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Want {
    /// Status off (setting off): clear it and let go of the pipe.
    Off,
    /// Archy is running, no game in front.
    Idle,
    /// Archy is clipping this game.
    Game(String),
}

pub struct Presence {
    tx: Sender<Want>,
    last: Want,
}

impl Presence {
    pub fn new() -> Self {
        let (tx, rx) = channel::<Want>();
        let _ = std::thread::Builder::new().name("archy-discord".into()).spawn(move || {
            let mut want = Want::Off;
            let mut pipe: Option<File> = None;
            let mut sent: Option<Want> = None;
            loop {
                // Sleep until the status changes; while Discord is unreachable retry every 15 s.
                let retry = want != Want::Off && sent.as_ref() != Some(&want);
                let msg = if retry { rx.recv_timeout(Duration::from_secs(15)) } else { rx.recv().map_err(|_| RecvTimeoutError::Disconnected) };
                match msg {
                    Ok(w) => want = w,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if sent.as_ref() == Some(&want) { continue; }
                if want == Want::Off {
                    if let Some(p) = pipe.as_mut() { let _ = set_activity(p, None); }
                    pipe = None;
                    sent = Some(Want::Off);
                    continue;
                }
                if CLIENT_ID.is_empty() { continue; }
                if pipe.is_none() { pipe = connect(); }
                let Some(p) = pipe.as_mut() else { continue };
                let (details, state) = match &want { Want::Game(g) => (format!("Clipping {g} with Archy"), "Ready to clip"), _ => ("Archy".to_string(), "Ready to clip") };
                if set_activity(p, Some((details.as_str(), state))).is_ok() { sent = Some(want.clone()); }
                else { pipe = None; sent = None; }
            }
        });
        Presence { tx, last: Want::Off }
    }

    /// Cheap to call every tick: only a change reaches the thread.
    pub fn set(&mut self, want: Want) {
        if self.last != want {
            self.last = want.clone();
            let _ = self.tx.send(want);
        }
    }
}

fn frame(op: u32, json: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + json.len());
    b.extend_from_slice(&op.to_le_bytes());
    b.extend_from_slice(&(json.len() as u32).to_le_bytes());
    b.extend_from_slice(json.as_bytes());
    b
}

fn read_frame(p: &mut File) -> std::io::Result<()> {
    let mut head = [0u8; 8];
    p.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let mut body = vec![0u8; len.min(64 * 1024)];
    p.read_exact(&mut body)
}

fn connect() -> Option<File> {
    for i in 0..10 {
        let Ok(mut p) = OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) else { continue };
        let hello = serde_json::json!({ "v": 1, "client_id": CLIENT_ID }).to_string();
        if p.write_all(&frame(0, &hello)).is_ok() && read_frame(&mut p).is_ok() { return Some(p); }
    }
    None
}

fn set_activity(p: &mut File, text: Option<(&str, &str)>) -> std::io::Result<()> {
    let activity = text.map(|(d, st)| serde_json::json!({ "details": d, "state": st, "assets": { "large_image": "archy", "large_text": "Archy" } }));
    let msg = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": format!("{:?}", std::time::Instant::now()),
    });
    p.write_all(&frame(1, &msg.to_string()))?;
    read_frame(p)
}
