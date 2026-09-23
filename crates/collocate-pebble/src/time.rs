fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn rfc3339_nanos(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once('T')?;
    let mut dp = date.split('-');
    let (y, mo, d): (i64, i64, i64) = (dp.next()?.parse().ok()?, dp.next()?.parse().ok()?, dp.next()?.parse().ok()?);
    let tz_at = rest.find(['Z', 'z', '+', '-'])?;
    let (clock, tz) = rest.split_at(tz_at);
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, ""));
    let mut tp = hms.split(':');
    let (h, mi, sec): (i64, i64, i64) = (tp.next()?.parse().ok()?, tp.next()?.parse().ok()?, tp.next()?.parse().ok()?);
    let mut nanos: i64 = 0;
    for (i, c) in frac.chars().take(9).enumerate() {
        nanos += i64::from(c.to_digit(10)?) * 10i64.pow(8 - i as u32);
    }
    let offset = match tz.chars().next()? {
        'Z' | 'z' => 0,
        sign => {
            let (oh, om) = tz[1..].split_once(':')?;
            let secs = oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60;
            if sign == '-' {
                -secs
            } else {
                secs
            }
        }
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec - offset;
    u64::try_from(secs * 1_000_000_000 + nanos).ok()
}
