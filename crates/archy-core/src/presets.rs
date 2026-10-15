//! Quality presets: output height + average bitrate, so clip size is predictable.

pub struct Preset {
    pub name: &'static str,
    pub height: u32,
    /// Average bitrate in Mbps.
    pub mbps: f32,
}

/// Approximate size per 30 s clip in the comments.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "360p",
        height: 360,
        mbps: 1.5,
    }, // ~6 MB
    Preset {
        name: "480p",
        height: 480,
        mbps: 2.5,
    }, // ~9 MB, fits Discord's 10 MB
    Preset {
        name: "720p",
        height: 720,
        // 5 Mbps looked grainy in motion (fast camera over detailed ground).
        mbps: 6.5,
    }, // ~25 MB
    Preset {
        name: "1080p",
        height: 1080,
        mbps: 10.0,
    }, // ~38 MB
    Preset {
        name: "1440p",
        height: 1440,
        mbps: 18.0,
    }, // ~68 MB
];

pub fn find(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}
