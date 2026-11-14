// SPDX-License-Identifier: GPL-3.0-only

//! Repeating events (`RRULE`): the rule itself, its iCalendar text form, and
//! expansion of a rule into the dates that fall in a queried range.
//!
//! Supported: `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`, `INTERVAL`, `UNTIL`,
//! `COUNT`, and `BYDAY` for weekly rules. A monthly or yearly rule whose day
//! does not exist in some month (the 31st, February 29th) lands on the last
//! day of that month.

use jiff::{
    ToSpan,
    civil::{Date, Weekday},
};

use crate::Event;

/// How often an event repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Frequency {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Frequency {
    pub const ALL: [Frequency; 5] = [
        Frequency::None,
        Frequency::Daily,
        Frequency::Weekly,
        Frequency::Monthly,
        Frequency::Yearly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Frequency::None => "Never",
            Frequency::Daily => "Daily",
            Frequency::Weekly => "Weekly",
            Frequency::Monthly => "Monthly",
            Frequency::Yearly => "Yearly",
        }
    }
}

/// Repeat rule of an event. `until` and `count` both end the series; when
/// both are present the earlier limit wins, as in iCalendar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecurrenceRule {
    pub freq: Frequency,
    /// Every `interval` days/weeks/months/years; 0 is treated as 1.
    pub interval: u32,
    /// Last day an occurrence may fall on (inclusive).
    pub until: Option<Date>,
    /// Total number of occurrences, including the first.
    pub count: Option<u32>,
    /// Weekdays of a weekly rule; empty means the weekday of the first event.
    pub by_day: Vec<Weekday>,
}

/// An event's occurrence on one day: `index` points into the event list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub index: usize,
    pub date: Date,
}

const DAY_CODES: [(&str, Weekday); 7] = [
    ("SU", Weekday::Sunday),
    ("MO", Weekday::Monday),
    ("TU", Weekday::Tuesday),
    ("WE", Weekday::Wednesday),
    ("TH", Weekday::Thursday),
    ("FR", Weekday::Friday),
    ("SA", Weekday::Saturday),
];

/// Hard stop so a malformed rule can never spin forever.
const MAX_PERIODS: i64 = 200_000;

impl RecurrenceRule {
    pub fn of(freq: Frequency) -> Self {
        Self { freq, interval: 1, ..Self::default() }
    }

    pub fn is_none(&self) -> bool {
        self.freq == Frequency::None
    }

    fn step(&self) -> i64 {
        i64::from(self.interval.max(1))
    }

    /// Parse the value of an `RRULE` line (`FREQ=WEEKLY;COUNT=4`). `parse_until`
    /// turns an `UNTIL` value into a date. Unsupported frequencies give `None`.
    pub fn parse(value: &str, parse_until: impl Fn(&str) -> Option<Date>) -> Option<Self> {
        let mut rule = Self::default();
        for part in value.split(';') {
            let (key, val) = part.split_once('=')?;
            match key.to_ascii_uppercase().as_str() {
                "FREQ" => {
                    rule.freq = match val.to_ascii_uppercase().as_str() {
                        "DAILY" => Frequency::Daily,
                        "WEEKLY" => Frequency::Weekly,
                        "MONTHLY" => Frequency::Monthly,
                        "YEARLY" => Frequency::Yearly,
                        _ => return None,
                    }
                }
                "INTERVAL" => rule.interval = val.parse().ok().filter(|n| *n > 0)?,
                "COUNT" => rule.count = Some(val.parse().ok()?),
                "UNTIL" => rule.until = parse_until(val),
                "BYDAY" => {
                    rule.by_day = val
                        .split(',')
                        .filter_map(|day| {
                            // Ordinals such as 2TU are not supported; keep the weekday.
                            let code = day.get(day.len().saturating_sub(2)..)?;
                            DAY_CODES
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case(code))
                                .map(|(_, weekday)| *weekday)
                        })
                        .collect();
                }
                _ => {}
            }
        }
        if rule.interval == 0 {
            rule.interval = 1;
        }
        (!rule.is_none()).then_some(rule)
    }

    /// The `RRULE` value, or `None` for an event that does not repeat.
    pub fn to_value(&self) -> Option<String> {
        let freq = match self.freq {
            Frequency::None => return None,
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        };
        let mut out = format!("FREQ={freq}");
        if self.interval > 1 {
            out.push_str(&format!(";INTERVAL={}", self.interval));
        }
        if let Some(until) = self.until {
            out.push_str(&format!(
                ";UNTIL={:04}{:02}{:02}",
                until.year(),
                until.month(),
                until.day()
            ));
        }
        if let Some(count) = self.count {
            out.push_str(&format!(";COUNT={count}"));
        }
        if self.freq == Frequency::Weekly && !self.by_day.is_empty() {
            let days: Vec<&str> = self
                .by_day
                .iter()
                .filter_map(|day| DAY_CODES.iter().find(|(_, w)| w == day).map(|(code, _)| *code))
                .collect();
            out.push_str(&format!(";BYDAY={}", days.join(",")));
        }
        Some(out)
    }

    /// Dates of period number `k` (0 is the period of `start`), ascending.
    fn period_dates(&self, start: Date, k: i64) -> Vec<Date> {
        let offset = k.saturating_mul(self.step());
        match self.freq {
            Frequency::None => Vec::new(),
            Frequency::Daily => start.checked_add(offset.days()).into_iter().collect(),
            Frequency::Weekly if self.by_day.is_empty() => {
                start.checked_add((offset * 7).days()).into_iter().collect()
            }
            Frequency::Weekly => {
                let week = crate::week_start(start);
                let mut days: Vec<i64> = self
                    .by_day
                    .iter()
                    .map(|day| i64::from(day.to_sunday_zero_offset()))
                    .collect();
                days.sort_unstable();
                days.dedup();
                days.into_iter()
                    .filter_map(|day| week.checked_add((offset * 7 + day).days()).ok())
                    .filter(|date| *date >= start)
                    .collect()
            }
            Frequency::Monthly | Frequency::Yearly => {
                let months = if self.freq == Frequency::Yearly { offset * 12 } else { offset };
                let Ok(first) = start.first_of_month().checked_add(months.months()) else {
                    return Vec::new();
                };
                let day = start.day().min(first.days_in_month());
                // Feb 29 repeated yearly: clamp to the month's last day.
                Date::new(first.year(), first.month(), day).into_iter().collect()
            }
        }
    }

    /// First period that can matter for dates on or after `from`, minus slack.
    fn first_period(&self, start: Date, from: Date) -> i64 {
        if from <= start {
            return 0;
        }
        let months = i64::from(from.year() - start.year()) * 12
            + i64::from(from.month()) - i64::from(start.month());
        let periods = match self.freq {
            Frequency::None => 0,
            Frequency::Daily => (from - start).get_days() as i64 / self.step(),
            Frequency::Weekly => (from - start).get_days() as i64 / (7 * self.step()),
            Frequency::Monthly => months / self.step(),
            Frequency::Yearly => months / (12 * self.step()),
        };
        (periods - 1).max(0)
    }

    /// Dates of the series that starts on `start` and fall in `from..=to`.
    pub fn occurrences(&self, start: Date, from: Date, to: Date) -> Vec<Date> {
        let mut found = Vec::new();
        if to < from {
            return found;
        }
        if self.is_none() {
            if (from..=to).contains(&start) {
                found.push(start);
            }
            return found;
        }
        let limit = self.until.map_or(to, |until| until.min(to));
        // A count has to be followed from the beginning; otherwise skip ahead.
        let first = if self.count.is_some() { 0 } else { self.first_period(start, from) };
        let mut seen: u64 = 0;
        for k in first..first.saturating_add(MAX_PERIODS) {
            let dates = self.period_dates(start, k);
            if dates.is_empty() && self.by_day.is_empty() {
                // Past the end of the supported calendar.
                break;
            }
            for date in dates {
                if date > limit || self.count.is_some_and(|count| seen >= u64::from(count)) {
                    return found;
                }
                seen += 1;
                if date >= from {
                    found.push(date);
                }
            }
        }
        found
    }
}

/// Every occurrence of `events` that falls in `from..=to`, sorted by day and
/// start time (all-day first).
pub fn expand(events: &[Event], from: Date, to: Date) -> Vec<Occurrence> {
    let mut all: Vec<Occurrence> = events
        .iter()
        .enumerate()
        .flat_map(|(index, event)| {
            event
                .recurrence
                .occurrences(event.date, from, to)
                .into_iter()
                .map(move |date| Occurrence { index, date })
        })
        .collect();
    all.sort_by_key(|o| (o.date, events[o.index].start, o.index));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn rule(value: &str) -> RecurrenceRule {
        RecurrenceRule::parse(value, |v| {
            let digits = v.get(..8)?;
            Some(date(
                digits.get(..4)?.parse().ok()?,
                digits.get(4..6)?.parse().ok()?,
                digits.get(6..8)?.parse().ok()?,
            ))
        })
        .unwrap()
    }

    #[test]
    fn parse_and_serialize_round_trip() {
        let parsed = rule("FREQ=WEEKLY;INTERVAL=2;UNTIL=20261231;BYDAY=MO,WE");
        assert_eq!(parsed.freq, Frequency::Weekly);
        assert_eq!(parsed.interval, 2);
        assert_eq!(parsed.until, Some(date(2026, 12, 31)));
        assert_eq!(parsed.by_day, vec![Weekday::Monday, Weekday::Wednesday]);
        assert_eq!(
            parsed.to_value().unwrap(),
            "FREQ=WEEKLY;INTERVAL=2;UNTIL=20261231;BYDAY=MO,WE"
        );
        assert_eq!(rule("FREQ=DAILY;COUNT=3").count, Some(3));
        assert!(RecurrenceRule::parse("FREQ=HOURLY", |_| None).is_none());
        assert!(RecurrenceRule::default().to_value().is_none());
    }

    #[test]
    fn daily_crosses_month_and_year_boundary() {
        let r = RecurrenceRule::of(Frequency::Daily);
        let days = r.occurrences(date(2025, 12, 29), date(2025, 12, 30), date(2026, 1, 2));
        assert_eq!(
            days,
            vec![date(2025, 12, 30), date(2025, 12, 31), date(2026, 1, 1), date(2026, 1, 2)]
        );
    }

    #[test]
    fn nothing_before_the_first_event() {
        let r = RecurrenceRule::of(Frequency::Daily);
        assert!(r.occurrences(date(2026, 3, 10), date(2026, 3, 1), date(2026, 3, 9)).is_empty());
    }

    #[test]
    fn weekly_by_day_and_interval() {
        let mut r = RecurrenceRule::of(Frequency::Weekly);
        r.by_day = vec![Weekday::Monday, Weekday::Friday];
        // 2026-01-05 is a Monday.
        let days = r.occurrences(date(2026, 1, 5), date(2026, 1, 1), date(2026, 1, 20));
        assert_eq!(
            days,
            vec![
                date(2026, 1, 5),
                date(2026, 1, 9),
                date(2026, 1, 12),
                date(2026, 1, 16),
                date(2026, 1, 19)
            ]
        );
        r.by_day.clear();
        r.interval = 2;
        let days = r.occurrences(date(2026, 1, 5), date(2026, 1, 1), date(2026, 2, 10));
        assert_eq!(days, vec![date(2026, 1, 5), date(2026, 1, 19), date(2026, 2, 2)]);
    }

    #[test]
    fn monthly_clamps_to_last_day() {
        let r = RecurrenceRule::of(Frequency::Monthly);
        let days = r.occurrences(date(2026, 1, 31), date(2026, 1, 1), date(2026, 5, 31));
        assert_eq!(
            days,
            vec![
                date(2026, 1, 31),
                date(2026, 2, 28),
                date(2026, 3, 31),
                date(2026, 4, 30),
                date(2026, 5, 31)
            ]
        );
    }

    #[test]
    fn yearly_leap_day_and_far_query() {
        let r = RecurrenceRule::of(Frequency::Yearly);
        let days = r.occurrences(date(2024, 2, 29), date(2027, 1, 1), date(2029, 12, 31));
        assert_eq!(days, vec![date(2027, 2, 28), date(2028, 2, 29), date(2029, 2, 28)]);
    }

    #[test]
    fn count_and_until_limit_the_series() {
        let mut r = RecurrenceRule::of(Frequency::Daily);
        r.count = Some(3);
        let far = r.occurrences(date(2026, 6, 1), date(2026, 1, 1), date(2030, 1, 1));
        assert_eq!(far, vec![date(2026, 6, 1), date(2026, 6, 2), date(2026, 6, 3)]);
        // Count is relative to the series start, not to the queried range.
        let later = r.occurrences(date(2026, 6, 1), date(2026, 6, 3), date(2030, 1, 1));
        assert_eq!(later, vec![date(2026, 6, 3)]);

        r.count = None;
        r.until = Some(date(2026, 6, 2));
        assert_eq!(r.occurrences(date(2026, 6, 1), date(2026, 6, 1), date(2030, 1, 1)).len(), 2);
    }

    #[test]
    fn expand_sorts_across_events() {
        let mut daily = Event::new(date(2026, 3, 1), "Standup");
        daily.start = Some((9, 0));
        daily.recurrence = RecurrenceRule::of(Frequency::Daily);
        let mut once = Event::new(date(2026, 3, 2), "Lunch");
        once.start = Some((8, 0));
        let events = vec![daily, once];
        let found = expand(&events, date(2026, 3, 2), date(2026, 3, 3));
        let got: Vec<(usize, Date)> = found.iter().map(|o| (o.index, o.date)).collect();
        assert_eq!(got, vec![(1, date(2026, 3, 2)), (0, date(2026, 3, 2)), (0, date(2026, 3, 3))]);
    }
}
