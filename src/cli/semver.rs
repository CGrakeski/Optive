//! 依赖版本约束：精确号、`^` / `~`、比较运算符。
//!
//! 规则对齐 Cargo 的常见子集（不含 `*` 与逗号复合区间）：
//! - `1.2.3` / `v1.2.3`：精确匹配该三元组
//! - `^1.2.3`：`>=1.2.3 <2.0.0`；`^0.2.3` → `<0.3.0`；`^0.0.3` → `<0.0.4`
//! - `~1.2.3`：`>=1.2.3 <1.3.0`；`~1.2` → `>=1.2.0 <1.3.0`；`~1` → `>=1.0.0 <2.0.0`
//! - `>=` `>` `<=` `<` `=`：与给定版本比较

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Ge,
    Gt,
    Le,
    Lt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionReq {
    Exact(Version),
    Caret(Version),
    /// `components`：用户写了几段（`~1` → 1，`~1.2` → 2，`~1.2.3` → 3）。
    Tilde {
        version: Version,
        components: u8,
    },
    Cmp {
        op: CmpOp,
        version: Version,
    },
}

impl VersionReq {
    #[must_use]
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Self::Exact(want) => v == want,
            Self::Caret(base) => caret_matches(*base, *v),
            Self::Tilde {
                version: base,
                components,
            } => tilde_matches(*base, *components, *v),
            Self::Cmp { op, version } => match op {
                CmpOp::Ge => v >= version,
                CmpOp::Gt => v > version,
                CmpOp::Le => v <= version,
                CmpOp::Lt => v < version,
            },
        }
    }
}

fn caret_matches(base: Version, v: Version) -> bool {
    if v < base {
        return false;
    }
    if base.major > 0 {
        v.major == base.major
    } else if base.minor > 0 {
        v.major == 0 && v.minor == base.minor
    } else {
        v.major == 0 && v.minor == 0 && v.patch == base.patch
    }
}

fn tilde_matches(base: Version, components: u8, v: Version) -> bool {
    if v < base {
        return false;
    }
    if components <= 1 {
        v.major == base.major
    } else {
        v.major == base.major && v.minor == base.minor
    }
}

/// 解析 tag 或约束里的版本号；允许前导 `v`/`V`。缺省的 minor/patch 视为 0。
pub fn parse_version(s: &str) -> Result<Version, String> {
    let t = strip_v(s.trim());
    if t.is_empty() {
        return Err("empty version".into());
    }
    let mut parts = t.split('.');
    let major = parse_num(parts.next().unwrap_or(""), "major")?;
    let minor = match parts.next() {
        Some(p) => parse_num(p, "minor")?,
        None => 0,
    };
    let patch = match parts.next() {
        Some(p) => parse_num(p, "patch")?,
        None => 0,
    };
    if parts.next().is_some() {
        return Err(format!("too many version components in `{s}`"));
    }
    Ok(Version {
        major,
        minor,
        patch,
    })
}

/// 从 git tag 名解析版本；无法解析则 `None`（忽略非 semver tag）。
#[must_use]
pub fn parse_version_from_tag(tag: &str) -> Option<Version> {
    let t = tag.trim();
    if t.is_empty() {
        return None;
    }
    parse_version(t).ok()
}

pub fn parse_req(s: &str) -> Result<VersionReq, String> {
    let raw = s.trim();
    if raw.is_empty() {
        return Err("empty version constraint".into());
    }
    if let Some(rest) = raw.strip_prefix('^') {
        return Ok(VersionReq::Caret(parse_version(rest)?));
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let (version, components) = parse_version_components(rest)?;
        return Ok(VersionReq::Tilde {
            version,
            components,
        });
    }
    if let Some(rest) = raw.strip_prefix(">=") {
        return Ok(VersionReq::Cmp {
            op: CmpOp::Ge,
            version: parse_version(rest)?,
        });
    }
    if let Some(rest) = raw.strip_prefix("<=") {
        return Ok(VersionReq::Cmp {
            op: CmpOp::Le,
            version: parse_version(rest)?,
        });
    }
    if let Some(rest) = raw.strip_prefix('>') {
        return Ok(VersionReq::Cmp {
            op: CmpOp::Gt,
            version: parse_version(rest)?,
        });
    }
    if let Some(rest) = raw.strip_prefix('<') {
        return Ok(VersionReq::Cmp {
            op: CmpOp::Lt,
            version: parse_version(rest)?,
        });
    }
    if let Some(rest) = raw.strip_prefix('=') {
        return Ok(VersionReq::Exact(parse_version(rest)?));
    }
    Ok(VersionReq::Exact(parse_version(raw)?))
}

fn parse_version_components(s: &str) -> Result<(Version, u8), String> {
    let t = strip_v(s.trim());
    let n = t.split('.').filter(|p| !p.is_empty()).count();
    if n == 0 {
        return Err("empty version".into());
    }
    let components = u8::try_from(n).unwrap_or(3);
    Ok((parse_version(s)?, components.min(3)))
}

fn strip_v(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

fn parse_num(s: &str, what: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("missing {what}"));
    }
    if s.chars().any(|c| !c.is_ascii_digit()) {
        return Err(format!("invalid {what} `{s}`"));
    }
    u64::from_str(s).map_err(|e| format!("invalid {what}: {e}"))
}

impl FromStr for VersionReq {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_req(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(maj: u64, min: u64, pat: u64) -> Version {
        Version {
            major: maj,
            minor: min,
            patch: pat,
        }
    }

    #[test]
    fn exact_and_v_prefix() {
        let r = parse_req("1.2.3").unwrap();
        assert!(r.matches(&v(1, 2, 3)));
        assert!(!r.matches(&v(1, 2, 4)));
        let r = parse_req("v0.1.2").unwrap();
        assert!(r.matches(&v(0, 1, 2)));
    }

    #[test]
    fn caret_stable() {
        let r = parse_req("^1.2.3").unwrap();
        assert!(r.matches(&v(1, 2, 3)));
        assert!(r.matches(&v(1, 9, 0)));
        assert!(!r.matches(&v(1, 2, 2)));
        assert!(!r.matches(&v(2, 0, 0)));
    }

    #[test]
    fn caret_zero_minor() {
        let r = parse_req("^0.2.3").unwrap();
        assert!(r.matches(&v(0, 2, 3)));
        assert!(r.matches(&v(0, 2, 9)));
        assert!(!r.matches(&v(0, 3, 0)));
        assert!(!r.matches(&v(1, 0, 0)));
    }

    #[test]
    fn caret_zero_patch() {
        let r = parse_req("^0.0.3").unwrap();
        assert!(r.matches(&v(0, 0, 3)));
        assert!(!r.matches(&v(0, 0, 4)));
    }

    #[test]
    fn tilde_patch() {
        let r = parse_req("~1.2.3").unwrap();
        assert!(r.matches(&v(1, 2, 3)));
        assert!(r.matches(&v(1, 2, 9)));
        assert!(!r.matches(&v(1, 3, 0)));
        let r = parse_req("~1").unwrap();
        assert!(r.matches(&v(1, 0, 0)));
        assert!(r.matches(&v(1, 9, 0)));
        assert!(!r.matches(&v(2, 0, 0)));
    }

    #[test]
    fn cmp_ops() {
        let r = parse_req(">=1.2.0").unwrap();
        assert!(r.matches(&v(1, 2, 0)));
        assert!(r.matches(&v(2, 0, 0)));
        assert!(!r.matches(&v(1, 1, 9)));
        let r = parse_req("<2.0.0").unwrap();
        assert!(r.matches(&v(1, 9, 9)));
        assert!(!r.matches(&v(2, 0, 0)));
        let r = parse_req(">0.1.0").unwrap();
        assert!(r.matches(&v(0, 1, 1)));
        assert!(!r.matches(&v(0, 1, 0)));
    }

    #[test]
    fn tag_parse() {
        assert_eq!(parse_version_from_tag("v1.2.3"), Some(v(1, 2, 3)));
        assert_eq!(parse_version_from_tag("0.1.2"), Some(v(0, 1, 2)));
        assert!(parse_version_from_tag("nightly").is_none());
    }
}
