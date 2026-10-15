//! Constant-frame-rate pacing: which captured frames to keep, and when.
//!
//! Clips must have a frame on every slot of a fixed grid (1/fps apart), each
//! stamped exactly on its slot. Editors (After Effects, Premiere, Resolve) read
//! anything else as variable frame rate and stutter or drift out of sync: with
//! raw compositor times a 60 fps clip from a 144 Hz monitor had gaps of 7, 14,
//! 21 and 28 ms and claimed to be 120 fps.
//!
//! So each slot takes the first frame within half an interval of it (a 90 or
//! 144 Hz game thins to exactly the target), and a slot that gets no new frame
//! (static screen, a game running below the target) repeats the last one.
//! Times are on the capture clock, so audio stays in sync.

/// Holes up to this long are filled with repeats (at least 4 slots): a short
/// hiccup stays invisible. A longer one means the loop itself was held up, and
/// a burst would only overflow the encoder's queue; those slots are lost.
const MAX_FILL_TIME: i64 = 600_000; // 60 ms
const MIN_FILL: i64 = 4;

pub struct Pacer {
    interval: i64,
    /// Capture time of slot 0 (100 ns units); `None` until the first frame.
    origin: Option<i64>,
    /// The next slot to fill.
    next: i64,
    /// Most slots repeated at once.
    max_fill: i64,
    /// Slots skipped (not filled) since the last `take_lost`.
    lost: u64,
}

/// What to send for a newly captured frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Slots {
    /// Slot times to fill with the previous frame first (short holes).
    pub repeat: Vec<i64>,
    /// The slot time this frame is stamped with.
    pub at: i64,
}

impl Pacer {
    pub fn new(fps: u32) -> Self {
        // The same whole-number interval the encoders use for frame durations.
        let interval = 10_000_000 / fps.max(1) as i64;
        Self { interval, origin: None, next: 0, max_fill: (MAX_FILL_TIME / interval).max(MIN_FILL), lost: 0 }
    }

    /// Whether the first frame has been placed yet.
    pub fn started(&self) -> bool {
        self.origin.is_some()
    }

    pub fn interval(&self) -> i64 {
        self.interval
    }

    /// Slots left empty (the loop was held up too long) since the last call:
    /// they are frames missing from the clip, so they count as skipped.
    pub fn take_lost(&mut self) -> u64 {
        std::mem::take(&mut self.lost)
    }

    fn slot_time(&self, n: i64) -> i64 {
        self.origin.unwrap_or(0) + n * self.interval
    }

    /// A frame captured at `time`, seen at `now`: `None` if its slot is already
    /// taken (too soon after the last one), else the holes to repeat and its slot.
    pub fn frame(&mut self, time: i64, now: i64) -> Option<Slots> {
        let Some(origin) = self.origin else {
            // The first frame of a capture is whatever was last on screen, stamped
            // with when it was drawn (often a second ago). Anchoring the grid to that
            // made the next real frame look a second late: a gap at every start.
            let start = if now - time > self.interval { now } else { time };
            self.origin = Some(start);
            self.next = 1;
            return Some(Slots { repeat: Vec::new(), at: start });
        };
        let slot = (time - origin + self.interval / 2).div_euclid(self.interval);
        if slot < self.next {
            return None;
        }
        let hole = slot - self.next;
        let repeat = if hole <= self.max_fill {
            (self.next..slot).map(|n| self.slot_time(n)).collect()
        } else {
            self.lost += hole as u64;
            Vec::new() // held up: carry on from here rather than burst
        };
        self.next = slot + 1;
        Some(Slots { repeat, at: self.slot_time(slot) })
    }

    /// Whether a frame captured at `time` would get a slot (without taking it).
    pub fn wants(&self, time: i64) -> bool {
        match self.origin {
            None => true,
            Some(origin) => (time - origin + self.interval / 2).div_euclid(self.interval) >= self.next,
        }
    }

    /// No new frame has arrived by `now`: the slot times that are due, to be
    /// filled with the last frame (none before the first frame).
    pub fn idle(&mut self, now: i64) -> Vec<i64> {
        if self.origin.is_none() {
            return Vec::new();
        }
        let mut due = Vec::new();
        while self.slot_time(self.next) + self.interval / 2 <= now && (due.len() as i64) < self.max_fill {
            due.push(self.slot_time(self.next));
            self.next += 1;
        }
        // Still far behind (the loop was held up): skip ahead instead of bursting.
        if self.slot_time(self.next) + self.interval / 2 <= now {
            // The slot `now` falls in stays open for the frame that comes next.
            let resume = (now - self.origin.unwrap_or(0) + self.interval / 2).div_euclid(self.interval);
            self.lost += (resume - self.next).max(0) as u64;
            self.next = resume;
        }
        due
    }

    /// When the next slot becomes overdue (capture-clock time to wake up at).
    pub fn deadline(&self) -> Option<i64> {
        self.origin.map(|_| self.slot_time(self.next) + self.interval / 2)
    }
}

/// The capture clock now (QPC, in 100 ns units), as frame times use.
pub fn clock_now() -> i64 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let (mut count, mut freq) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    if freq <= 0 {
        return 0;
    }
    (count as i128 * 10_000_000 / freq as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds frames, idling between them like the capture loop, and returns
    /// every slot time sent (repeats included).
    fn run(fps: u32, frames: &[i64]) -> Vec<i64> {
        let mut p = Pacer::new(fps);
        let mut sent = Vec::new();
        for &t in frames {
            sent.extend(p.idle(t));
            if let Some(s) = p.frame(t, t) {
                sent.extend(s.repeat);
                sent.push(s.at);
            }
        }
        sent
    }

    fn assert_cfr(sent: &[i64], interval: i64) {
        for w in sent.windows(2) {
            assert_eq!(w[1] - w[0], interval, "not constant: {w:?}");
        }
    }

    #[test]
    fn high_refresh_becomes_exact_target_rate() {
        // A 144 Hz monitor for 10 s at a 60 fps target.
        let sent = run(60, &(0..1440).map(|i| i * 69_444).collect::<Vec<_>>());
        assert_cfr(&sent, 166_666);
        assert!((598..=602).contains(&sent.len()), "{}", sent.len());
    }

    #[test]
    fn jitter_keeps_every_frame_on_its_slot() {
        let jitter = [0, 9_000, -7_000, 4_000];
        let sent = run(60, &(0..600).map(|i| i * 166_667 + jitter[i as usize % 4]).collect::<Vec<_>>());
        assert_eq!(sent.len(), 600);
        assert_cfr(&sent, 166_666);
    }

    #[test]
    fn slow_game_is_filled_with_repeats() {
        // A game at 30 fps recorded at 60: every other slot repeats.
        let sent = run(60, &(0..300).map(|i| i * 333_333).collect::<Vec<_>>());
        assert_cfr(&sent, 166_666);
        assert!((597..=600).contains(&sent.len()), "{}", sent.len());
    }

    #[test]
    fn static_screen_repeats_in_real_time() {
        let mut p = Pacer::new(60);
        p.frame(1_000_000, 1_000_000).unwrap();
        // Nothing new for a second, checked as the loop would (every ~8 ms).
        let mut sent = vec![1_000_000];
        for k in 1..=125 {
            sent.extend(p.idle(1_000_000 + k * 80_000));
        }
        assert_cfr(&sent, 166_666);
        assert!((59..=61).contains(&(sent.len() - 1)), "{}", sent.len());
    }

    #[test]
    fn a_stale_first_frame_does_not_open_a_gap() {
        // Capture starts; its first frame was drawn 955 ms earlier.
        let mut p = Pacer::new(60);
        let now = 100_000_000;
        let first = p.frame(now - 9_550_000, now).unwrap();
        assert_eq!(first.at, now, "re-stamped to when it was seen");
        // The next real frame, 15 ms later, lands on the very next slot.
        let next = p.frame(now + 150_000, now + 150_000).unwrap();
        assert!(next.repeat.is_empty());
        assert_eq!(next.at - first.at, 166_666);
    }

    #[test]
    fn a_short_hiccup_at_144_is_filled_not_lost() {
        // The capture thread was held up 55 ms (8 slots at 144 fps).
        let mut p = Pacer::new(144);
        let i = p.interval();
        p.frame(0, 0).unwrap();
        let next = p.frame(9 * i, 9 * i).unwrap();
        assert_eq!(next.repeat.len(), 8);
        assert_eq!(p.take_lost(), 0);
        // A hole well past 60 ms isn't burst into the encoder: it's lost, and counted.
        let after = p.frame(40 * i, 40 * i).unwrap();
        assert!(after.repeat.is_empty());
        assert_eq!(p.take_lost(), 30);
    }

    #[test]
    fn a_long_stall_skips_ahead_without_a_burst() {
        let mut p = Pacer::new(60);
        p.frame(0, 0).unwrap();
        let due = p.idle(50_000_000); // the loop was blocked for 5 s
        assert!(due.len() as i64 <= p.max_fill);
        assert!(p.take_lost() > 250, "the skipped slots are counted");
        let next = p.frame(50_000_000, 50_000_000).expect("the frame after a stall is kept");
        assert!(next.repeat.is_empty());
        assert_eq!(next.at % 166_666, 0, "still on the grid");
        assert!((next.at - 50_000_000).abs() <= 83_333);
    }
}
