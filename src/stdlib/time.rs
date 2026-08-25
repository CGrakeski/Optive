//! `std.time`：时钟、睡眠、格式化与可选时区。

use num_bigint::BigInt;

use crate::error::RuntimeError;
use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use crate::shared::Shared;

use super::{
    builtin, expect_arity, expect_int, expect_num_f64, expect_text, float_from_f64, io_map,
    submodule,
};

pub(super) fn build_time_module() -> Shared<ModuleObject> {
    submodule(
        "time",
        &[
            ("now", builtin(time_now)),
            ("now_ms", builtin(time_now_ms)),
            ("monotonic", builtin(time_monotonic)),
            ("sleep", builtin(time_sleep)),
            ("sleep_ms", builtin(time_sleep_ms)),
            ("format", builtin(time_format)),
            ("parse", builtin(time_parse)),
            ("utc_parts", builtin(time_utc_parts)),
            ("parts", builtin(time_parts)),
            ("local_offset", builtin(time_local_offset)),
        ],
    )
}

pub(super) fn time_now(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| io_map("now failed", e))?
        .as_secs();
    Ok(Value::Num(Num::Small(secs as i64)))
}

pub(super) fn time_now_ms(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| io_map("now_ms failed", e))?
        .as_millis();
    Ok(Value::Num(Num::from_bigint(BigInt::from(ms))))
}

pub(super) fn time_monotonic(_vm: &mut Vm, _args: &[Value]) -> Result<Value> {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    let secs = start.elapsed().as_secs_f64();
    Ok(Value::Num(float_from_f64(secs)?))
}

pub(super) fn time_sleep(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let secs = if args.is_empty() {
        0.0
    } else {
        expect_num_f64("sleep", args, 0)?
    };
    vm.coop_sleep_secs(secs)
}

pub(super) fn time_sleep_ms(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let ms = if args.is_empty() {
        0
    } else {
        expect_int("sleep_ms", args, 0)?.max(0) as u64
    };
    vm.coop_sleep_ms(ms)
}

/// Unix 日序 → UTC (y, m, d)；算法见 Howard Hinnant。
pub(super) const fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

pub(super) fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i64::from(era) * 146_097 + i64::from(doe) - 719_468
}

pub(super) const fn utc_parts_from_secs(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let day = if secs >= 0 {
        secs / 86400
    } else {
        (secs - 86399) / 86400
    };
    let sod = (secs - day * 86400) as u32;
    let (y, m, d) = civil_from_days(day);
    let hour = sod / 3600;
    let min = (sod % 3600) / 60;
    let sec = sod % 60;
    (y, m, d, hour, min, sec)
}

struct Wall {
    y: i32,
    m: u32,
    d: u32,
    hh: u32,
    mm: u32,
    ss: u32,
    offset: i32,
    name: String,
}

enum ZoneSpec {
    Utc,
    Iana(String),
    Offset(i32),
}

fn format_offset_basic(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let abs = secs.unsigned_abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    format!("{sign}{h:02}{m:02}")
}

fn format_offset_colon(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let abs = secs.unsigned_abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    format!("{sign}{h:02}:{m:02}")
}

fn parse_offset_text(s: &str) -> Option<i32> {
    let s = s.trim();
    let (neg, rest) = if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (true, s.strip_prefix('-')?)
    };
    let (h, m): (u32, u32) = if rest.len() == 4 && rest.chars().all(|c| c.is_ascii_digit()) {
        (rest[..2].parse().ok()?, rest[2..].parse().ok()?)
    } else if rest.len() == 5 && rest.as_bytes().get(2) == Some(&b':') {
        (rest[..2].parse().ok()?, rest[3..].parse().ok()?)
    } else {
        return None;
    };
    if h > 23 || m > 59 {
        return None;
    }
    let secs = (h as i32) * 3600 + (m as i32) * 60;
    Some(if neg { -secs } else { secs })
}

fn parse_zone_text(s: &str) -> Result<ZoneSpec> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("utc") || t == "Z" {
        return Ok(ZoneSpec::Utc);
    }
    if let Some(off) = parse_offset_text(t) {
        return Ok(ZoneSpec::Offset(off));
    }
    match jiff::tz::TimeZone::get(t) {
        Ok(_) => Ok(ZoneSpec::Iana(t.to_string())),
        Err(_) => Err(RuntimeError::value_err(format!("unknown time zone `{t}`"))),
    }
}

fn parse_zone_value(v: &Value) -> Result<ZoneSpec> {
    match v {
        Value::Text(s) => parse_zone_text(s),
        Value::Num(_) => {
            let secs = crate::value::expect_i64("tz", v)?;
            if !(-24 * 3600..=24 * 3600).contains(&secs) {
                return Err(RuntimeError::value_err(
                    "time zone offset must be within ±24 hours",
                ));
            }
            Ok(ZoneSpec::Offset(secs as i32))
        }
        _ => Err(RuntimeError::type_err(
            "time zone must be text (IANA / offset) or num (offset seconds)",
        )),
    }
}

fn optional_zone(args: &[Value], idx: usize) -> Result<ZoneSpec> {
    match args.get(idx) {
        None => Ok(ZoneSpec::Utc),
        Some(v) => parse_zone_value(v),
    }
}

fn wall_from_secs(secs: i64, zone: &ZoneSpec) -> Result<Wall> {
    match zone {
        ZoneSpec::Utc => {
            let (y, m, d, hh, mm, ss) = utc_parts_from_secs(secs);
            Ok(Wall {
                y,
                m,
                d,
                hh,
                mm,
                ss,
                offset: 0,
                name: "UTC".into(),
            })
        }
        ZoneSpec::Offset(off) => {
            let adj = secs.saturating_add(i64::from(*off));
            let (y, m, d, hh, mm, ss) = utc_parts_from_secs(adj);
            Ok(Wall {
                y,
                m,
                d,
                hh,
                mm,
                ss,
                offset: *off,
                name: format_offset_colon(*off),
            })
        }
        ZoneSpec::Iana(name) => {
            let ts = jiff::Timestamp::from_second(secs)
                .map_err(|e| RuntimeError::value_err(format!("timestamp out of range: {e}")))?;
            let tz = jiff::tz::TimeZone::get(name)
                .map_err(|_| RuntimeError::value_err(format!("unknown time zone `{name}`")))?;
            let zoned = ts.to_zoned(tz);
            let dt = zoned.datetime();
            Ok(Wall {
                y: i32::from(dt.year()),
                m: u32::from(dt.month() as u8),
                d: u32::from(dt.day() as u8),
                hh: u32::from(dt.hour() as u8),
                mm: u32::from(dt.minute() as u8),
                ss: u32::from(dt.second() as u8),
                offset: zoned.offset().seconds(),
                name: name.clone(),
            })
        }
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    const DIM: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let Some(&days) = DIM.get(m.saturating_sub(1) as usize) else {
        return 0;
    };
    if m == 2 && is_leap_year(y) {
        29
    } else {
        days
    }
}

fn is_leap_year(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn epoch_from_wall(
    y: i32,
    mo: u32,
    da: u32,
    hh: u32,
    mi: u32,
    se: u32,
    zone: &ZoneSpec,
) -> Result<i64> {
    if !(1..=12).contains(&mo) || hh > 23 || mi > 59 || se > 59 {
        return Err(RuntimeError::value_err(
            "time.parse: out-of-range date/time component",
        ));
    }
    if !(1..=days_in_month(y, mo)).contains(&da) {
        return Err(RuntimeError::value_err(
            "time.parse: out-of-range date/time component",
        ));
    }
    match zone {
        ZoneSpec::Utc => {
            let days = days_from_civil(y, mo, da);
            Ok(days * 86400 + i64::from(hh * 3600 + mi * 60 + se))
        }
        ZoneSpec::Offset(off) => {
            let days = days_from_civil(y, mo, da);
            Ok(days * 86400 + i64::from(hh * 3600 + mi * 60 + se) - i64::from(*off))
        }
        ZoneSpec::Iana(name) => {
            let tz = jiff::tz::TimeZone::get(name)
                .map_err(|_| RuntimeError::value_err(format!("unknown time zone `{name}`")))?;
            let y16 = i16::try_from(y)
                .map_err(|_| RuntimeError::value_err("time.parse: year out of range"))?;
            let dt = jiff::civil::DateTime::new(
                y16,
                i8::try_from(mo).unwrap_or(1),
                i8::try_from(da).unwrap_or(1),
                i8::try_from(hh).unwrap_or(0),
                i8::try_from(mi).unwrap_or(0),
                i8::try_from(se).unwrap_or(0),
                0,
            )
            .map_err(|e| RuntimeError::value_err(format!("time.parse: invalid datetime: {e}")))?;
            let zoned = dt.to_zoned(tz).map_err(|e| {
                RuntimeError::value_err(format!("time.parse: cannot place in `{name}`: {e}"))
            })?;
            Ok(zoned.timestamp().as_second())
        }
    }
}

fn parts_dict(wall: &Wall) -> Value {
    let mut map = DictMap::new();
    map.insert(
        ValueKey::Text("year".into()),
        Value::Num(Num::Small(i64::from(wall.y))),
    );
    map.insert(
        ValueKey::Text("month".into()),
        Value::Num(Num::Small(i64::from(wall.m))),
    );
    map.insert(
        ValueKey::Text("day".into()),
        Value::Num(Num::Small(i64::from(wall.d))),
    );
    map.insert(
        ValueKey::Text("hour".into()),
        Value::Num(Num::Small(i64::from(wall.hh))),
    );
    map.insert(
        ValueKey::Text("minute".into()),
        Value::Num(Num::Small(i64::from(wall.mm))),
    );
    map.insert(
        ValueKey::Text("second".into()),
        Value::Num(Num::Small(i64::from(wall.ss))),
    );
    Value::Dict(Shared::new(map))
}

pub(super) fn time_utc_parts(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect_arity("utc_parts", args, 1)?;
    let secs = expect_int("utc_parts", args, 0)?;
    let (y, m, d, hh, mm, ss) = utc_parts_from_secs(secs);
    Ok(parts_dict(&Wall {
        y,
        m,
        d,
        hh,
        mm,
        ss,
        offset: 0,
        name: "UTC".into(),
    }))
}

pub(super) fn time_parts(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(RuntimeError::type_err(
            "parts requires 1 or 2 arguments (secs[, tz])",
        ));
    }
    let secs = expect_int("parts", args, 0)?;
    let zone = optional_zone(args, 1)?;
    let wall = wall_from_secs(secs, &zone)?;
    Ok(parts_dict(&wall))
}

pub(super) fn time_local_offset(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if !args.is_empty() {
        return Err(RuntimeError::type_err("local_offset takes no arguments"));
    }
    let ts = jiff::Timestamp::now();
    let tz = jiff::tz::TimeZone::system();
    let off = ts.to_zoned(tz).offset().seconds();
    Ok(Value::Num(Num::Small(i64::from(off))))
}

fn format_with_wall(fmt: &str, wall: &Wall) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{:04}", wall.y)),
            Some('m') => out.push_str(&format!("{:02}", wall.m)),
            Some('d') => out.push_str(&format!("{:02}", wall.d)),
            Some('H') => out.push_str(&format!("{:02}", wall.hh)),
            Some('M') => out.push_str(&format!("{:02}", wall.mm)),
            Some('S') => out.push_str(&format!("{:02}", wall.ss)),
            Some('z') => out.push_str(&format_offset_basic(wall.offset)),
            Some(':') if chars.peek() == Some(&'z') => {
                chars.next();
                out.push_str(&format_offset_colon(wall.offset));
            }
            Some('Z') => out.push_str(&wall.name),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

pub(super) fn time_format(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 && args.len() != 3 {
        return Err(RuntimeError::type_err(
            "format requires 2 or 3 arguments (secs, fmt[, tz])",
        ));
    }
    let secs = expect_int("format", args, 0)?;
    let fmt = expect_text("format", args, 1)?;
    let zone = optional_zone(args, 2)?;
    let wall = wall_from_secs(secs, &zone)?;
    Ok(Value::Text(format_with_wall(&fmt, &wall)))
}

pub(super) fn time_take_digits(
    it: &mut std::iter::Peekable<std::str::Chars<'_>>,
    n: usize,
) -> Result<u32> {
    let mut s = String::new();
    for _ in 0..n {
        match it.next() {
            Some(c) if c.is_ascii_digit() => s.push(c),
            _ => {
                return Err(RuntimeError::value_err("time.parse: expected digits"));
            }
        }
    }
    s.parse::<u32>()
        .map_err(|_| RuntimeError::value_err("time.parse: invalid number"))
}

fn take_offset_basic(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<i32> {
    let mut s = String::new();
    for _ in 0..5 {
        match it.next() {
            Some(c) => s.push(c),
            None => return Err(RuntimeError::value_err("time.parse: expected %z offset")),
        }
    }
    parse_offset_text(&s).ok_or_else(|| RuntimeError::value_err("time.parse: invalid %z offset"))
}

fn take_offset_colon(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<i32> {
    let mut s = String::new();
    for _ in 0..6 {
        match it.next() {
            Some(c) => s.push(c),
            None => return Err(RuntimeError::value_err("time.parse: expected %:z offset")),
        }
    }
    parse_offset_text(&s).ok_or_else(|| RuntimeError::value_err("time.parse: invalid %:z offset"))
}

fn take_zone_name(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<ZoneSpec> {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '+' || c == '-' {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    if s.is_empty() {
        return Err(RuntimeError::value_err("time.parse: expected %Z zone"));
    }
    parse_zone_text(&s)
}

pub(super) fn time_parse(_vm: &mut Vm, args: &[Value]) -> Result<Value> {
    if args.len() != 2 && args.len() != 3 {
        return Err(RuntimeError::type_err(
            "parse requires 2 or 3 arguments (text, fmt[, tz])",
        ));
    }
    let text = expect_text("parse", args, 0)?;
    let fmt = expect_text("parse", args, 1)?;
    let mut zone = optional_zone(args, 2)?;
    let mut yi = 1970i32;
    let mut mo = 1u32;
    let mut da = 1u32;
    let mut hh = 0u32;
    let mut mi = 0u32;
    let mut se = 0u32;
    let mut ti = text.chars().peekable();
    let mut fi = fmt.chars().peekable();
    while let Some(fc) = fi.next() {
        if fc == '%' {
            let spec = fi
                .next()
                .ok_or_else(|| RuntimeError::value_err("time.parse: trailing % in format"))?;
            match spec {
                'Y' => yi = time_take_digits(&mut ti, 4)? as i32,
                'm' => mo = time_take_digits(&mut ti, 2)?,
                'd' => da = time_take_digits(&mut ti, 2)?,
                'H' => hh = time_take_digits(&mut ti, 2)?,
                'M' => mi = time_take_digits(&mut ti, 2)?,
                'S' => se = time_take_digits(&mut ti, 2)?,
                'z' => zone = ZoneSpec::Offset(take_offset_basic(&mut ti)?),
                ':' if fi.peek() == Some(&'z') => {
                    fi.next();
                    zone = ZoneSpec::Offset(take_offset_colon(&mut ti)?);
                }
                'Z' => zone = take_zone_name(&mut ti)?,
                '%' => {
                    if ti.next() != Some('%') {
                        return Err(RuntimeError::value_err("time.parse: expected '%'"));
                    }
                }
                other => {
                    return Err(RuntimeError::value_err(format!(
                        "time.parse: unsupported format %{other}"
                    )))
                }
            }
        } else {
            let got = ti.next();
            if got != Some(fc) {
                return Err(RuntimeError::value_err(format!(
                    "time.parse: expected '{fc}', got {got:?}"
                )));
            }
        }
    }
    if ti.next().is_some() {
        return Err(RuntimeError::value_err("time.parse: trailing input"));
    }
    let secs = epoch_from_wall(yi, mo, da, hh, mi, se, &zone)?;
    Ok(Value::Num(Num::Small(secs)))
}
