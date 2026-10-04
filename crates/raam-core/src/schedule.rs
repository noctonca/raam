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

/// Minutes after midnight as HH:MM.
pub fn fmt_hm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

pub fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    // `then`, not `then_some`: the sum is only computed in range, since
    // an hour past u32::MAX / 60 would overflow it (a panic in a debug
    // build) before the range check threw it away.
    (h < 24 && m < 60).then(|| h * 60 + m)
}

/// Local seconds after midnight, and the matching unix time (via the Clock).
pub fn local_now() -> (u32, i64) {
    let epoch = crate::clock::wall_secs();
    let t = crate::clock::local(epoch);
    ((t.hour * 3600 + t.min * 60 + t.sec) as u32, epoch)
}

/// Seconds from `now_sod` (seconds after midnight) to the next
/// `target_min`, always in the future (a full day if it is exactly now).
pub fn until_secs(target_min: u32, now_sod: u32) -> u32 {
    debug_assert!(target_min < raam_model::limits::MINUTES_PER_DAY);
    let target = target_min * 60;
    let secs = (target + 86_400 - now_sod) % 86_400;
    if secs == 0 { 86_400 } else { secs }
}

/// `until_secs` as a `Duration`.
pub fn until(target_min: u32, now_sod: u32) -> Duration {
    Duration::from_secs(u64::from(until_secs(target_min, now_sod)))
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

    /// An hour too big for `h * 60` is out of range, not an overflow
    /// panic (found by the property below).
    #[test]
    fn a_huge_hour_is_out_of_range() {
        assert_eq!(parse_hm("71590000:0"), None);
        assert_eq!(parse_hm(&format!("{}:59", u32::MAX)), None);
    }

    #[test]
    fn minutes_after_midnight_read_as_hh_mm() {
        let cases = [
            (0, "00:00"),
            (5, "00:05"),
            (60, "01:00"),
            (1380, "23:00"),
            (1439, "23:59"),
        ];
        for (min, want) in cases {
            assert_eq!(fmt_hm(min), want, "{min}");
        }
    }

    #[test]
    fn until_is_always_in_the_future() {
        assert_eq!(until(60, 0), Duration::from_secs(3600));
        assert_eq!(until(0, 1), Duration::from_secs(86_399));
        assert_eq!(until(0, 0), Duration::from_secs(86_400));
    }

    // Not under miri: proptest reads the working directory (its
    // regressions file) and the OS's randomness, which miri's isolation
    // refuses, and miri is here for unsafe code, which these don't touch.
    #[cfg(not(miri))]
    mod properties {
        use super::*;
        use proptest::prelude::*;

        const DAY_SECS: u32 = limits::MINUTES_PER_DAY * 60;

        fn minute() -> impl Strategy<Value = u32> {
            0..limits::MINUTES_PER_DAY
        }

        /// A schedule that sleeps some of the day: on, with distinct times.
        fn active() -> impl Strategy<Value = Schedule> {
            (minute(), minute())
                .prop_filter("an empty window never sleeps", |(s, w)| s != w)
                .prop_map(|(sleep_min, wake_min)| Schedule {
                    enabled: true,
                    sleep_min,
                    wake_min,
                })
        }

        /// The minute of the day `secs` after `now_sod`, wrapping midnight.
        fn minute_after(now_sod: u32, secs: u64) -> u32 {
            ((u64::from(now_sod) + secs) % u64::from(DAY_SECS)) as u32 / 60
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// `until` lands on its target minute, a full day at most and
            /// never now, from any second of the day.
            #[test]
            fn until_lands_on_the_target_within_a_day(
                target in minute(),
                now_sod in 0..DAY_SECS,
            ) {
                let wait = until(target, now_sod).as_secs();
                prop_assert!(wait > 0 && wait <= u64::from(DAY_SECS));
                prop_assert_eq!((u64::from(now_sod) + wait) % u64::from(DAY_SECS),
                    u64::from(target) * 60);
            }

            /// Asleep now, the wake alarm (`until(wake_min)`) fires at the
            /// first awake minute: every minute before it is asleep.
            #[test]
            fn the_wake_alarm_is_the_end_of_the_sleep(
                sched in active(),
                now_sod in 0..DAY_SECS,
            ) {
                prop_assume!(sched.asleep_at(now_sod / 60));
                let wait = until(sched.wake_min, now_sod).as_secs();
                prop_assert!(!sched.asleep_at(minute_after(now_sod, wait)));
                // Each minute boundary crossed on the way is still asleep.
                let first = 60 - u64::from(now_sod % 60);
                for secs in (first..wait).step_by(60) {
                    prop_assert!(sched.asleep_at(minute_after(now_sod, secs)), "{secs}s on");
                }
            }

            /// The loop's longest wait, the nearer of the two boundaries,
            /// never sleeps through a change: asleep-or-not holds until it
            /// and flips at it.
            #[test]
            fn the_boundary_wait_ends_where_the_state_flips(
                sched in active(),
                now_sod in 0..DAY_SECS,
            ) {
                let wait = until(sched.sleep_min, now_sod)
                    .min(until(sched.wake_min, now_sod))
                    .as_secs();
                let now = sched.asleep_at(now_sod / 60);
                prop_assert_ne!(sched.asleep_at(minute_after(now_sod, wait)), now);
                let first = 60 - u64::from(now_sod % 60);
                for secs in (first..wait).step_by(60) {
                    prop_assert_eq!(sched.asleep_at(minute_after(now_sod, secs)), now,
                        "{}s on", secs);
                }
            }

            /// Off, or with sleep and wake at the same minute, never sleeps.
            #[test]
            fn an_inactive_schedule_never_sleeps(
                enabled: bool,
                sleep_min in minute(),
                wake_min in minute(),
                at in minute(),
            ) {
                let wake_min = if enabled { sleep_min } else { wake_min };
                let sched = Schedule { enabled, sleep_min, wake_min };
                prop_assert!(!sched.asleep_at(at));
            }

            /// A window and its complement split the day: swapping sleep
            /// and wake sleeps exactly the other minutes.
            #[test]
            fn swapped_times_sleep_the_other_minutes(sched in active(), at in minute()) {
                let swapped = Schedule {
                    sleep_min: sched.wake_min,
                    wake_min: sched.sleep_min,
                    ..sched
                };
                prop_assert_ne!(sched.asleep_at(at), swapped.asleep_at(at));
            }

            #[test]
            fn hh_mm_round_trips(min in minute()) {
                prop_assert_eq!(parse_hm(&fmt_hm(min)), Some(min));
            }

            /// Whatever a debug prop holds, it parses to a time of day or
            /// to nothing, and never panics.
            #[test]
            fn any_text_parses_to_a_time_of_day_or_nothing(
                text in "[ ]?[0-9]{0,11}:[0-9]{0,11}[ ]?|\\PC*",
            ) {
                if let Some(min) = parse_hm(&text) {
                    prop_assert!(min < limits::MINUTES_PER_DAY);
                    prop_assert_eq!(parse_hm(&fmt_hm(min)), Some(min));
                }
            }
        }
    }
}
