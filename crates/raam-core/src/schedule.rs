//! The sleep/wake schedule's math: times of day in local minutes after
//! midnight. Pure — local time itself comes through the Clock seam, and
//! the debug-prop overrides live with the host.

use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Schedule {
    pub enabled: bool,
    pub sleep_min: u32,
    pub wake_min: u32,
}

impl Schedule {
    /// Whether `min` (minutes after local midnight) falls in sleep hours.
    /// The window may wrap midnight (23:00-05:00) or not (13:00-14:00).
    pub fn asleep_at(&self, min: u32) -> bool {
        if !self.enabled || self.sleep_min == self.wake_min {
            return false;
        }
        if self.sleep_min < self.wake_min {
            min >= self.sleep_min && min < self.wake_min
        } else {
            min >= self.sleep_min || min < self.wake_min
        }
    }
}

pub fn fmt_hm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

pub fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Local seconds after midnight, and the matching unix time (via the Clock).
pub fn local_now() -> (u32, i64) {
    let epoch = crate::clock::wall().as_secs() as i64;
    let t = crate::clock::local(epoch);
    ((t.hour * 3600 + t.min * 60 + t.sec) as u32, epoch)
}

/// Time from `now_sod` (seconds after midnight) to the next `target_min`,
/// always in the future (a full day if it is exactly now).
pub fn until(target_min: u32, now_sod: u32) -> Duration {
    let target = target_min * 60;
    let secs = (target + 86_400 - now_sod) % 86_400;
    Duration::from_secs(if secs == 0 { 86_400 } else { secs as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use raam_model::limits;

    #[test]
    fn sleep_window_wraps_midnight() {
        let s = Schedule {
            enabled: true,
            sleep_min: limits::DEFAULT_SLEEP_MIN,
            wake_min: limits::DEFAULT_WAKE_MIN,
        };
        assert!(s.asleep_at(23 * 60));
        assert!(s.asleep_at(2 * 60));
        assert!(!s.asleep_at(5 * 60));
        assert!(!s.asleep_at(12 * 60));
    }

    #[test]
    fn sleep_window_within_a_day() {
        let s = Schedule {
            enabled: true,
            sleep_min: 13 * 60,
            wake_min: 14 * 60,
        };
        assert!(s.asleep_at(13 * 60 + 30));
        assert!(!s.asleep_at(14 * 60));
    }

    #[test]
    fn disabled_or_empty_never_sleeps() {
        let off = Schedule {
            enabled: false,
            sleep_min: 0,
            wake_min: 1,
        };
        assert!(!off.asleep_at(0));
        let empty = Schedule {
            enabled: true,
            sleep_min: 60,
            wake_min: 60,
        };
        assert!(!empty.asleep_at(60));
    }

    #[test]
    fn hm_round_trip() {
        assert_eq!(parse_hm("23:00"), Some(23 * 60));
        assert_eq!(parse_hm(" 5:07 "), Some(5 * 60 + 7));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(fmt_hm(23 * 60 + 5), "23:05");
    }

    #[test]
    fn until_is_always_in_the_future() {
        assert_eq!(until(60, 0), Duration::from_secs(3600));
        assert_eq!(until(0, 1), Duration::from_secs(86_399));
        assert_eq!(until(0, 0), Duration::from_secs(86_400));
    }
}
