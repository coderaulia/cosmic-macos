// SPDX-License-Identifier: GPL-3.0-only

//! Offline sunrise and sunset using the NOAA solar calculator equations.

/// Zenith angle of sunrise/sunset: 90 degrees plus refraction and solar radius.
const ZENITH: f64 = 90.833;

/// Julian day number (at noon) of a Gregorian date.
fn julian_day_number(year: i32, month: i32, day: i32) -> f64 {
    let a = (14 - month) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    f64::from(day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045)
}

/// Sunrise and sunset as minutes after 00:00 UTC of the given date
/// (values may fall outside 0..1440 for far-off longitudes). Longitude is
/// positive east. `None` during polar day or polar night.
pub fn sunrise_sunset_utc(year: i32, month: i32, day: i32, lat: f64, lon: f64) -> Option<(f64, f64)> {
    // Julian century at the approximate local solar noon.
    let jd = julian_day_number(year, month, day) - lon / 360.0;
    let t = (jd - 2_451_545.0) / 36525.0;

    let l0 = (280.46646 + t * (36000.76983 + t * 0.0003032)).rem_euclid(360.0);
    let m = 357.52911 + t * (35999.05029 - 0.0001537 * t);
    let e = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let (mr, l0r) = (m.to_radians(), l0.to_radians());
    let c = mr.sin() * (1.914602 - t * (0.004817 + 0.000014 * t))
        + (2.0 * mr).sin() * (0.019993 - 0.000101 * t)
        + (3.0 * mr).sin() * 0.000289;
    let true_long = l0 + c;
    let omega = 125.04 - 1934.136 * t;
    let lambda = (true_long - 0.00569 - 0.00478 * omega.to_radians().sin()).to_radians();
    let seconds = 21.448 - t * (46.815 + t * (0.00059 - t * 0.001813));
    let obliq0 = 23.0 + (26.0 + seconds / 60.0) / 60.0;
    let obliq = (obliq0 + 0.00256 * omega.to_radians().cos()).to_radians();
    let decl = (obliq.sin() * lambda.sin()).asin();

    let y = (obliq / 2.0).tan().powi(2);
    let eq_time = 4.0
        * (y * (2.0 * l0r).sin() - 2.0 * e * mr.sin() + 4.0 * e * y * mr.sin() * (2.0 * l0r).cos()
            - 0.5 * y * y * (4.0 * l0r).sin()
            - 1.25 * e * e * (2.0 * mr).sin())
        .to_degrees();

    let latr = lat.to_radians();
    let cos_ha = ZENITH.to_radians().cos() / (latr.cos() * decl.cos()) - latr.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) {
        return None;
    }
    let ha = cos_ha.acos().to_degrees();
    let noon = 720.0 - 4.0 * lon - eq_time;
    Some((noon - 4.0 * ha, noon + 4.0 * ha))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(minutes: f64) -> String {
        let m = minutes.rem_euclid(1440.0).round() as i32;
        format!("{:02}:{:02}", m / 60, m % 60)
    }

    fn assert_near(actual: f64, expected_hhmm: &str) {
        let (h, m) = expected_hhmm.split_once(':').unwrap();
        let expected = f64::from(h.parse::<i32>().unwrap() * 60 + m.parse::<i32>().unwrap());
        assert!(
            (actual.rem_euclid(1440.0) - expected).abs() <= 3.0,
            "got {} expected {expected_hhmm}",
            hm(actual)
        );
    }

    #[test]
    fn london_summer_solstice() {
        // 2024-06-21: sunrise 04:43 BST, sunset 21:21 BST (UTC+1).
        let (rise, set) = sunrise_sunset_utc(2024, 6, 21, 51.5074, -0.1278).unwrap();
        assert_near(rise, "03:43");
        assert_near(set, "20:21");
    }

    #[test]
    fn london_winter_solstice() {
        // 2024-12-21: sunrise 08:04 GMT, sunset 15:53 GMT.
        let (rise, set) = sunrise_sunset_utc(2024, 12, 21, 51.5074, -0.1278).unwrap();
        assert_near(rise, "08:04");
        assert_near(set, "15:53");
    }

    #[test]
    fn jakarta_is_near_twelve_hours() {
        let (rise, set) = sunrise_sunset_utc(2024, 9, 22, -6.2, 106.8).unwrap();
        assert!((set - rise - 720.0).abs() < 15.0);
        // Local solar noon is about 04:46 UTC (11:46 WIB).
        assert_near((rise + set) / 2.0, "04:46");
    }

    #[test]
    fn polar_cases() {
        assert!(sunrise_sunset_utc(2024, 6, 21, 80.0, 0.0).is_none());
        assert!(sunrise_sunset_utc(2024, 12, 21, 80.0, 0.0).is_none());
    }
}
