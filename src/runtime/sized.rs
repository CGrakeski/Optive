//! 定宽整数类型（与 `num` 并列的一等原始类型）。

use std::fmt;

use crate::error::RuntimeError;
use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizedNum {
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Isize(isize),
    Usize(usize),
    F32(f32),
    F64(f64),
}

impl SizedNum {
    pub fn type_name(self) -> &'static str {
        match self {
            Self::I8(_) => "i8",
            Self::U8(_) => "u8",
            Self::I16(_) => "i16",
            Self::U16(_) => "u16",
            Self::I32(_) => "i32",
            Self::U32(_) => "u32",
            Self::I64(_) => "i64",
            Self::U64(_) => "u64",
            Self::Isize(_) => "isize",
            Self::Usize(_) => "usize",
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
        }
    }

    pub fn is_truthy(self) -> bool {
        match self {
            Self::I8(v) => v != 0,
            Self::U8(v) => v != 0,
            Self::I16(v) => v != 0,
            Self::U16(v) => v != 0,
            Self::I32(v) => v != 0,
            Self::U32(v) => v != 0,
            Self::I64(v) => v != 0,
            Self::U64(v) => v != 0,
            Self::Isize(v) => v != 0,
            Self::Usize(v) => v != 0,
            Self::F32(v) => v != 0.0,
            Self::F64(v) => v != 0.0,
        }
    }

    pub fn display_string(self) -> String {
        match self {
            Self::I8(v) => format!("{v}i8"),
            Self::U8(v) => format!("{v}u8"),
            Self::I16(v) => format!("{v}i16"),
            Self::U16(v) => format!("{v}u16"),
            Self::I32(v) => format!("{v}i32"),
            Self::U32(v) => format!("{v}u32"),
            Self::I64(v) => format!("{v}i64"),
            Self::U64(v) => format!("{v}u64"),
            Self::Isize(v) => format!("{v}isize"),
            Self::Usize(v) => format!("{v}usize"),
            Self::F32(v) => format!("{v}f32"),
            Self::F64(v) => format!("{v}f64"),
        }
    }

    pub fn print_string(self) -> String {
        match self {
            Self::I8(v) => v.to_string(),
            Self::U8(v) => v.to_string(),
            Self::I16(v) => v.to_string(),
            Self::U16(v) => v.to_string(),
            Self::I32(v) => v.to_string(),
            Self::U32(v) => v.to_string(),
            Self::I64(v) => v.to_string(),
            Self::U64(v) => v.to_string(),
            Self::Isize(v) => v.to_string(),
            Self::Usize(v) => v.to_string(),
            Self::F32(v) => v.to_string(),
            Self::F64(v) => v.to_string(),
        }
    }

    pub fn to_i64(self) -> Option<i64> {
        match self {
            Self::I8(v) => Some(v as i64),
            Self::U8(v) => Some(v as i64),
            Self::I16(v) => Some(v as i64),
            Self::U16(v) => Some(v as i64),
            Self::I32(v) => Some(v as i64),
            Self::U32(v) => Some(v as i64),
            Self::I64(v) => Some(v),
            Self::U64(v) => i64::try_from(v).ok(),
            Self::Isize(v) => Some(v as i64),
            Self::Usize(v) => i64::try_from(v).ok(),
            Self::F32(_) | Self::F64(_) => None,
        }
    }

    pub fn to_f64(self) -> f64 {
        match self {
            Self::I8(v) => v as f64,
            Self::U8(v) => v as f64,
            Self::I16(v) => v as f64,
            Self::U16(v) => v as f64,
            Self::I32(v) => v as f64,
            Self::U32(v) => v as f64,
            Self::I64(v) => v as f64,
            Self::U64(v) => v as f64,
            Self::Isize(v) => v as f64,
            Self::Usize(v) => v as f64,
            Self::F32(v) => v as f64,
            Self::F64(v) => v,
        }
    }

    pub fn from_literal(text: &str) -> Result<Self> {
        let t = text.trim();
        for suf in LITERAL_SUFFIXES {
            let Some(body) = t.strip_suffix(suf) else {
                continue;
            };
            if body.is_empty() {
                continue;
            }
            // 后缀前必须是数字字面量主体（禁止把标识符误切成后缀）
            let ok = body
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '+' || c == '.')
                && body
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'));
            if !ok {
                continue;
            }
            return match *suf {
                "isize" => parse_isize(body),
                "usize" => parse_usize(body),
                "i64" => parse_i64(body),
                "u64" => parse_u64(body),
                "i32" => parse_i32(body),
                "u32" => parse_u32(body),
                "i16" => parse_i16(body),
                "u16" => parse_u16(body),
                "f64" => parse_f64(body),
                "f32" => parse_f32(body),
                "i8" => parse_i8(body),
                "u8" => parse_u8(body),
                _ => unreachable!("LITERAL_SUFFIXES entry without parser: {suf}"),
            };
        }
        Err(RuntimeError::value_err(format!("invalid sized literal: {text}")))
    }

    pub const ALL_NAMES: &[&str] = &[
        "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64", "isize", "usize", "f32", "f64",
    ];
}

impl fmt::Display for SizedNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_string())
    }
}

macro_rules! parse_int {
    ($name:ident, $ty:ty, $ctor:ident) => {
        fn $name(body: &str) -> Result<SizedNum> {
            body.parse::<$ty>()
                .map(SizedNum::$ctor)
                .map_err(|_| RuntimeError::value_err(format!("invalid {} literal: {body}", stringify!($ty))))
        }
    };
}

parse_int!(parse_i8, i8, I8);
parse_int!(parse_u8, u8, U8);
parse_int!(parse_i16, i16, I16);
parse_int!(parse_u16, u16, U16);
parse_int!(parse_i32, i32, I32);
parse_int!(parse_u32, u32, U32);
parse_int!(parse_i64, i64, I64);
parse_int!(parse_u64, u64, U64);
parse_int!(parse_isize, isize, Isize);
parse_int!(parse_usize, usize, Usize);

fn parse_f32(body: &str) -> Result<SizedNum> {
    body.parse::<f32>()
        .map(SizedNum::F32)
        .map_err(|_| RuntimeError::value_err(format!("invalid f32 literal: {body}")))
}

fn parse_f64(body: &str) -> Result<SizedNum> {
    body.parse::<f64>()
        .map(SizedNum::F64)
        .map_err(|_| RuntimeError::value_err(format!("invalid f64 literal: {body}")))
}

/// 词法 / 字面量后缀表（较长优先，单一来源）。
pub const LITERAL_SUFFIXES: &[&str] = &[
    "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "f64", "f32", "i8", "u8",
];

#[cfg(test)]
mod suffix_sync {
    use super::{SizedNum, LITERAL_SUFFIXES};

    #[test]
    fn literal_suffixes_cover_all_names() {
        assert_eq!(LITERAL_SUFFIXES.len(), SizedNum::ALL_NAMES.len());
        for name in SizedNum::ALL_NAMES {
            assert!(
                LITERAL_SUFFIXES.contains(name),
                "{name} missing from LITERAL_SUFFIXES"
            );
        }
    }

    /// 每个 `LITERAL_SUFFIXES` 条目在 `from_literal` 的 match 中都必须有对应 parser 分支；
    /// 若新增后缀却忘了加 arm，此处会命中 `_ => unreachable!` 而失败。
    #[test]
    fn every_suffix_has_parser() {
        for &suf in LITERAL_SUFFIXES {
            let lit = format!("1{suf}");
            let parsed = SizedNum::from_literal(&lit);
            assert!(parsed.is_ok(), "no parser for suffix `{suf}`: {parsed:?}");
            assert_eq!(parsed.unwrap().type_name(), suf);
        }
    }
}