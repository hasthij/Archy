//! The replay buffer: the last N seconds of encoded video, kept in RAM.
//!
//! It holds compressed packets, not frames: 60 s at 10 Mbps is ~75 MB. Saving
//! a clip copies packet references out under the lock and writes the file
//! afterwards, so the encoder is never blocked by a save.

use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone)]
pub struct StoredPacket {
    pub data: Arc<[u8]>,
    /// Presentation time, 100 ns units.
    pub time: i64,
    pub duration: i64,
    pub keyframe: bool,
}

pub struct ReplayBuffer {
    packets: VecDeque<StoredPacket>,
    /// How much history to keep (100 ns units).
    keep: i64,
    bytes: usize,
}

impl ReplayBuffer {
    pub fn new(seconds: u32) -> Self {
        Self {
            packets: VecDeque::new(),
            keep: seconds as i64 * 10_000_000,
            bytes: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Seconds from the first to the end of the last packet.
    pub fn span_seconds(&self) -> f64 {
        match (self.packets.front(), self.packets.back()) {
            (Some(a), Some(b)) => (b.time + b.duration - a.time) as f64 / 1e7,
            _ => 0.0,
        }
    }

    pub fn push(&mut self, packet: StoredPacket) {
        let latest = packet.time;
        self.bytes += packet.data.len();
        self.packets.push_back(packet);
        // Drop whole keyframe intervals from the front, but only while the next
        // keyframe still starts early enough to cover the full history window.
        while let Some(next_key) = self.packets.iter().skip(1).position(|p| p.keyframe) {
            let next_key = next_key + 1;
            if self.packets[next_key].time > latest - self.keep {
                break;
            }
            for p in self.packets.drain(..next_key) {
                self.bytes -= p.data.len();
            }
        }
    }

    /// Packets covering the last `seconds`, starting at a keyframe at or before
    /// that point (clips can only start on a keyframe without re-encoding).
    pub fn last(&self, seconds: u32) -> Vec<StoredPacket> {
        let Some(latest) = self.packets.back().map(|p| p.time) else {
            return Vec::new();
        };
        let from = latest - seconds as i64 * 10_000_000;
        let start = self
            .packets
            .iter()
            .rposition(|p| p.keyframe && p.time <= from)
            .or_else(|| self.packets.iter().position(|p| p.keyframe));
        match start {
            Some(i) => self.packets.range(i..).cloned().collect(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(i: i64) -> StoredPacket {
        // 60 fps, keyframe every second, 1 KB each.
        StoredPacket {
            data: Arc::from(vec![0u8; 1000]),
            time: i * 166_667,
            duration: 166_667,
            keyframe: i % 60 == 0,
        }
    }

    #[test]
    fn keeps_about_the_window_and_starts_on_keyframe() {
        let mut ring = ReplayBuffer::new(10);
        for i in 0..60 * 30 {
            ring.push(packet(i));
        }
        let all = ring.last(10);
        assert!(all[0].keyframe);
        let span = all.last().unwrap().time - all[0].time;
        assert!((10 * 10_000_000..11 * 10_000_000).contains(&span), "{span}");
        // Memory bounded to window + one keyframe interval.
        assert!(ring.bytes() <= 1000 * 60 * 12);
    }

    #[test]
    fn shorter_clip_than_buffer() {
        let mut ring = ReplayBuffer::new(60);
        for i in 0..60 * 90 {
            ring.push(packet(i));
        }
        let clip = ring.last(15);
        let span = clip.last().unwrap().time - clip[0].time;
        assert!(clip[0].keyframe);
        assert!((15 * 10_000_000..16 * 10_000_000).contains(&span), "{span}");
    }

    #[test]
    fn empty_until_first_keyframe() {
        let mut ring = ReplayBuffer::new(10);
        assert!(ring.last(5).is_empty());
        ring.push(packet(1)); // not a keyframe
        assert!(ring.last(5).is_empty());
    }
}
