//! Auto captions: who spoke when (speaker diarization) + what was said (Whisper),
//! both through the same sherpa-onnx DLL the voice feature loads. Offline and local:
//! audio never leaves the PC. Runs only when the user asks, in its own process.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

type H = *const c_void;
type P = *const c_char;

// ---- C API mirrors (sherpa-onnx v1.13.8 c-api.h). Zeroed = "use the default", so only sizes must be exact. ----
#[repr(C)] struct Transducer { encoder: P, decoder: P, joiner: P }
#[repr(C)] struct OneModel { model: P }
#[repr(C)] struct Whisper { encoder: P, decoder: P, language: P, task: P, tail_paddings: i32, token_ts: i32, segment_ts: i32 }
#[repr(C)] struct Lm { model: P, scale: f32 }
#[repr(C)] struct SenseVoice { model: P, language: P, use_itn: i32 }
#[repr(C)] struct Moonshine { a: P, b: P, c: P, d: P, e: P }
#[repr(C)] struct TwoFiles { encoder: P, decoder: P }
#[repr(C)] struct Canary { encoder: P, decoder: P, src: P, tgt: P, use_pnc: i32 }
#[repr(C)] struct FunAsr { a: P, b: P, c: P, d: P, e: P, f: P, max_new_tokens: i32, temperature: f32, top_p: f32, seed: i32, language: P, itn: i32, hotwords: P }
#[repr(C)] struct Qwen3 { a: P, b: P, c: P, d: P, max_total_len: i32, max_new_tokens: i32, temperature: f32, top_p: f32, seed: i32, hotwords: P }
#[repr(C)] struct Cohere { encoder: P, decoder: P, language: P, use_punct: i32, use_itn: i32 }
#[repr(C)]
struct OfflineModel {
    transducer: Transducer, paraformer: OneModel, nemo_ctc: OneModel, whisper: Whisper, tdnn: OneModel,
    tokens: P, num_threads: i32, debug: i32, provider: P, model_type: P, modeling_unit: P, bpe_vocab: P, telespeech_ctc: P,
    sense_voice: SenseVoice, moonshine: Moonshine, fire_red_asr: TwoFiles, dolphin: OneModel, zipformer_ctc: OneModel,
    canary: Canary, wenet_ctc: OneModel, omnilingual: OneModel, medasr: OneModel, funasr_nano: FunAsr,
    fire_red_asr_ctc: OneModel, qwen3_asr: Qwen3, cohere: Cohere,
}
#[repr(C)] struct Feature { sample_rate: i32, feature_dim: i32 }
#[repr(C)] struct Homophone { dict_dir: P, lexicon: P, rule_fsts: P }
#[repr(C)]
struct RecognizerConfig {
    feat: Feature, model: OfflineModel, lm: Lm, decoding_method: P, max_active_paths: i32,
    hotwords_file: P, hotwords_score: f32, rule_fsts: P, rule_fars: P, blank_penalty: f32, hr: Homophone,
}
#[repr(C)] struct RecResult { text: P, timestamps: *const f32, count: i32, tokens: P, tokens_arr: *const P }

#[repr(C)] struct Pyannote { model: P, window_shift_ratio: f32 }
#[repr(C)] struct Segmentation { pyannote: Pyannote, num_threads: i32, debug: i32, provider: P }
#[repr(C)] struct Embedding { model: P, num_threads: i32, debug: i32, provider: P }
#[repr(C)] struct Clustering { num_clusters: i32, threshold: f32, compute_confidence: i32 }
#[repr(C)] struct DiarConfig { segmentation: Segmentation, embedding: Embedding, clustering: Clustering, min_on: f32, min_off: f32 }
#[repr(C)] struct DiarSeg { start: f32, end: f32, speaker: i32, confidence: f32 }

#[repr(C)] struct EmbCfg { model: P, num_threads: i32, debug: i32, provider: P }

type DiarCallback = unsafe extern "C" fn(i32, i32, *mut c_void) -> i32;

struct Api {
    create_rec: unsafe extern "C" fn(*const RecognizerConfig) -> H,
    destroy_rec: unsafe extern "C" fn(H),
    create_stream: unsafe extern "C" fn(H) -> H,
    destroy_stream: unsafe extern "C" fn(H),
    accept: unsafe extern "C" fn(H, i32, *const f32, i32),
    decode: unsafe extern "C" fn(H, H),
    result: unsafe extern "C" fn(H) -> *const RecResult,
    destroy_result: unsafe extern "C" fn(*const RecResult),
    create_diar: unsafe extern "C" fn(*const DiarConfig) -> H,
    destroy_diar: unsafe extern "C" fn(H),
    diar_rate: unsafe extern "C" fn(H) -> i32,
    diar_process: unsafe extern "C" fn(H, *const f32, i32, DiarCallback, *mut c_void) -> H,
    diar_count: unsafe extern "C" fn(H) -> i32,
    diar_sorted: unsafe extern "C" fn(H) -> *const DiarSeg,
    diar_free_segments: unsafe extern "C" fn(*const DiarSeg),
    diar_free_result: unsafe extern "C" fn(H),
    ext_create: unsafe extern "C" fn(*const EmbCfg) -> H,
    ext_destroy: unsafe extern "C" fn(H),
    ext_dim: unsafe extern "C" fn(H) -> i32,
    ext_stream: unsafe extern "C" fn(H) -> H,
    ext_ready: unsafe extern "C" fn(H, H) -> i32,
    ext_compute: unsafe extern "C" fn(H, H) -> *const f32,
    ext_free: unsafe extern "C" fn(*const f32),
    on_accept: unsafe extern "C" fn(H, i32, *const f32, i32),
    on_finish: unsafe extern "C" fn(H),
    on_destroy: unsafe extern "C" fn(H),
}

impl Api {
    #[allow(clippy::missing_transmute_annotations)]
    fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("sherpa-onnx-c-api.dll");
        let lib = unsafe { LoadLibraryExW(&HSTRING::from(path.as_os_str()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
            .map_err(|e| format!("can't load {}: {}", path.display(), e.message()))?;
        macro_rules! sym {
            ($name:literal) => {
                match unsafe { GetProcAddress(lib, windows::core::s!($name)) } {
                    Some(f) => unsafe { std::mem::transmute(f) },
                    None => return Err(concat!("the speech engine lacks ", $name).into()),
                }
            };
        }
        Ok(Self {
            create_rec: sym!("SherpaOnnxCreateOfflineRecognizer"),
            destroy_rec: sym!("SherpaOnnxDestroyOfflineRecognizer"),
            create_stream: sym!("SherpaOnnxCreateOfflineStream"),
            destroy_stream: sym!("SherpaOnnxDestroyOfflineStream"),
            accept: sym!("SherpaOnnxAcceptWaveformOffline"),
            decode: sym!("SherpaOnnxDecodeOfflineStream"),
            result: sym!("SherpaOnnxGetOfflineStreamResult"),
            destroy_result: sym!("SherpaOnnxDestroyOfflineRecognizerResult"),
            create_diar: sym!("SherpaOnnxCreateOfflineSpeakerDiarization"),
            destroy_diar: sym!("SherpaOnnxDestroyOfflineSpeakerDiarization"),
            diar_rate: sym!("SherpaOnnxOfflineSpeakerDiarizationGetSampleRate"),
            diar_process: sym!("SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback"),
            diar_count: sym!("SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments"),
            diar_sorted: sym!("SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime"),
            diar_free_segments: sym!("SherpaOnnxOfflineSpeakerDiarizationDestroySegment"),
            diar_free_result: sym!("SherpaOnnxOfflineSpeakerDiarizationDestroyResult"),
            ext_create: sym!("SherpaOnnxCreateSpeakerEmbeddingExtractor"),
            ext_destroy: sym!("SherpaOnnxDestroySpeakerEmbeddingExtractor"),
            ext_dim: sym!("SherpaOnnxSpeakerEmbeddingExtractorDim"),
            ext_stream: sym!("SherpaOnnxSpeakerEmbeddingExtractorCreateStream"),
            ext_ready: sym!("SherpaOnnxSpeakerEmbeddingExtractorIsReady"),
            ext_compute: sym!("SherpaOnnxSpeakerEmbeddingExtractorComputeEmbedding"),
            ext_free: sym!("SherpaOnnxSpeakerEmbeddingExtractorDestroyEmbedding"),
            on_accept: sym!("SherpaOnnxOnlineStreamAcceptWaveform"),
            on_finish: sym!("SherpaOnnxOnlineStreamInputFinished"),
            on_destroy: sym!("SherpaOnnxDestroyOnlineStream"),
        })
    }
}

/// One caption line. `speaker` counts from 0 (shown to the user as "User 1", "User 2"...).
#[derive(Clone, Debug)]
pub struct Line {
    pub start: f32,
    pub end: f32,
    pub text: String,
    pub speaker: i32,
}

/// Model files for one run. Whisper is required; the two speaker models are optional
/// (without them every line is User 1).
pub struct Models<'a> {
    pub whisper_dir: &'a Path,
    pub segmentation: Option<&'a Path>,
    pub embedding: Option<&'a Path>,
    /// How many people talk, when the user knows; 0 = work it out.
    pub people: i32,
}

fn cstr(p: &Path) -> CString {
    CString::new(p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn pick(dir: &Path, needle: &str, ext: &str) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            n.contains(needle) && n.ends_with(ext)
        })
        .collect();
    v.sort_by_key(|p| !p.to_string_lossy().contains(".int8."));
    v.into_iter().next()
}

struct Progress<'a>(&'a mut dyn FnMut(f32));

unsafe extern "C" fn diar_tick(done: i32, total: i32, arg: *mut c_void) -> i32 {
    if total > 0 {
        let p = unsafe { &mut *(arg as *mut Progress) };
        (p.0)(0.3 * done as f32 / total as f32);
    }
    0
}

/// `samples`: 16 kHz mono in [-1, 1]. `progress` gets 0.0..=1.0. Line times are seconds from the start of `samples`.
pub fn generate(dll_dir: &Path, m: &Models, samples: &[f32], threads: i32, progress: &mut dyn FnMut(f32)) -> Result<Vec<Line>, String> {
    let api = Api::load(dll_dir)?;
    let rate = 16_000.0_f32;
    let total = samples.len() as f32 / rate;
    if total < 0.3 {
        return Ok(Vec::new());
    }

    // 1. Who speaks when.
    let mut turns: Vec<(f32, f32, i32)> = Vec::new();
    if let (Some(seg), Some(emb)) = (m.segmentation, m.embedding) {
        let (seg_c, emb_c, cpu) = (cstr(seg), cstr(emb), CString::new("cpu").unwrap());
        let cfg = DiarConfig {
            segmentation: Segmentation { pyannote: Pyannote { model: seg_c.as_ptr(), window_shift_ratio: 0.0 }, num_threads: threads, debug: 0, provider: cpu.as_ptr() },
            embedding: Embedding { model: emb_c.as_ptr(), num_threads: threads, debug: 0, provider: cpu.as_ptr() },
            clustering: Clustering { num_clusters: if m.people > 0 { m.people } else { -1 }, threshold: std::env::var("ARCHY_SPK_THRESHOLD").ok().and_then(|v| v.parse().ok()).unwrap_or(0.45), compute_confidence: 0 },
            min_on: 0.3,
            min_off: 0.5,
        };
        let sd = unsafe { (api.create_diar)(&cfg) };
        if sd.is_null() {
            return Err("the speaker model couldn't start (check the model files)".into());
        }
        debug_assert_eq!(unsafe { (api.diar_rate)(sd) }, 16_000);
        let mut prog = Progress(progress);
        let r = unsafe { (api.diar_process)(sd, samples.as_ptr(), samples.len() as i32, diar_tick, &mut prog as *mut Progress as *mut c_void) };
        if !r.is_null() {
            let n = unsafe { (api.diar_count)(r) } as usize;
            let segs = unsafe { (api.diar_sorted)(r) };
            if !segs.is_null() {
                for i in 0..n {
                    let s = unsafe { &*segs.add(i) };
                    turns.push((s.start, s.end, s.speaker));
                }
                unsafe { (api.diar_free_segments)(segs) };
            }
            unsafe { (api.diar_free_result)(r) };
        }
        unsafe { (api.destroy_diar)(sd) };
    }
    if turns.is_empty() {
        turns.push((0.0, total, 0));
    }
    progress(0.3);

    // Join a speaker's neighbouring pieces and keep each piece under Whisper's 30 s window.
    let mut chunks: Vec<(f32, f32, i32)> = Vec::new();
    for (s, e, spk) in turns {
        if let Some(last) = chunks.last_mut() {
            if last.2 == spk && s - last.1 < 0.4 && e - last.0 <= 25.0 {
                last.1 = e;
                continue;
            }
        }
        let mut a = s;
        while e - a > 25.0 {
            chunks.push((a, a + 25.0, spk));
            a += 25.0;
        }
        if e - a >= 0.3 {
            chunks.push((a, e, spk));
        }
    }

    // 2. What was said.
    let tokens = pick(m.whisper_dir, "tokens", ".txt").ok_or("the model's tokens file is missing")?;
    // A folder with one plain model file (no encoder/decoder) is a NeMo CTC model (Parakeet 110M).
    let ctc = if pick(m.whisper_dir, "encoder", ".onnx").is_none() { pick(m.whisper_dir, "model", ".onnx") } else { None };
    let encoder = pick(m.whisper_dir, "encoder", ".onnx").or_else(|| ctc.clone()).ok_or("the model's encoder file is missing")?;
    let decoder = pick(m.whisper_dir, "decoder", ".onnx").or_else(|| ctc.clone()).ok_or("the model's decoder file is missing")?;
    // A folder with a joiner is a Parakeet (transducer) model; otherwise it's Whisper.
    let joiner = pick(m.whisper_dir, "joiner", ".onnx");
    let (enc_c, dec_c, tok_c, join_c) = (cstr(&encoder), cstr(&decoder), cstr(&tokens), joiner.as_deref().map(cstr));
    let (lang, task, cpu, greedy) = (CString::new("en").unwrap(), CString::new("transcribe").unwrap(), CString::new("cpu").unwrap(), CString::new("greedy_search").unwrap());
    let nemo = CString::new("nemo_transducer").unwrap();
    let mut cfg: RecognizerConfig = unsafe { std::mem::zeroed() };
    cfg.feat = Feature { sample_rate: 16_000, feature_dim: 80 };
    if ctc.is_some() {
        cfg.model.nemo_ctc = OneModel { model: enc_c.as_ptr() };
    } else if let Some(j) = &join_c {
        cfg.model.transducer = Transducer { encoder: enc_c.as_ptr(), decoder: dec_c.as_ptr(), joiner: j.as_ptr() };
        cfg.model.model_type = nemo.as_ptr();
    } else {
        cfg.model.whisper = Whisper { encoder: enc_c.as_ptr(), decoder: dec_c.as_ptr(), language: lang.as_ptr(), task: task.as_ptr(), tail_paddings: -1, token_ts: 0, segment_ts: 0 };
    }
    cfg.model.tokens = tok_c.as_ptr();
    cfg.model.num_threads = threads;
    cfg.model.provider = cpu.as_ptr();
    cfg.decoding_method = greedy.as_ptr();
    cfg.max_active_paths = 4;
    let rec = unsafe { (api.create_rec)(&cfg) };
    if rec.is_null() {
        return Err("the Whisper model couldn't start (check the model files)".into());
    }

    let all: f32 = chunks.iter().map(|c| c.1 - c.0).sum::<f32>().max(0.1);
    let mut done = 0.0_f32;
    let mut lines = Vec::new();
    for (s, e, spk) in chunks {
        let a = ((s * rate) as usize).min(samples.len());
        let b = ((e * rate) as usize).min(samples.len());
        if b > a {
            let piece = &samples[a..b];
            let stream = unsafe { (api.create_stream)(rec) };
            unsafe {
                (api.accept)(stream, 16_000, piece.as_ptr(), piece.len() as i32);
                (api.decode)(rec, stream);
            }
            let res = unsafe { (api.result)(stream) };
            if !res.is_null() {
                let text = unsafe { (*res).text };
                if !text.is_null() {
                    let t = unsafe { CStr::from_ptr(text) }.to_string_lossy().trim().to_string();
                    // Real per-word times when the model reports them (Parakeet); otherwise spread evenly.
                    let r = unsafe { &*res };
                    let mut timed = Vec::new();
                    if !r.timestamps.is_null() && !r.tokens_arr.is_null() && r.count > 0 && !is_noise(&t) {
                        let n = r.count as usize;
                        for i in 0..n {
                            let tok = unsafe { CStr::from_ptr(*r.tokens_arr.add(i)) }.to_string_lossy().into_owned();
                            let ts = s + unsafe { *r.timestamps.add(i) };
                            if tok.starts_with(['\u{2581}', ' ']) || timed.is_empty() {
                                timed.push((ts, ts, tok.trim_start_matches(['\u{2581}', ' ']).to_string()));
                            } else if let Some(w) = timed.last_mut() {
                                w.1 = ts;
                                w.2.push_str(&tok);
                            }
                        }
                    }
                    if timed.is_empty() { lines.extend(split_lines(&t, s, e, spk)); } else { lines.extend(lines_from_words(timed, e, spk)); }
                }
                unsafe { (api.destroy_result)(res) };
            }
            unsafe { (api.destroy_stream)(stream) };
        }
        done += e - s;
        progress(0.3 + 0.7 * (done / all).min(1.0));
    }
    unsafe { (api.destroy_rec)(rec) };
    // Second opinion on who speaks: a voice fingerprint per caption line, clustered (exact count when the user gave one).
    // Only when the user says how many people talk: on mixed game audio an automatic count is not reliable
    // (a single voice gets cut into several "users"), but forcing a known count works well.
    if m.segmentation.is_some() && (m.people > 0 || std::env::var("ARCHY_RELABEL").is_ok()) {
        if let Some(emb) = m.embedding {
            relabel(&api, emb, samples, &mut lines, m.people, threads);
        }
    }
    // Order by time, then number the voices by who is heard first (User 1, 2, 3 with no gaps).
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut order: Vec<i32> = Vec::new();
    for l in &mut lines {
        l.speaker = match order.iter().position(|s| *s == l.speaker) {
            Some(k) => k as i32,
            None => {
                order.push(l.speaker);
                order.len() as i32 - 1
            }
        };
    }
    Ok(lines)
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

struct Cluster {
    members: Vec<usize>,
    cen: Vec<f32>,
    dur: f32,
}

/// Replaces the diarizer's speaker numbers with ones from clustering each line's voice fingerprint.
/// `people` > 0 forces exactly that many voices (when there are enough usable lines).
fn relabel(api: &Api, model: &Path, samples: &[f32], lines: &mut [Line], people: i32, threads: i32) {
    const MIN_SECS: f32 = 0.8;
    let debug = std::env::var("ARCHY_DEBUG").is_ok();
    let (model_c, cpu) = (cstr(model), CString::new("cpu").unwrap());
    let cfg = EmbCfg { model: model_c.as_ptr(), num_threads: threads, debug: 0, provider: cpu.as_ptr() };
    let ex = unsafe { (api.ext_create)(&cfg) };
    if ex.is_null() {
        return;
    }
    let dim = unsafe { (api.ext_dim)(ex) } as usize;
    let rate = 16_000.0_f32;
    let mut embs: Vec<Option<Vec<f32>>> = Vec::with_capacity(lines.len());
    for l in lines.iter() {
        let a = ((l.start * rate) as usize).min(samples.len());
        let b = ((l.end * rate) as usize).min(samples.len());
        if l.end - l.start < MIN_SECS || b <= a {
            embs.push(None);
            continue;
        }
        let st = unsafe { (api.ext_stream)(ex) };
        let mut out = None;
        unsafe {
            (api.on_accept)(st, 16_000, samples[a..b].as_ptr(), (b - a) as i32);
            (api.on_finish)(st);
            if (api.ext_ready)(ex, st) != 0 {
                let p = (api.ext_compute)(ex, st);
                if !p.is_null() {
                    out = Some(unit(std::slice::from_raw_parts(p, dim).to_vec()));
                    (api.ext_free)(p);
                }
            }
            (api.on_destroy)(st);
        }
        embs.push(out);
    }
    unsafe { (api.ext_destroy)(ex) };

    // Everything in the clip shares the same game sound and recording channel, which makes every voice look alike.
    // Subtracting the average fingerprint cancels that and leaves what differs between people.
    let have: Vec<usize> = (0..embs.len()).filter(|&i| embs[i].is_some()).collect();
    if have.len() >= 3 {
        let mut mean = vec![0.0_f32; dim];
        for &i in &have {
            for (m, x) in mean.iter_mut().zip(embs[i].as_ref().unwrap()) {
                *m += x / have.len() as f32;
            }
        }
        for &i in &have {
            let e = embs[i].as_mut().unwrap();
            for (x, m) in e.iter_mut().zip(&mean) {
                *x -= m;
            }
            *e = unit(std::mem::take(e));
        }
    }

    let mut cl: Vec<Cluster> = (0..lines.len())
        .filter_map(|i| embs[i].as_ref().map(|e| Cluster { members: vec![i], cen: e.clone(), dur: lines[i].end - lines[i].start }))
        .collect();
    if cl.len() < 2 {
        return;
    }
    // Two voices are the same person when their fingerprints are this close (cosine distance).
    let t: f32 = std::env::var("ARCHY_SPK_T").ok().and_then(|v| v.parse().ok()).unwrap_or(0.65);
    let closest = |cl: &[Cluster]| {
        let mut best = (-2.0_f32, 0, 1);
        for i in 0..cl.len() {
            for j in i + 1..cl.len() {
                let s = dot(&cl[i].cen, &cl[j].cen);
                if s > best.0 {
                    best = (s, i, j);
                }
            }
        }
        best
    };
    let merge = |cl: &mut Vec<Cluster>, i: usize, j: usize| {
        let b = cl.remove(j);
        let a = &mut cl[i];
        let cen: Vec<f32> = a.cen.iter().zip(&b.cen).map(|(x, y)| x * a.dur + y * b.dur).collect();
        a.cen = unit(cen);
        a.dur += b.dur;
        a.members.extend(b.members);
    };
    loop {
        if cl.len() <= 1 || (people > 0 && cl.len() as i32 <= people) {
            break;
        }
        let (s, i, j) = closest(&cl);
        if debug {
            eprintln!("merge sim {s:.3} ({} clusters)", cl.len());
        }
        if people <= 0 && 1.0 - s > t {
            break;
        }
        merge(&mut cl, i, j);
    }
    if people <= 0 {
        // One short line on its own is noise more often than a person.
        while cl.len() > 1 {
            // A person needs a real share of the talking: a few lines and several seconds (a lone voice
            // doesn't get cut into "users" by background noise).
            let Some(k) = (0..cl.len()).filter(|&k| cl[k].members.len() < 3 || cl[k].dur < 5.0).min_by(|&a, &b| cl[a].dur.total_cmp(&cl[b].dur)) else { break };
            let best = (0..cl.len()).filter(|&o| o != k).max_by(|&a, &b| dot(&cl[k].cen, &cl[a].cen).total_cmp(&dot(&cl[k].cen, &cl[b].cen))).unwrap();
            let (lo, hi) = (k.min(best), k.max(best));
            merge(&mut cl, lo, hi);
        }
        // At most six voices.
        while cl.len() > 6 {
            let (_, i, j) = closest(&cl);
            merge(&mut cl, i, j);
        }
    }
    let mut label = vec![-1_i32; lines.len()];
    for (c, k) in cl.iter().enumerate() {
        for &i in &k.members {
            label[i] = c as i32;
        }
    }
    // Lines too short to fingerprint take the voice of the nearest fingerprinted line in time.
    let known: Vec<usize> = (0..lines.len()).filter(|&i| label[i] >= 0).collect();
    // The diarizer's own label says which cluster a short line most likely belongs to (when its other lines agree).
    let mut votes: std::collections::HashMap<i32, Vec<u32>> = std::collections::HashMap::new();
    for &i in &known {
        votes.entry(lines[i].speaker).or_insert_with(|| vec![0; cl.len()])[label[i] as usize] += 1;
    }
    for i in 0..lines.len() {
        if label[i] < 0 {
            if let Some(v) = votes.get(&lines[i].speaker) {
                let total: u32 = v.iter().sum();
                let (best, n) = v.iter().enumerate().max_by_key(|(_, n)| **n).map(|(k, n)| (k, *n)).unwrap_or((0, 0));
                if total >= 2 && n as f32 >= 0.6 * total as f32 {
                    label[i] = best as i32;
                    continue;
                }
            }
            if let Some(&n) = known.iter().min_by(|&&a, &&b| (lines[a].start - lines[i].start).abs().total_cmp(&(lines[b].start - lines[i].start).abs())) {
                label[i] = label[n];
            }
        }
    }
    if debug {
        eprintln!("relabel: {} clusters (threshold {t}), {} of {} lines fingerprinted", cl.len(), known.len(), lines.len());
    }
    for (l, s) in lines.iter_mut().zip(label) {
        if s >= 0 {
            l.speaker = s;
        }
    }
}

/// Whisper marks silence and noise as "[BLANK_AUDIO]", "(music)" and the like: not captions.
fn is_noise(t: &str) -> bool {
    let t = t.trim();
    let bracketed = matches!(t.chars().next(), Some('[' | '(' | '*')) && matches!(t.chars().last(), Some(']' | ')' | '*'));
    t.is_empty() || (bracketed && t.len() < 40)
}

/// Readable lines from words with their own start times: a line breaks at a sentence end, a pause or ~7 words.
fn lines_from_words(mut words: Vec<(f32, f32, String)>, limit: f32, speaker: i32) -> Vec<Line> {
    words.retain(|w| w.2.chars().any(|c| c.is_alphanumeric()));
    let n = words.len();
    // A word lasts until its last piece plus a short tail, or until the next word starts.
    for i in 0..n {
        let next = if i + 1 < n { words[i + 1].0 } else { limit };
        words[i].1 = (words[i].1 + 0.3).min(next).max(words[i].0 + 0.08);
    }
    let mut out: Vec<Line> = Vec::new();
    let mut cur: Option<Line> = None;
    let mut count = 0;
    let flush = |out: &mut Vec<Line>, cur: &mut Option<Line>, count: &mut i32| {
        if let Some(l) = cur.take() { out.push(l); }
        *count = 0;
    };
    for (i, (st, en, w)) in words.iter().enumerate() {
        if let Some(l) = &cur {
            if count >= 7 || l.text.len() + w.len() > 42 || *st - l.end > 0.7 {
                flush(&mut out, &mut cur, &mut count);
            }
        }
        match &mut cur {
            Some(l) => { l.text.push(' '); l.text.push_str(w); l.end = *en; }
            None => cur = Some(Line { start: *st, end: *en, text: w.clone(), speaker }),
        }
        count += 1;
        if w.ends_with(['.', '?', '!']) && count >= 3 && i + 1 < n {
            flush(&mut out, &mut cur, &mut count);
        }
    }
    flush(&mut out, &mut cur, &mut count);
    out
}

/// Small models can get stuck repeating ("I said I said I said..."): keep a repeated word or short phrase at most twice in a row.
fn collapse_loops(w: Vec<&str>) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for x in w {
        out.push(x);
        for n in 1..=3 {
            let len = out.len();
            if len >= n * 3 && out[len - n..] == out[len - 2 * n..len - n] && out[len - n..] == out[len - 3 * n..len - 2 * n] {
                out.truncate(len - n);
                break;
            }
        }
    }
    out
}

/// Cuts one spoken stretch into readable lines (about 7 words), timed in proportion to their length.
fn split_lines(text: &str, start: f32, end: f32, speaker: i32) -> Vec<Line> {
    if is_noise(text) {
        return Vec::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let words = collapse_loops(words.into_iter().filter(|w| w.chars().any(|c| c.is_alphanumeric())).collect());
    if words.is_empty() {
        return Vec::new();
    }
    let mut groups: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut n = 0;
    for w in &words {
        if !cur.is_empty() && (n >= 7 || cur.len() + w.len() > 42) {
            groups.push(std::mem::take(&mut cur));
            n = 0;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(w);
        n += 1;
        if w.ends_with(['.', '?', '!']) && n >= 3 {
            groups.push(std::mem::take(&mut cur));
            n = 0;
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    let total: usize = groups.iter().map(|g| g.len().max(1)).sum();
    let span = (end - start).max(0.2);
    let mut at = start;
    groups
        .into_iter()
        .map(|g| {
            let len = span * g.len().max(1) as f32 / total as f32;
            let l = Line { start: at, end: at + len, text: g, speaker };
            at += len;
            l
        })
        .collect()
}
