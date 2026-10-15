//! On-device keyword spotting for voice clipping ("Archy, clip that").
//!
//! Uses sherpa-onnx's streaming keyword spotter (a 3.3M-parameter zipformer,
//! Apache-2.0), loaded at runtime from Archy's `voice` folder, which the
//! installer downloads. It listens to the microphone Archy records (not
//! Windows' default device), is built to pick a short phrase out of noise, and
//! never sends audio anywhere.

mod bpe;

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

pub use bpe::Vocab;

/// Folder name of the keyword model inside the voice folder.
pub const MODEL_DIR: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
/// Sample rate the model expects.
pub const SAMPLE_RATE: u32 = 16_000;

// ---- C API (sherpa-onnx v1.13.8 c-api.h) ----

#[repr(C)]
struct TransducerConfig {
    encoder: *const c_char,
    decoder: *const c_char,
    joiner: *const c_char,
}

#[repr(C)]
struct OnlineModelConfig {
    transducer: TransducerConfig,
    paraformer: [*const c_char; 2],
    zipformer2_ctc: *const c_char,
    tokens: *const c_char,
    num_threads: i32,
    provider: *const c_char,
    debug: i32,
    model_type: *const c_char,
    modeling_unit: *const c_char,
    bpe_vocab: *const c_char,
    tokens_buf: *const c_char,
    tokens_buf_size: i32,
    nemo_ctc: *const c_char,
    t_one_ctc: *const c_char,
}

#[repr(C)]
struct FeatureConfig {
    sample_rate: i32,
    feature_dim: i32,
}

#[repr(C)]
struct KeywordSpotterConfig {
    feat_config: FeatureConfig,
    model_config: OnlineModelConfig,
    max_active_paths: i32,
    num_trailing_blanks: i32,
    keywords_score: f32,
    keywords_threshold: f32,
    keywords_file: *const c_char,
    keywords_buf: *const c_char,
    keywords_buf_size: i32,
}

#[repr(C)]
struct KeywordResult {
    keyword: *const c_char,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    count: i32,
    timestamps: *mut f32,
    start_time: f32,
    json: *const c_char,
}

type Handle = *const c_void;

struct Api {
    lib: HMODULE,
    create: unsafe extern "C" fn(*const KeywordSpotterConfig) -> Handle,
    destroy: unsafe extern "C" fn(Handle),
    create_stream: unsafe extern "C" fn(Handle) -> Handle,
    destroy_stream: unsafe extern "C" fn(Handle),
    accept: unsafe extern "C" fn(Handle, i32, *const f32, i32),
    ready: unsafe extern "C" fn(Handle, Handle) -> i32,
    decode: unsafe extern "C" fn(Handle, Handle),
    result: unsafe extern "C" fn(Handle, Handle) -> *const KeywordResult,
    destroy_result: unsafe extern "C" fn(*const KeywordResult),
    reset: unsafe extern "C" fn(Handle, Handle),
}

impl Api {
    #[allow(clippy::missing_transmute_annotations)] // each target type is its field's
    fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("sherpa-onnx-c-api.dll");
        // Altered search path: onnxruntime.dll is found next to it, not elsewhere on the system.
        let lib = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
            .map_err(|e| format!("can't load {}: {}", path.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                match unsafe { GetProcAddress(lib, windows::core::s!($name)) } {
                    Some(f) => unsafe { std::mem::transmute(f) },
                    None => return Err(concat!("voice engine lacks ", $name).into()),
                }
            };
        }
        Ok(Self {
            lib,
            create: sym!("SherpaOnnxCreateKeywordSpotter"),
            destroy: sym!("SherpaOnnxDestroyKeywordSpotter"),
            create_stream: sym!("SherpaOnnxCreateKeywordStream"),
            destroy_stream: sym!("SherpaOnnxDestroyOnlineStream"),
            accept: sym!("SherpaOnnxOnlineStreamAcceptWaveform"),
            ready: sym!("SherpaOnnxIsKeywordStreamReady"),
            decode: sym!("SherpaOnnxDecodeKeywordStream"),
            result: sym!("SherpaOnnxGetKeywordResult"),
            destroy_result: sym!("SherpaOnnxDestroyKeywordResult"),
            reset: sym!("SherpaOnnxResetKeywordStream"),
        })
    }
}

/// Where the voice engine lives: `<exe dir>\voice` once installed, or the dev
/// copy under `tools\voice` when running from the project.
pub fn find_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut candidates = vec![exe.parent()?.join("voice")];
    for up in exe.ancestors().skip(1).take(5) {
        candidates.push(up.join("tools").join("voice"));
    }
    candidates.into_iter().find(|d| d.join("sherpa-onnx-c-api.dll").exists() && d.join(MODEL_DIR).exists())
}

fn find_file(dir: &Path, prefix: &str, prefer_int8: bool) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            name.starts_with(prefix) && name.ends_with(".onnx")
        })
        .collect();
    files.sort_by_key(|p| p.to_string_lossy().contains(".int8.") != prefer_int8);
    files.into_iter().next()
}

fn cstr(p: &Path) -> CString {
    CString::new(p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The detector's knobs. Calibration (see `calibrate`) picks them per user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Boost for the phrase's tokens while searching; higher = catches more, but
    /// too high starts matching words that merely start the same way.
    pub score: f32,
    /// How many guesses the search keeps alive at once; wider = catches more.
    pub paths: u32,
    /// Minimum confidence to fire; lower = easier to trigger.
    pub threshold: f32,
}

impl Tuning {
    /// Built-in levels. Measured on a real gaming mic (10 phrases, then 90 s of
    /// normal talk): the old 1.5/4 caught 3 of 10; 2.0/8 caught 7, no false alarms.
    pub fn preset(sensitivity: u32) -> Self {
        match sensitivity {
            0..=35 => Self { score: 1.5, paths: 4, threshold: 0.30 },
            36..=65 => Self { score: 2.0, paths: 8, threshold: 0.25 },
            _ => Self { score: 2.0, paths: 16, threshold: 0.15 },
        }
    }

    /// Fine-tuning on top of a level or a calibration: -50 (much stricter) to
    /// +50 (much looser); 0 changes nothing. Moves all three knobs together so
    /// one slider covers the range between "never by accident" and "hears a
    /// whisper".
    pub fn adjusted(self, amount: i32) -> Self {
        let t = amount.clamp(-50, 50) as f32 / 50.0;
        Self {
            score: (self.score + 0.5 * t).clamp(1.0, 3.0),
            paths: ((self.paths as f32) * (1.0 + 0.5 * t)).round().clamp(2.0, 32.0) as u32,
            threshold: (self.threshold - 0.12 * t).clamp(0.05, 0.6),
        }
    }
}

pub struct Spotter {
    api: Api,
    spotter: Handle,
    stream: Handle,
    /// Tag order: keyword `k{i}` belongs to phrase `i`.
    _strings: Vec<CString>,
}

// Used from the voice thread only.
unsafe impl Send for Spotter {}

impl Spotter {
    /// `learned` adds token sequences (from calibration: how this user actually
    /// says phrase `i`) as extra ways to trigger phrase `i`.
    pub fn new(dir: &Path, phrases: &[String], learned: &[(usize, String)], tuning: Tuning) -> Result<Self, String> {
        let model = dir.join(MODEL_DIR);
        let bytes = std::fs::read(model.join("bpe.model")).map_err(|e| format!("voice model: {e}"))?;
        let vocab = Vocab::from_model(&bytes).ok_or("can't read the voice model's vocabulary")?;
        let mut lines = Vec::new();
        for (i, p) in phrases.iter().enumerate() {
            let tokens = vocab.encode(p);
            if tokens.is_empty() {
                continue;
            }
            lines.push(format!("{tokens} @k{i}"));
        }
        for (i, tokens) in learned {
            if *i < phrases.len() && !tokens.trim().is_empty() {
                lines.push(format!("{} @k{i}", tokens.trim()));
            }
        }
        if lines.is_empty() {
            return Err("no voice phrases".into());
        }
        let api = Api::load(dir)?;
        let encoder = find_file(&model, "encoder", false).ok_or("voice model encoder missing")?;
        let decoder = find_file(&model, "decoder", false).ok_or("voice model decoder missing")?;
        let joiner = find_file(&model, "joiner", true).ok_or("voice model joiner missing")?;
        let strings = vec![
            cstr(&encoder),
            cstr(&decoder),
            cstr(&joiner),
            cstr(&model.join("tokens.txt")),
            CString::new("cpu").unwrap_or_default(),
            CString::new(lines.join("\n")).unwrap_or_default(),
        ];
        let null = std::ptr::null();
        let config = KeywordSpotterConfig {
            feat_config: FeatureConfig { sample_rate: SAMPLE_RATE as i32, feature_dim: 80 },
            model_config: OnlineModelConfig {
                transducer: TransducerConfig { encoder: strings[0].as_ptr(), decoder: strings[1].as_ptr(), joiner: strings[2].as_ptr() },
                paraformer: [null; 2],
                zipformer2_ctc: null,
                tokens: strings[3].as_ptr(),
                // One thread: the model is tiny, and it stays off the game's cores.
                num_threads: 1,
                provider: strings[4].as_ptr(),
                debug: 0,
                model_type: null,
                modeling_unit: null,
                bpe_vocab: null,
                tokens_buf: null,
                tokens_buf_size: 0,
                nemo_ctc: null,
                t_one_ctc: null,
            },
            max_active_paths: tuning.paths.clamp(1, 32) as i32,
            num_trailing_blanks: 1,
            keywords_score: tuning.score,
            keywords_threshold: tuning.threshold.clamp(0.01, 0.9),
            keywords_file: null,
            keywords_buf: strings[5].as_ptr(),
            keywords_buf_size: strings[5].as_bytes().len() as i32,
        };
        let spotter = unsafe { (api.create)(&config) };
        if spotter.is_null() {
            return Err("the voice engine couldn't start (check the model files)".into());
        }
        let stream = unsafe { (api.create_stream)(spotter) };
        if stream.is_null() {
            unsafe { (api.destroy)(spotter) };
            return Err("the voice engine couldn't open a stream".into());
        }
        Ok(Self { api, spotter, stream, _strings: strings })
    }

    /// Feeds 16 kHz mono samples (-1..1); returns the phrase index when one is spoken.
    pub fn accept(&mut self, samples: &[f32]) -> Option<usize> {
        let mut found = None;
        unsafe {
            (self.api.accept)(self.stream, SAMPLE_RATE as i32, samples.as_ptr(), samples.len() as i32);
            while (self.api.ready)(self.spotter, self.stream) != 0 {
                (self.api.decode)(self.spotter, self.stream);
                let r = (self.api.result)(self.spotter, self.stream);
                if r.is_null() {
                    continue;
                }
                let keyword = if (*r).keyword.is_null() { "" } else { CStr::from_ptr((*r).keyword).to_str().unwrap_or("") };
                if let Some(i) = keyword.strip_prefix('k').and_then(|n| n.parse::<usize>().ok()) {
                    found = Some(i);
                    // Start fresh so one utterance triggers once.
                    (self.api.reset)(self.spotter, self.stream);
                }
                (self.api.destroy_result)(r);
            }
        }
        found
    }
}

impl Drop for Spotter {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy_stream)(self.stream);
            (self.api.destroy)(self.spotter);
            let _ = FreeLibrary(self.api.lib);
        }
    }
}

// ---- Free transcription with the same model (to learn how a user says a phrase) ----

#[repr(C)]
struct CtcFstDecoderConfig {
    graph: *const c_char,
    max_active: i32,
}

#[repr(C)]
struct HomophoneReplacerConfig {
    dict_dir: *const c_char,
    lexicon: *const c_char,
    rule_fsts: *const c_char,
}

#[repr(C)]
struct OnlineRecognizerConfig {
    feat_config: FeatureConfig,
    model_config: OnlineModelConfig,
    decoding_method: *const c_char,
    max_active_paths: i32,
    enable_endpoint: i32,
    rule1_min_trailing_silence: f32,
    rule2_min_trailing_silence: f32,
    rule3_min_utterance_length: f32,
    hotwords_file: *const c_char,
    hotwords_score: f32,
    ctc_fst_decoder_config: CtcFstDecoderConfig,
    rule_fsts: *const c_char,
    rule_fars: *const c_char,
    blank_penalty: f32,
    hotwords_buf: *const c_char,
    hotwords_buf_size: i32,
    hr: HomophoneReplacerConfig,
}

#[repr(C)]
struct RecognizerResult {
    text: *const c_char,
    tokens: *const c_char,
    tokens_arr: *const *const c_char,
    timestamps: *mut f32,
    count: i32,
    json: *const c_char,
}

/// The model tokens heard in 16 kHz mono `audio` (e.g. ["▁AR", "CH", "ER", ...]):
/// what this user's way of saying something sounds like to the model.
pub fn transcribe(dir: &Path, segments: &[&[f32]]) -> Result<Vec<Vec<String>>, String> {
    let model = dir.join(MODEL_DIR);
    let path = dir.join("sherpa-onnx-c-api.dll");
    let lib = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map_err(|e| format!("can't load {}: {}", path.display(), e.message()))?;
    type H = *const c_void;
    macro_rules! sym {
        ($name:literal, $t:ty) => {
            match unsafe { GetProcAddress(lib, windows::core::s!($name)) } {
                Some(f) => unsafe { std::mem::transmute::<_, $t>(f) },
                None => return Err(concat!("voice engine lacks ", $name).into()),
            }
        };
    }
    let create = sym!("SherpaOnnxCreateOnlineRecognizer", unsafe extern "C" fn(*const OnlineRecognizerConfig) -> H);
    let destroy = sym!("SherpaOnnxDestroyOnlineRecognizer", unsafe extern "C" fn(H));
    let create_stream = sym!("SherpaOnnxCreateOnlineStream", unsafe extern "C" fn(H) -> H);
    let destroy_stream = sym!("SherpaOnnxDestroyOnlineStream", unsafe extern "C" fn(H));
    let accept = sym!("SherpaOnnxOnlineStreamAcceptWaveform", unsafe extern "C" fn(H, i32, *const f32, i32));
    let finished = sym!("SherpaOnnxOnlineStreamInputFinished", unsafe extern "C" fn(H));
    let ready = sym!("SherpaOnnxIsOnlineStreamReady", unsafe extern "C" fn(H, H) -> i32);
    let decode = sym!("SherpaOnnxDecodeOnlineStream", unsafe extern "C" fn(H, H));
    let result = sym!("SherpaOnnxGetOnlineStreamResult", unsafe extern "C" fn(H, H) -> *const RecognizerResult);
    let destroy_result = sym!("SherpaOnnxDestroyOnlineRecognizerResult", unsafe extern "C" fn(*const RecognizerResult));

    let encoder = find_file(&model, "encoder", false).ok_or("voice model encoder missing")?;
    let decoder = find_file(&model, "decoder", false).ok_or("voice model decoder missing")?;
    let joiner = find_file(&model, "joiner", true).ok_or("voice model joiner missing")?;
    let strings = [
        cstr(&encoder),
        cstr(&decoder),
        cstr(&joiner),
        cstr(&model.join("tokens.txt")),
        CString::new("cpu").unwrap_or_default(),
        CString::new("modified_beam_search").unwrap_or_default(),
    ];
    let null = std::ptr::null();
    let config = OnlineRecognizerConfig {
        feat_config: FeatureConfig { sample_rate: SAMPLE_RATE as i32, feature_dim: 80 },
        model_config: OnlineModelConfig {
            transducer: TransducerConfig { encoder: strings[0].as_ptr(), decoder: strings[1].as_ptr(), joiner: strings[2].as_ptr() },
            paraformer: [null; 2],
            zipformer2_ctc: null,
            tokens: strings[3].as_ptr(),
            num_threads: 1,
            provider: strings[4].as_ptr(),
            debug: 0,
            model_type: null,
            modeling_unit: null,
            bpe_vocab: null,
            tokens_buf: null,
            tokens_buf_size: 0,
            nemo_ctc: null,
            t_one_ctc: null,
        },
        decoding_method: strings[5].as_ptr(),
        max_active_paths: 4,
        enable_endpoint: 0,
        rule1_min_trailing_silence: 2.4,
        rule2_min_trailing_silence: 1.2,
        rule3_min_utterance_length: 20.0,
        hotwords_file: null,
        hotwords_score: 1.5,
        ctc_fst_decoder_config: CtcFstDecoderConfig { graph: null, max_active: 3000 },
        rule_fsts: null,
        rule_fars: null,
        blank_penalty: 0.0,
        hotwords_buf: null,
        hotwords_buf_size: 0,
        hr: HomophoneReplacerConfig { dict_dir: null, lexicon: null, rule_fsts: null },
    };
    let mut all = Vec::new();
    unsafe {
        let rec = create(&config);
        if rec.is_null() {
            let _ = FreeLibrary(lib);
            return Err("the voice engine couldn't start its transcriber".into());
        }
        for audio in segments {
            let mut tokens = Vec::new();
            let stream = create_stream(rec);
        accept(stream, SAMPLE_RATE as i32, audio.as_ptr(), audio.len() as i32);
        let tail = vec![0f32; SAMPLE_RATE as usize / 2];
        accept(stream, SAMPLE_RATE as i32, tail.as_ptr(), tail.len() as i32);
        finished(stream);
        while ready(rec, stream) != 0 {
            decode(rec, stream);
        }
        let r = result(rec, stream);
        if !r.is_null() && !(*r).tokens_arr.is_null() {
            for i in 0..(*r).count.max(0) as usize {
                let t = *(*r).tokens_arr.add(i);
                if !t.is_null() {
                    // Word starts come back as a leading space; keywords spell it "▁".
                    let t = CStr::from_ptr(t).to_string_lossy();
                    tokens.push(match t.strip_prefix(' ') {
                        Some(rest) => format!("\u{2581}{rest}"),
                        None => t.into_owned(),
                    });
                }
            }
        }
        if !r.is_null() {
            destroy_result(r);
        }
            destroy_stream(stream);
            all.push(tokens);
        }
        destroy(rec);
        let _ = FreeLibrary(lib);
    }
    Ok(all)
}

/// The part of `heard` that lines up best with `expected` (both token lists),
/// or None when nothing resembles it. Start and end in `heard` are free, so
/// words said before or after the phrase are dropped.
fn best_match(expected: &[String], heard: &[String]) -> Option<Vec<String>> {
    let (m, n) = (expected.len(), heard.len());
    if m == 0 || n == 0 {
        return None;
    }
    let bare = |t: &str| t.trim_start_matches('\u{2581}').to_string();
    let sub = |a: &str, b: &str| {
        if a == b {
            0.0
        } else if bare(a) == bare(b) {
            0.25
        } else {
            1.0
        }
    };
    // cost[i][j], start[i][j]: expected[..i] aligned to heard[start..j].
    let mut cost = vec![vec![0f32; n + 1]; m + 1];
    let mut start = vec![vec![0usize; n + 1]; m + 1];
    for (j, s) in start[0].iter_mut().enumerate() {
        *s = j;
    }
    for i in 1..=m {
        cost[i][0] = i as f32;
        for j in 1..=n {
            let diag = cost[i - 1][j - 1] + sub(&expected[i - 1], &heard[j - 1]);
            let up = cost[i - 1][j] + 1.0; // an expected token wasn't heard
            let left = cost[i][j - 1] + 1.0; // an extra token was heard
            let (c, s) = if diag <= up && diag <= left {
                (diag, start[i - 1][j - 1])
            } else if up <= left {
                (up, start[i - 1][j])
            } else {
                (left, start[i][j - 1])
            };
            cost[i][j] = c;
            start[i][j] = s;
        }
    }
    let best = (1..=n).map(|j| cost[m][j]).fold(f32::INFINITY, f32::min);
    // Among equally good ends, the latest: a whole take beats a fragment of it.
    let end = (1..=n).rev().find(|&j| cost[m][j] <= best)?;
    let span: Vec<String> = heard[start[m][end]..end].to_vec();
    // Too different to be the phrase, or too short to be safe.
    (best <= 0.75 * m as f32 && span.len() >= 4).then_some(span)
}

/// What a calibration found.
#[derive(Clone, Debug)]
pub struct Calibration {
    pub tuning: Tuning,
    /// Prompts the phrase was caught in.
    pub hits: usize,
    pub prompts: usize,
    /// Times it fired while the user was just talking.
    pub false_alarms: usize,
    /// How this user says the phrase, as model tokens (extra triggers).
    pub learned: Vec<String>,
}

/// Times (seconds) at which `phrase` is detected in 16 kHz mono `audio`.
fn detections(dir: &Path, phrase: &str, learned: &[String], audio: &[f32], tuning: Tuning) -> Result<Vec<f32>, String> {
    let learned: Vec<(usize, String)> = learned.iter().map(|l| (0, l.clone())).collect();
    let mut spotter = Spotter::new(dir, &[phrase.to_string()], &learned, tuning)?;
    let mut found = Vec::new();
    let chunk = SAMPLE_RATE as usize / 10;
    for (n, block) in audio.chunks(chunk).enumerate() {
        if spotter.accept(block).is_some() {
            found.push((n + 1) as f32 * 0.1);
        }
    }
    if spotter.accept(&vec![0.0; SAMPLE_RATE as usize]).is_some() {
        found.push(audio.len() as f32 / SAMPLE_RATE as f32);
    }
    Ok(found)
}

/// Whether `phrase` is heard anywhere in 16 kHz mono `audio` (a quick check of
/// one calibration take).
pub fn heard_in(dir: &Path, phrase: &str, audio: &[f32], tuning: Tuning) -> Result<bool, String> {
    Ok(!detections(dir, phrase, &[], audio, tuning)?.is_empty())
}

/// Learns how the user says `phrase` from a recording where they said it once
/// after each of `prompts` (seconds), then just talked during `talk` (start,
/// end). Each take is transcribed and the way they said it becomes an extra
/// trigger (so "archa-clipped-that" said in one breath counts). Then tries
/// many tunings, with and without those, and picks the one that catches the
/// most takes, never one that fires while they talk.
pub fn calibrate(
    dir: &Path,
    phrase: &str,
    audio: &[f32],
    prompts: &[f32],
    window: f32,
    talk: (f32, f32),
) -> Result<Calibration, String> {
    let rate = SAMPLE_RATE as f32;
    let segments: Vec<&[f32]> = prompts
        .iter()
        .map(|&p| {
            let a = ((p * rate) as usize).min(audio.len());
            let b = (((p + window) * rate) as usize).min(audio.len());
            &audio[a..b]
        })
        .collect();
    let model = dir.join(MODEL_DIR);
    let vocab = Vocab::from_model(&std::fs::read(model.join("bpe.model")).map_err(|e| e.to_string())?)
        .ok_or("can't read the voice model's vocabulary")?;
    let expected: Vec<String> = vocab.encode(phrase).split(' ').map(str::to_string).collect();
    let mut learned: Vec<String> = Vec::new();
    for heard in transcribe(dir, &segments)? {
        if let Some(span) = best_match(&expected, &heard) {
            let line = span.join(" ");
            if line != expected.join(" ") && !learned.contains(&line) {
                learned.push(line);
            }
        }
    }
    // From cautious to loose. (Each try is a full pass over the recording, so
    // the list stays short: ~2 s each on one core.)
    let grid: Vec<Tuning> = [
        (1.5, 4, 0.30),
        (2.0, 4, 0.25),
        (2.0, 8, 0.25),
        (2.0, 8, 0.15),
        (2.5, 8, 0.20),
        (2.0, 16, 0.15),
        (2.5, 16, 0.15),
        (3.0, 16, 0.10),
    ]
    .into_iter()
    .map(|(score, paths, threshold)| Tuning { score, paths, threshold })
    .collect();
    // A few threads, each with its own one-thread engine.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    // Every tuning both with and without the learned takes (in case they misfire).
    let grid: Vec<(Tuning, Vec<String>)> = grid
        .into_iter()
        .flat_map(|g| {
            let mut v = vec![(g, Vec::new())];
            if !learned.is_empty() {
                v.push((g, learned.clone()));
            }
            v
        })
        .collect();
    let results: Vec<Result<(Tuning, Vec<String>, Vec<f32>), String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let grid = &grid;
                scope.spawn(move || {
                    grid.iter()
                        .skip(t)
                        .step_by(threads)
                        .map(|(g, l)| detections(dir, phrase, l, audio, *g).map(|d| (*g, l.clone(), d)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let mut scored = Vec::new();
    for r in results {
        let (tuning, learned, found) = r?;
        let hits = prompts
            .iter()
            .filter(|&&p| found.iter().any(|&t| t > p && t <= p + window))
            .count();
        let false_alarms = found.iter().filter(|&&t| t > talk.0 && t <= talk.1).count();
        scored.push(Calibration { tuning, hits, prompts: prompts.len(), false_alarms, learned });
    }
    // The settings that caught the most takes without ever firing on the
    // look-alike sentences; among those, a middle one. The strictest one passed
    // the test but missed real phrases in noisy games; the loosest is the most
    // likely to fire by accident. Fewer extra learned triggers win ties.
    let clean: Vec<Calibration> = scored.into_iter().filter(|c| c.false_alarms == 0).collect();
    let best = clean.iter().map(|c| c.hits).max().ok_or("it fired while you were reading the look-alike sentences at every setting; pick a longer or more unusual phrase")?;
    let mut top: Vec<Calibration> = clean.into_iter().filter(|c| c.hits == best).collect();
    top.sort_by(|a, b| caution(&b.tuning).total_cmp(&caution(&a.tuning)).then(a.learned.len().cmp(&b.learned.len())));
    let middle = top.len() / 2;
    Ok(top.swap_remove(middle))
}

/// Silence put before each calibration take, and the least room after it, so
/// every take sits in its own window and the detector has time to answer.
pub const CALIBRATION_LEAD: f32 = 0.3;
pub const CALIBRATION_TAIL: f32 = 1.5;

/// Calibrates from separate takes of the phrase (each recorded while the user
/// held a button) and `talk`, sentences that must not trigger. Every take gets
/// an equal window (the longest take plus room for the detector to answer), so
/// a phrase said late or heard a moment after it ends still counts.
pub fn calibrate_takes(dir: &Path, phrase: &str, takes: &[Vec<f32>], talk: &[f32]) -> Result<Calibration, String> {
    if takes.is_empty() {
        return Err("record the phrase at least once first".into());
    }
    let rate = SAMPLE_RATE as f32;
    let longest = takes.iter().map(Vec::len).max().unwrap_or(0) as f32 / rate;
    let window = (CALIBRATION_LEAD + longest + CALIBRATION_TAIL).max(3.0);
    let mut audio: Vec<f32> = Vec::new();
    let mut prompts = Vec::new();
    for take in takes {
        let start = audio.len();
        prompts.push(start as f32 / rate);
        audio.extend(std::iter::repeat_n(0.0, (CALIBRATION_LEAD * rate) as usize));
        audio.extend(take);
        audio.resize(start + (window * rate) as usize, 0.0);
    }
    let talk_from = audio.len() as f32 / rate;
    audio.extend(talk);
    audio.extend(std::iter::repeat_n(0.0, (CALIBRATION_TAIL * rate) as usize));
    calibrate(dir, phrase, &audio, &prompts, window, (talk_from, audio.len() as f32 / rate))
}

/// Bigger = less likely to fire by accident.
fn caution(t: &Tuning) -> f32 {
    t.threshold * 10.0 - t.score - t.paths as f32 / 8.0
}

/// 48 kHz interleaved stereo i16 -> 16 kHz mono f32 (average of each 3x2 block:
/// a downmix plus a simple low-pass, plenty for speech).
pub fn to_16k_mono(pcm: &[i16], out: &mut Vec<f32>) {
    out.clear();
    for block in pcm.chunks_exact(6) {
        let sum: i32 = block.iter().map(|&s| s as i32).sum();
        out.push(sum as f32 / (6.0 * 32768.0));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fine_tune_moves_the_right_way() {
        let base = super::Tuning::preset(50);
        assert_eq!(base.adjusted(0), base);
        let looser = base.adjusted(25);
        let stricter = base.adjusted(-25);
        assert!(looser.threshold < base.threshold && base.threshold < stricter.threshold);
        assert!(looser.score > base.score && looser.paths >= base.paths);
        let extreme = base.adjusted(1000);
        assert_eq!(extreme, base.adjusted(50));
        assert!(extreme.threshold >= 0.05 && base.adjusted(-50).paths >= 2);
    }

    #[test]
    fn resample_shape() {
        let mut out = Vec::new();
        super::to_16k_mono(&[16384; 12], &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.5).abs() < 1e-6);
    }
}
