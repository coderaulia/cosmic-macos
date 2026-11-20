// SPDX-License-Identifier: GPL-3.0-only
//! Pure month-grid helpers for the Calendar widget.

use std::collections::HashSet;

use jiff::civil::Date;

use crate::agenda::AgendaEntry;

/// Weeks (Monday first) of the month containing `date`; `None` pads days outside the month.
pub fn month_weeks(date: Date) -> Vec<[Option<i8>; 7]> {
    let offset = date.first_of_month().weekday().to_monday_zero_offset() as usize;
    let days = date.days_in_month();
    let mut weeks = Vec::new();
    let mut week = [None; 7];
    let mut col = offset;
    for day in 1..=days {
        week[col] = Some(day);
        col += 1;
        if col == 7 {
            weeks.push(week);
            week = [None; 7];
            col = 0;
        }
    }
    if col > 0 {
        weeks.push(week);
    }
    weeks
}

/// Days (1-31) of `date`'s month that have at least one event.
pub fn days_with_events(agenda: &[AgendaEntry], date: Date) -> HashSet<i8> {
    agenda
        .iter()
        .filter(|e| e.date.year() == date.year() && e.date.month() == date.month())
        .map(|e| e.date.day())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    fn entry(d: Date) -> AgendaEntry {
        AgendaEntry { date: d, time: None, title: "x".into() }
    }

    #[test]
    fn weeks_for_a_month_starting_midweek() {
        // 2026-01-01 is a Thursday.
        let weeks = month_weeks(date(2026, 1, 15));
        assert_eq!(weeks.len(), 5);
        assert_eq!(weeks[0], [None, None, None, Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(weeks[4][5], Some(31));
        assert_eq!(weeks[4][6], None);
        let total: usize = weeks.iter().flatten().flatten().count();
        assert_eq!(total, 31);
    }

    #[test]
    fn six_weeks_and_exact_fit() {
        // March 2026 starts on Sunday and has 31 days: six rows.
        assert_eq!(month_weeks(date(2026, 3, 1)).len(), 6);
        // February 2027 starts Monday, 28 days: four exact rows.
        let weeks = month_weeks(date(2027, 2, 10));
        assert_eq!(weeks.len(), 4);
        assert!(weeks.iter().flatten().all(Option::is_some));
    }

    #[test]
    fn event_days_only_in_same_month() {
        let agenda = vec![
            entry(date(2026, 1, 3)),
            entry(date(2026, 1, 3)),
            entry(date(2026, 1, 20)),
            entry(date(2026, 2, 5)),
            entry(date(2025, 1, 9)),
        ];
        let days = days_with_events(&agenda, date(2026, 1, 15));
        assert_eq!(days, HashSet::from([3, 20]));
    }
}
