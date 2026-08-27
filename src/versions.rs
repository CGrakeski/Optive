//! 语言、字节码与 embedding API 版本契约。
//!
//! 解释器 Cargo 包版本可以独立演进；缓存键与兼容检查使用这里的常量，
//! 而不是 `CARGO_PKG_VERSION`。

/// 当前语言语义版本（主.次）。
pub const LANGUAGE_VERSION: &str = "0.2";
/// `.tivc` 磁盘格式号。布局变了就丢缓存；0.x 不读旧格式。
pub const BYTECODE_FORMAT_VERSION: u16 = 2;
/// 稳定 embedding facade 版本。
pub const EMBED_API_VERSION: u16 = 1;

#[must_use]
pub fn bytecode_cache_version() -> String {
    format!("lang={LANGUAGE_VERSION};bc={BYTECODE_FORMAT_VERSION}")
}

/// `requires_optive` 约束：`1.2.3` 或 `>=1.2.3`。
pub fn satisfies_requires_optive(req: &str, current: &str) -> Result<bool, String> {
    let req = req.trim();
    let current = parse_triple(current)?;
    if let Some(min) = req.strip_prefix(">=") {
        return Ok(current >= parse_triple(min)?);
    }
    Ok(current == parse_triple(req)?)
}

fn parse_triple(raw: &str) -> Result<(u64, u64, u64), String> {
    let raw = raw.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parse_part(parts.next(), raw)?;
    let minor = parse_part(parts.next(), raw)?;
    let patch = parse_part(parts.next(), raw)?;
    if parts.next().is_some() {
        return Err(format!("too many version components in `{raw}`"));
    }
    Ok((major, minor, patch))
}

fn parse_part(part: Option<&str>, raw: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("invalid version `{raw}`"))?;
    part.parse().map_err(|_| format!("invalid version `{raw}`"))
}

pub fn language_compatible(declared: &str) -> bool {
    let declared = declared.trim();
    declared == LANGUAGE_VERSION || declared.starts_with(&format!("{LANGUAGE_VERSION}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_optive_exact_and_ge() {
        assert!(satisfies_requires_optive("0.2.0", "0.2.0").unwrap());
        assert!(!satisfies_requires_optive("0.3.0", "0.2.0").unwrap());
        assert!(satisfies_requires_optive(">=0.2.0", "0.2.1").unwrap());
        assert!(!satisfies_requires_optive(">=0.3.0", "0.2.0").unwrap());
    }

    #[test]
    fn language_minor_matches() {
        assert!(language_compatible("0.2"));
        assert!(language_compatible("0.2.0"));
        assert!(!language_compatible("0.1"));
    }
}
