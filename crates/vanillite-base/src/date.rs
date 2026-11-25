//! Dates as Vanilla writes them in its JSON files (`yyyy-MM-dd HH:mm:ss Z`), in UTC.

/// Year, month, day, hour, minute, second (UTC) of Unix time `secs` (`civil_from_days`).
pub fn utc(secs: u64) -> (i64, u8, u8, u8, u8, u8) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe.saturating_add(era.saturating_mul(400)) + i64::from(month <= 2);
    let small = |v: i64| u8::try_from(v).unwrap_or(0);
    let part = |v: u64| u8::try_from(v).unwrap_or(0);
    (
        year,
        small(month),
        small(day),
        part(rest / 3600),
        part(rest / 60 % 60),
        part(rest % 60),
    )
}

/// Unix seconds of `yyyy-MM-dd HH:mm:ss Z` (`BanListEntry.DATE_FORMAT`); `None` for other
/// text (`forever`) and dates before 1970.
pub fn parse_date(text: &str) -> Option<u64> {
    let (date, rest) = text.trim().split_once(' ')?;
    let (time, zone) = rest.split_once(' ')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, mo, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&day) || !(0..=9999).contains(&y) {
        return None;
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let digits = zone.trim_start_matches(['+', '-']);
    let offset = match (digits.get(..2), digits.get(2..)) {
        (Some(hh), Some(mm)) if digits.len() == 4 => {
            sign * (hh.parse::<i64>().ok()? * 3600 + mm.parse::<i64>().ok()? * 60)
        }
        _ => 0,
    };
    // `days_from_civil`.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy.rem_euclid(400);
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let clock = h.clamp(0, 23) * 3600 + mi.clamp(0, 59) * 60 + s.clamp(0, 59);
    u64::try_from(days * 86_400 + clock - offset).ok()
}

/// `yyyy-MM-dd HH:mm:ss +0000` of Unix time `secs`.
pub fn format_date(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = utc(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} +0000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("1970-01-02 00:00:00 +0000"), Some(86_400));
        assert_eq!(parse_date("1970-01-01 01:00:00 +0100"), Some(0));
        assert_eq!(parse_date("forever"), None);
        let t = parse_date("2026-11-24 13:05:09 +0000").unwrap_or(0);
        assert_eq!(utc(t), (2026, 11, 24, 13, 5, 9));
        assert_eq!(format_date(t), "2026-11-24 13:05:09 +0000");
    }
}
