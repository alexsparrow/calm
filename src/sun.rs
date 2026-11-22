//! Where the sun is: elevation by date, time and place, for the daylight rail.
//!
//! The solar position uses NOAA's general solar position equations (fractional-year
//! Fourier series for the equation of time and declination); accurate to well under a
//! degree, which is far finer than a rail can show.
//!
//! Location: calm.kdl's `location lat= lon=`, else the coordinates the time-zone database
//! gives for your zone (zone1970.tab), the zone's principal city.

use chrono::{Datelike, NaiveDate};

/// Latitude and longitude in degrees (north and east positive).
pub type Place = (f32, f32);

/// Sun elevation in degrees at `minute` (local, primary zone) of `date`, where
/// `utc_offset` is that zone's offset in minutes.
pub fn elevation((lat, lon): Place, date: NaiveDate, minute: f32, utc_offset: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let n = date.ordinal() as f32;
    let hour_utc = (minute - utc_offset) / 60.;
    let g = TAU / 365. * (n - 1. + (hour_utc - 12.) / 24.);
    let eqtime = 229.18 * (0.000075 + 0.001868 * g.cos() - 0.032077 * g.sin() - 0.014615 * (2. * g).cos() - 0.040849 * (2. * g).sin());
    let decl = 0.006918 - 0.399912 * g.cos() + 0.070257 * g.sin() - 0.006758 * (2. * g).cos() + 0.000907 * (2. * g).sin()
        - 0.002697 * (3. * g).cos()
        + 0.00148 * (3. * g).sin();
    // True solar time, in minutes, and the hour angle.
    let tst = minute + eqtime + 4. * lon - utc_offset;
    let ha = (tst / 4. - 180.).to_radians();
    let lat = lat.to_radians();
    let cos_zenith = (lat.sin() * decl.sin() + lat.cos() * decl.cos() * ha.cos()).clamp(-1., 1.);
    90. - cos_zenith.acos() * 180. / PI
}

/// Daylight as 0..1: nothing below civil twilight (−6°), full from 30° up.
pub fn daylight(elevation: f32) -> f32 {
    ((elevation + 6.) / 36.).clamp(0., 1.)
}

/// The zone's principal-city coordinates from the system time-zone database.
pub fn zone_place(zone: &str) -> Option<Place> {
    let dirs = std::env::var("TZDIR").into_iter().chain(["/usr/share/zoneinfo".into(), "/etc/zoneinfo".into()]);
    for dir in dirs {
        for table in ["zone1970.tab", "zone.tab"] {
            let Ok(text) = std::fs::read_to_string(std::path::Path::new(&dir).join(table)) else { continue };
            for line in text.lines().filter(|l| !l.starts_with('#')) {
                let mut cols = line.split('\t');
                let (Some(_), Some(coords), Some(name)) = (cols.next(), cols.next(), cols.next()) else { continue };
                if name == zone {
                    return parse_iso6709(coords);
                }
            }
        }
    }
    None
}

/// `+143512+1205804` or `+5130-00007` (ISO 6709, degrees-minutes[-seconds]).
fn parse_iso6709(s: &str) -> Option<Place> {
    let split = s[1..].find(['+', '-'])? + 1;
    let (lat, lon) = s.split_at(split);
    let part = |p: &str, deg_digits: usize| -> Option<f32> {
        let sign = if p.starts_with('-') { -1. } else { 1. };
        let d = &p[1..];
        let deg: f32 = d.get(..deg_digits)?.parse().ok()?;
        let min: f32 = d.get(deg_digits..deg_digits + 2)?.parse().ok()?;
        let sec: f32 = d.get(deg_digits + 2..deg_digits + 4).and_then(|x| x.parse().ok()).unwrap_or(0.);
        Some(sign * (deg + min / 60. + sec / 3600.))
    };
    Some((part(lat, 2)?, part(lon, 3)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zone_coordinates() {
        let (lat, lon) = parse_iso6709("+143512+1205804").unwrap();
        assert!((lat - 14.587).abs() < 0.01 && (lon - 120.968).abs() < 0.01);
        let (lat, lon) = parse_iso6709("+513030-0000731").unwrap();
        assert!((lat - 51.508).abs() < 0.01 && (lon + 0.125).abs() < 0.01);
    }

    #[test]
    fn sun_is_up_at_noon_and_down_at_midnight() {
        let manila = (14.59, 120.98);
        let date = NaiveDate::from_ymd_opt(2026, 11, 22).unwrap();
        let noon = elevation(manila, date, 12. * 60., 480.);
        let midnight = elevation(manila, date, 0., 480.);
        // Late November in Manila: the noon sun is about 90 − 14.6 − 20 ≈ 55° up.
        assert!((50. ..60.).contains(&noon), "{noon}");
        assert!(midnight < -30., "{midnight}");
        // London in midwinter barely clears 15°.
        let london = elevation((51.5, -0.13), NaiveDate::from_ymd_opt(2026, 12, 21).unwrap(), 12. * 60., 0.);
        assert!((13. ..17.).contains(&london), "{london}");
    }
}
