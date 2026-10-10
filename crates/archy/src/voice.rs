//! Voice clipping: "Archy, clip that" saves a clip, no key needed.
//!
//! Uses Windows' built-in speech recognizer restricted to a short phrase list,
//! which runs on this PC (no internet) and is far lighter than free dictation.
//! A recognized phrase is posted to the tray window as a message.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use windows::core::{Ref, Result, HSTRING};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::SpeechRecognition::{
    SpeechContinuousRecognitionCompletedEventArgs,
    SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
    SpeechRecognitionConfidence, SpeechRecognitionListConstraint, SpeechRecognitionResultStatus,
    SpeechRecognizer,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows_collections::IIterable;

pub struct VoiceListener {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl VoiceListener {
    /// Listens for `phrases`; when phrase `i` is heard, posts `message` with
    /// WPARAM = i to `window`. `on_error` gets a readable reason if listening can't start.
    pub fn start(
        phrases: Vec<String>,
        window: isize,
        message: u32,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> Self {
        let (stop, stopped) = channel::<()>();
        let thread = std::thread::Builder::new()
            .name("archy-voice".into())
            .spawn(move || {
                if let Err(e) = listen(&phrases, window, message, stopped) {
                    on_error(explain(&e));
                }
            })
            .ok();
        Self { stop, thread }
    }
}

impl Drop for VoiceListener {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn explain(e: &windows::core::Error) -> String {
    // 0x80045509: speech privacy policy not accepted; 0x80070005: mic blocked.
    match e.code().0 as u32 {
        0x8004_5509 => "Windows speech recognition isn't set up. Turn on Settings > Privacy & security > Speech.".into(),
        0x8007_0005 => "Microphone access is off for desktop apps (Settings > Privacy & security > Microphone).".into(),
        _ => format!("voice clipping couldn't start: {}", e.message()),
    }
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn listen(
    phrases: &[String],
    window: isize,
    message: u32,
    stopped: std::sync::mpsc::Receiver<()>,
) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let recognizer = SpeechRecognizer::new()?;
    let items: Vec<HSTRING> = phrases
        .iter()
        .map(|p| HSTRING::from(normalize(p)))
        .collect();
    let list: IIterable<HSTRING> = items.into();
    let constraint = SpeechRecognitionListConstraint::Create(&list)?;
    recognizer.Constraints()?.Append(&constraint)?;
    let compiled = recognizer.CompileConstraintsAsync()?.join()?;
    if compiled.Status()? != SpeechRecognitionResultStatus::Success {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "phrases could not be compiled",
        ));
    }
    let session: SpeechContinuousRecognitionSession = recognizer.ContinuousRecognitionSession()?;
    // Never stop on its own during long quiet stretches of a game.
    session.SetAutoStopSilenceTimeout(TimeSpan {
        Duration: 365 * 24 * 3600 * 10_000_000,
    })?;

    let wanted: Vec<String> = phrases.iter().map(|p| normalize(p)).collect();
    session.ResultGenerated(&TypedEventHandler::new(
        move |_: Ref<SpeechContinuousRecognitionSession>,
              args: Ref<SpeechContinuousRecognitionResultGeneratedEventArgs>| {
            let Ok(args) = args.ok() else { return Ok(()) };
            let result = args.Result()?;
            let confident = matches!(
                result.Confidence()?,
                SpeechRecognitionConfidence::High | SpeechRecognitionConfidence::Medium
            );
            if confident {
                let heard = normalize(&result.Text()?.to_string());
                if let Some(i) = wanted.iter().position(|w| *w == heard) {
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(window as *mut _)),
                            message,
                            WPARAM(i),
                            LPARAM(0),
                        );
                    }
                }
            }
            Ok(())
        },
    ))?;
    // If Windows ends the session (device change, long silence), start it again,
    // unless Archy is the one stopping it.
    let stopping = Arc::new(AtomicBool::new(false));
    let restart = session.clone();
    let flag = stopping.clone();
    session.Completed(&TypedEventHandler::new(
        move |_: Ref<SpeechContinuousRecognitionSession>,
              _: Ref<SpeechContinuousRecognitionCompletedEventArgs>| {
            if !flag.load(Ordering::Acquire) {
                let _ = restart.StartAsync();
            }
            Ok(())
        },
    ))?;
    session.StartAsync()?.join()?;
    let _ = stopped.recv();
    stopping.store(true, Ordering::Release);
    let _ = session.StopAsync().map(|op| op.join());
    Ok(())
}
