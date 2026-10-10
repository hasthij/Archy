//! Which captured frames to keep for a target frame rate.

/// Frames are due on a fixed grid (one per interval). Each due slot takes the
/// first frame within half an interval of it, so a 90 or 144 Hz game averages
/// exactly the target rate and short delivery hiccups are caught up. After a
/// real pause (static screen) the grid restarts instead of bursting to catch up;
/// restarting on every 20-30 ms compositor hiccup cost ~6% of frames.
pub struct Cadence {
    interval: i64,
    next_due: i64,
}

const GRID_RESET: i64 = 2_500_000; // 250 ms in 100 ns units

impl Cadence {
    pub fn new(fps: u32) -> Self {
        Self {
            interval: 10_000_000 / fps.max(1) as i64,
            next_due: i64::MIN / 2,
        }
    }

    /// Whether to keep a frame captured at `time` (100 ns units).
    pub fn accept(&mut self, time: i64) -> bool {
        if time < self.next_due - self.interval / 2 {
            return false;
        }
        self.next_due = if time > self.next_due + GRID_RESET {
            time + self.interval
        } else {
            self.next_due + self.interval
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(fps: u32, times: impl Iterator<Item = i64>) -> usize {
        let mut c = Cadence::new(fps);
        times.filter(|&t| c.accept(t)).count()
    }

    #[test]
    fn same_rate_keeps_every_frame_despite_jitter() {
        let jitter = [0, 9_000, -7_000, 4_000];
        let n = kept(60, (0..600).map(|i| i * 166_667 + jitter[i as usize % 4]));
        assert_eq!(n, 600);
    }

    #[test]
    fn high_refresh_thins_to_target() {
        // 144 Hz for 10 s -> about 600 frames at 60.
        let n = kept(60, (0..1440).map(|i| i * 69_444));
        assert!((595..=605).contains(&n), "{n}");
    }

    #[test]
    fn pause_restarts_grid_without_burst() {
        let mut times: Vec<i64> = (0..60).map(|i| i * 166_667).collect();
        // 2 s pause, then frames resume at 144 Hz for 1 s.
        let resume = times[59] + 20_000_000;
        times.extend((0..144).map(|i| resume + i * 69_444));
        let n = kept(60, times.into_iter());
        assert!((118..=122).contains(&n), "{n}");
    }
}
