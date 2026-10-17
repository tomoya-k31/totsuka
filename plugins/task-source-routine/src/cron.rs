//! The five-field cron schedules routines run on (#918).
//!
//! `minute hour day-of-month month day-of-week`, each field a `*`, a number,
//! a range `a-b`, a step `*/n` / `a-b/n`, or a comma list of those. Day of
//! week is `0`–`7` with both `0` and `7` meaning Sunday. As in Vixie cron,
//! when both day fields are restricted a day matching **either** runs.
//!
//! Names (`MON`, `JAN`) and the `@daily` shorthands are not accepted.
//!
//! Times are local wall-clock time, matched minute by minute: the latest
//! slot is found by walking back from now one minute at a time, so there is
//! no "next fire time" arithmetic to get wrong around month ends.

use std::str::FromStr;

use time::{Duration, OffsetDateTime};

/// A parsed schedule: one bit set per field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    /// Whether day-of-month / day-of-week were written as `*`, which decides
    /// how the two combine.
    days_any: bool,
    weekdays_any: bool,
}

impl FromStr for Schedule {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let fields: Vec<&str> = s.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(format!(
                "`{s}` has {} fields; a schedule has 5 (minute hour day month weekday)",
                fields.len()
            ));
        };
        // Sunday is both 0 and 7; fold 7 onto 0.
        let weekdays = field(weekday, "weekday", 0, 7)?;
        Ok(Self {
            minutes: field(minute, "minute", 0, 59)?,
            hours: field(hour, "hour", 0, 23)? as u32,
            days: field(day, "day", 1, 31)? as u32,
            months: field(month, "month", 1, 12)? as u16,
            weekdays: ((weekdays | (weekdays >> 7)) & 0x7f) as u8,
            days_any: day == "*",
            weekdays_any: weekday == "*",
        })
    }
}

/// Parse one field into a bit set over `min..=max`.
fn field(text: &str, name: &str, min: u32, max: u32) -> Result<u64, String> {
    let bad = |why: &str| format!("{name} field `{text}`: {why}");
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step.parse().map_err(|_| bad("the step is not a number"))?;
                if step == 0 {
                    return Err(bad("the step is 0"));
                }
                (range, step)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                number(a, min, max).ok_or_else(|| bad("not a number in range"))?,
                number(b, min, max).ok_or_else(|| bad("not a number in range"))?,
            )
        } else {
            let n = number(range, min, max).ok_or_else(|| {
                bad(&format!(
                    "expected `*`, a number from {min} to {max}, `a-b` or `*/n`"
                ))
            })?;
            // `5/15` means "from 5, every 15", as in Vixie cron.
            (n, if step > 1 { max } else { n })
        };
        if from > to {
            return Err(bad("the range runs backwards"));
        }
        for n in (from..=to).step_by(step as usize) {
            bits |= 1 << n;
        }
    }
    Ok(bits)
}

fn number(text: &str, min: u32, max: u32) -> Option<u32> {
    text.parse().ok().filter(|n| (min..=max).contains(n))
}

impl Schedule {
    /// Whether the minute `t` (in local time) is one of this schedule's.
    pub fn matches(&self, t: OffsetDateTime) -> bool {
        let has = |bits: u64, n: u8| bits & (1 << n) != 0;
        let day = has(self.days.into(), t.day());
        let weekday = has(self.weekdays.into(), t.weekday().number_days_from_sunday());
        let day_ok = match (self.days_any, self.weekdays_any) {
            (false, false) => day || weekday,
            _ => day && weekday,
        };
        has(self.minutes, t.minute())
            && has(self.hours.into(), t.hour())
            && has(self.months.into(), u8::from(t.month()))
            && day_ok
    }

    /// The latest slot at or before `now`, no older than `window` — the run
    /// a tick at `now` owes, if any. `now` carries the offset to match in.
    pub fn latest_within(&self, now: OffsetDateTime, window: Duration) -> Option<OffsetDateTime> {
        let floor = now.replace_second(0).ok()?.replace_nanosecond(0).ok()?;
        let minutes = window.whole_minutes().max(0);
        (0..=minutes)
            .map(|back| floor - Duration::minutes(back))
            .find(|t| self.matches(*t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn s(text: &str) -> Schedule {
        text.parse().unwrap()
    }

    #[test]
    fn fields_accept_lists_ranges_and_steps() {
        let every_15 = s("*/15 9-17 * * 1-5");
        assert!(every_15.matches(datetime!(2026-10-19 9:45 +9))); // Monday
        assert!(!every_15.matches(datetime!(2026-10-19 9:46 +9)));
        assert!(!every_15.matches(datetime!(2026-10-18 9:45 +9))); // Sunday
        assert!(!every_15.matches(datetime!(2026-10-19 18:00 +9)));
        let list = s("0,30 8 1,15 1-12/3 *");
        assert!(list.matches(datetime!(2026-10-15 8:30 +9)));
        assert!(!list.matches(datetime!(2026-11-15 8:30 +9)));
        assert!(s("5/20 * * * *").matches(datetime!(2026-10-19 3:45 +9)));
    }

    #[test]
    fn sunday_is_both_zero_and_seven() {
        let sunday = datetime!(2026-10-18 10:00 +9);
        assert!(s("0 10 * * 0").matches(sunday));
        assert!(s("0 10 * * 7").matches(sunday));
    }

    /// Vixie cron: two restricted day fields are OR'd, one restricted field
    /// alone decides.
    #[test]
    fn restricted_day_fields_combine_with_or() {
        let first_or_monday = s("0 9 1 * 1");
        assert!(first_or_monday.matches(datetime!(2026-10-01 9:00 +9))); // Thursday the 1st
        assert!(first_or_monday.matches(datetime!(2026-10-19 9:00 +9))); // a Monday
        assert!(!first_or_monday.matches(datetime!(2026-10-20 9:00 +9)));
        assert!(!s("0 9 1 * *").matches(datetime!(2026-10-19 9:00 +9)));
    }

    #[test]
    fn malformed_schedules_say_which_field() {
        for (text, needle) in [
            ("* * * *", "4 fields"),
            ("60 * * * *", "minute field"),
            ("* 9-8 * * *", "backwards"),
            ("*/0 * * * *", "step is 0"),
            ("* * * JAN *", "month field"),
            ("* * 0 * *", "day field"),
        ] {
            let err = text.parse::<Schedule>().unwrap_err();
            assert!(err.contains(needle), "{text}: {err}");
        }
    }

    #[test]
    fn latest_within_finds_the_owed_slot_and_respects_the_window() {
        let daily_9 = s("0 9 * * *");
        let now = datetime!(2026-10-19 13:20:41 +9);
        assert_eq!(
            daily_9.latest_within(now, Duration::hours(24)),
            Some(datetime!(2026-10-19 9:00 +9))
        );
        // Older than the window: nothing is owed.
        assert_eq!(daily_9.latest_within(now, Duration::hours(4)), None);
        // A zero window is the current minute only.
        assert_eq!(
            daily_9.latest_within(datetime!(2026-10-19 9:00:30 +9), Duration::ZERO),
            Some(datetime!(2026-10-19 9:00 +9))
        );
        assert_eq!(daily_9.latest_within(now, Duration::ZERO), None);
    }
}
