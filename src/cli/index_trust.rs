//! 包索引轻量信任链：可选 pin 与 signed-commit 策略。
//!
//! 默认 `off`，本地 `index.json` 与未签名 checkout 仍可开发。
//! 官方默认远程可用 `OPTIVE_INDEX_POLICY=strict` 要求 HEAD 带签名头；
//! `OPTIVE_INDEX_PIN` 一旦设置即强制 HEAD 等于该完整 object id。

use std::error::Error;
use std::path::Path;

use super::home;
use super::store::is_full_object_id;

/// 索引信任策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPolicy {
    /// 不要求签名；仅在设置了 pin 时校验 HEAD。
    Off,
    /// 索引必须是 git checkout，且 HEAD commit 带 `gpgsig` / SSH 签名头。
    Signed,
    /// 官方默认远程必须签名；自定义远程必须签名或 pin。
    Strict,
}

impl IndexPolicy {
    pub fn from_env() -> Result<Self, Box<dyn Error>> {
        match std::env::var("OPTIVE_INDEX_POLICY") {
            Ok(raw) => parse_policy(&raw),
            Err(_) => Ok(Self::Off),
        }
    }
}

fn parse_policy(raw: &str) -> Result<IndexPolicy, Box<dyn Error>> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "0" | "false" | "permissive" => Ok(IndexPolicy::Off),
        "signed" | "sign" => Ok(IndexPolicy::Signed),
        "strict" | "official" => Ok(IndexPolicy::Strict),
        other => Err(format!(
            "invalid OPTIVE_INDEX_POLICY `{other}` (expected off, signed, or strict)"
        )
        .into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexHead {
    pub commit: String,
    pub signed: bool,
}

pub fn pin_from_env() -> Result<Option<String>, Box<dyn Error>> {
    match std::env::var("OPTIVE_INDEX_PIN") {
        Ok(raw) => {
            let pin = raw.trim();
            if pin.is_empty() {
                return Ok(None);
            }
            if !is_full_object_id(pin) {
                return Err(format!(
                    "OPTIVE_INDEX_PIN must be a full 40- or 64-hex Git object id, got `{pin}`"
                )
                .into());
            }
            Ok(Some(pin.to_ascii_lowercase()))
        }
        Err(_) => Ok(None),
    }
}

/// 在 `index sync` 之后以及读取 git checkout 索引前调用。
pub fn verify_index_dir(path: &Path) -> Result<Option<IndexHead>, Box<dyn Error>> {
    let policy = IndexPolicy::from_env()?;
    let pin = pin_from_env()?;
    verify_index_dir_with(
        path,
        policy,
        pin.as_deref(),
        using_official_default_remote(),
        "official default index",
    )
}

fn using_official_default_remote() -> bool {
    match std::env::var("OPTIVE_INDEX_URL") {
        Ok(url) if !url.trim().is_empty() => return false,
        _ => {}
    }
    !home::optive_home().join("index.url").is_file()
}

fn verify_index_dir_with(
    path: &Path,
    policy: IndexPolicy,
    pin: Option<&str>,
    official_default: bool,
    url: &str,
) -> Result<Option<IndexHead>, Box<dyn Error>> {
    let head = inspect_index_head(path)?;
    let require_git = pin.is_some()
        || matches!(policy, IndexPolicy::Signed)
        || (policy == IndexPolicy::Strict && (official_default || pin.is_none()));
    if head.is_none() {
        if require_git {
            return Err(format!(
                "index at {} is not a git checkout; cannot apply OPTIVE_INDEX_POLICY/{}/pin. \
                 Use `Optive index sync` or set OPTIVE_INDEX_POLICY=off for a plain index.json",
                path.display(),
                if official_default {
                    url
                } else {
                    "custom remote"
                }
            )
            .into());
        }
        return Ok(None);
    }
    let head = head.expect("git HEAD inspected");
    check_index_head(&head, policy, pin, official_default)?;
    Ok(Some(head))
}

fn check_index_head(
    head: &IndexHead,
    policy: IndexPolicy,
    pin: Option<&str>,
    official_default: bool,
) -> Result<(), String> {
    if let Some(pin) = pin {
        if head.commit != pin {
            return Err(format!(
                "index HEAD {} does not match OPTIVE_INDEX_PIN {pin}",
                head.commit
            ));
        }
    }
    let require_signed = match policy {
        IndexPolicy::Off => false,
        IndexPolicy::Signed => true,
        IndexPolicy::Strict => official_default || pin.is_none(),
    };
    if require_signed && !head.signed {
        return Err(format!(
            "index HEAD {} is unsigned; official/strict policy requires a gpgsig or SSH signature header on the commit",
            head.commit
        ));
    }
    Ok(())
}

fn inspect_index_head(path: &Path) -> Result<Option<IndexHead>, Box<dyn Error>> {
    if gix::open(path).is_err() && !path.join(".git").exists() {
        return Ok(None);
    }
    let repo = gix::open(path).map_err(|e| format!("open index {}: {e}", path.display()))?;
    let id = repo
        .head_id()
        .map_err(|e| format!("read index HEAD {}: {e}", path.display()))?
        .detach();
    let object = repo
        .find_object(id)
        .map_err(|e| format!("load index HEAD {id}: {e}"))?;
    let commit = object
        .try_into_commit()
        .map_err(|_| format!("index HEAD {id} is not a commit"))?;
    Ok(Some(IndexHead {
        commit: id.to_string().to_ascii_lowercase(),
        signed: commit_has_signature(&commit.data),
    }))
}

fn commit_has_signature(data: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(data) else {
        return false;
    };
    let headers = text.split("\n\n").next().unwrap_or(text);
    headers.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("gpgsig ")
            || line.starts_with("gpgsig-sha256")
            || line.eq_ignore_ascii_case("gpgsig")
            || line.starts_with("-----BEGIN PGP SIGNATURE-----")
            || line.starts_with("-----BEGIN SSH SIGNATURE-----")
    })
}

/// 供 `Optive env` / doctor 打印。
pub fn describe_policy() -> String {
    let policy = IndexPolicy::from_env()
        .map(|p| format!("{p:?}").to_ascii_lowercase())
        .unwrap_or_else(|e| format!("invalid ({e})"));
    let pin = std::env::var("OPTIVE_INDEX_PIN").unwrap_or_default();
    if pin.trim().is_empty() {
        format!("policy={policy}, pin=(unset)")
    } else {
        format!("policy={policy}, pin={}", pin.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_policy_aliases() {
        assert_eq!(parse_policy("off").unwrap(), IndexPolicy::Off);
        assert_eq!(parse_policy("SIGNED").unwrap(), IndexPolicy::Signed);
        assert_eq!(parse_policy("official").unwrap(), IndexPolicy::Strict);
        assert!(parse_policy("maybe").is_err());
    }

    #[test]
    fn unsigned_payload_is_detected() {
        let unsigned = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author a <a@b> 1 +0000\n\
committer a <a@b> 1 +0000\n\n\
msg\n";
        assert!(!commit_has_signature(unsigned));
        let signed = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author a <a@b> 1 +0000\n\
committer a <a@b> 1 +0000\n\
gpgsig -----BEGIN PGP SIGNATURE-----\n \
 fake\n \
 -----END PGP SIGNATURE-----\n\n\
msg\n";
        assert!(commit_has_signature(signed));
        let sha256 = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author a <a@b> 1 +0000\n\
committer a <a@b> 1 +0000\n\
gpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n \
 fake\n \
 -----END PGP SIGNATURE-----\n\n\
msg\n";
        assert!(commit_has_signature(sha256));
        let ssh = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
-----BEGIN SSH SIGNATURE-----\n\
fake\n\
-----END SSH SIGNATURE-----\n\n\
msg\n";
        assert!(commit_has_signature(ssh));
        let marker_in_body = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
author a <a@b> 1 +0000\n\
committer a <a@b> 1 +0000\n\n\
msg mentions -----BEGIN PGP SIGNATURE-----\n";
        assert!(!commit_has_signature(marker_in_body));
    }

    #[test]
    fn pin_mismatch_is_rejected() {
        let head = IndexHead {
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            signed: true,
        };
        let err = check_index_head(
            &head,
            IndexPolicy::Off,
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            false,
        )
        .unwrap_err();
        assert!(err.contains("OPTIVE_INDEX_PIN"), "{err}");
    }

    #[test]
    fn signed_policy_rejects_unsigned_head() {
        let head = IndexHead {
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            signed: false,
        };
        let err = check_index_head(&head, IndexPolicy::Signed, None, false).unwrap_err();
        assert!(err.contains("unsigned"), "{err}");
    }

    #[test]
    fn strict_official_requires_signature() {
        let head = IndexHead {
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            signed: false,
        };
        assert!(check_index_head(&head, IndexPolicy::Strict, None, true).is_err());
        let signed = IndexHead {
            signed: true,
            ..head.clone()
        };
        assert!(check_index_head(&signed, IndexPolicy::Strict, None, true).is_ok());
    }

    #[test]
    fn off_allows_unsigned() {
        let head = IndexHead {
            commit: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            signed: false,
        };
        assert!(check_index_head(&head, IndexPolicy::Off, None, true).is_ok());
    }
}
