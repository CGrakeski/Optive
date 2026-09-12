//! Binary `.tivb` container: identity index, interfaces, bytecode, checksum.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::versions::BUNDLE_FORMAT_VERSION;

pub const BUNDLE_MAGIC: &[u8; 4] = b"TIVB";
const MAX_BUNDLE_FILE: usize = 64 * 1024 * 1024;
const MAX_BUNDLE_ITEMS: u32 = 100_000;
const MAX_BUNDLE_BLOB: u32 = 16 * 1024 * 1024;
const MAX_BUNDLE_STRING: u32 = 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct BundleImage {
    pub compiler: String,
    pub entry_package: String,
    pub entry_module: String,
    pub bindings: Vec<BundleBinding>,
    pub package_entries: BTreeMap<String, String>,
    pub modules: BTreeMap<(String, String), BundleModule>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BundleBinding {
    pub parent: String,
    pub name: String,
    pub package_id: String,
}

#[derive(Clone, Debug)]
pub struct BundleModule {
    pub interface_digest: String,
    pub interface: Vec<u8>,
    pub bytecode: Vec<u8>,
    pub dependencies: Vec<String>,
}

#[must_use]
pub fn is_bundle_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tivb"))
}

impl BundleImage {
    pub fn get(&self, package_id: &str, module: &str) -> Option<&BundleModule> {
        self.modules
            .get(&(package_id.to_string(), module.to_string()))
    }

    pub fn binding(&self, parent: &str, name: &str) -> Option<&str> {
        self.bindings.iter().find_map(|binding| {
            (binding.parent == parent && binding.name == name)
                .then_some(binding.package_id.as_str())
        })
    }

    pub fn resolve_module(&self, package_id: &str, parts: &[&str]) -> Option<String> {
        if parts.is_empty() {
            return self.package_entries.get(package_id).cloned();
        }
        let joined = parts.join("/");
        [
            format!("{joined}.tive"),
            format!("{joined}/main.tive"),
            format!("src/{joined}.tive"),
            format!("src/{joined}/main.tive"),
        ]
        .into_iter()
        .find(|candidate| {
            self.modules
                .contains_key(&(package_id.to_string(), candidate.clone()))
        })
    }

    pub fn resolve_file(
        &self,
        package_id: &str,
        current_module: &str,
        path: &str,
    ) -> Option<String> {
        let parent = Path::new(current_module)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let joined = parent.join(path);
        let normalized = normalize_virtual(&joined)?;
        let mut candidates = vec![normalized.clone()];
        if !normalized.ends_with(".tive") {
            candidates.push(format!("{normalized}.tive"));
        }
        candidates.into_iter().find(|candidate| {
            self.modules
                .contains_key(&(package_id.to_string(), candidate.clone()))
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut payload = Vec::new();
        write_str(&mut payload, &self.compiler);
        write_str(&mut payload, &self.entry_package);
        write_str(&mut payload, &self.entry_module);
        write_u32(
            &mut payload,
            u32::try_from(self.bindings.len()).map_err(|_| "too many bindings")?,
        );
        let mut bindings = self.bindings.clone();
        bindings.sort();
        for binding in &bindings {
            write_str(&mut payload, &binding.parent);
            write_str(&mut payload, &binding.name);
            write_str(&mut payload, &binding.package_id);
        }
        write_u32(
            &mut payload,
            u32::try_from(self.package_entries.len()).map_err(|_| "too many package entries")?,
        );
        for (package_id, entry) in &self.package_entries {
            write_str(&mut payload, package_id);
            write_str(&mut payload, entry);
        }
        write_u32(
            &mut payload,
            u32::try_from(self.modules.len()).map_err(|_| "too many bundle modules")?,
        );
        for ((package_id, module), item) in &self.modules {
            write_str(&mut payload, package_id);
            write_str(&mut payload, module);
            write_str(&mut payload, &item.interface_digest);
            write_blob(&mut payload, &item.interface)?;
            write_blob(&mut payload, &item.bytecode)?;
            write_u32(
                &mut payload,
                u32::try_from(item.dependencies.len())
                    .map_err(|_| "too many module dependencies")?,
            );
            for dependency in &item.dependencies {
                write_str(&mut payload, dependency);
            }
        }

        let mut out = Vec::with_capacity(16 + payload.len() + 32);
        out.extend_from_slice(BUNDLE_MAGIC);
        out.extend_from_slice(&BUNDLE_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&payload);
        let digest = Sha256::digest(&out);
        out.extend_from_slice(&digest);
        if out.len() > MAX_BUNDLE_FILE {
            return Err("bundle exceeds maximum size".into());
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 16 + 32 {
            return Err("tivb is truncated".into());
        }
        if bytes.len() > MAX_BUNDLE_FILE {
            return Err("tivb exceeds maximum size".into());
        }
        if bytes[..4] != *BUNDLE_MAGIC {
            return Err("not an Optive tivb bundle (missing TIVB magic)".into());
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("version slice"));
        if version != BUNDLE_FORMAT_VERSION {
            return Err(format!(
                "unsupported tivb version {version} (expected {BUNDLE_FORMAT_VERSION})"
            ));
        }
        let (body, checksum) = bytes.split_at(bytes.len() - 32);
        let expected = Sha256::digest(body);
        if checksum != expected.as_slice() {
            return Err("tivb checksum mismatch (file is corrupt or truncated)".into());
        }
        let mut r = Cursor::new(&body[16..]);
        let compiler = read_str(&mut r)?;
        let entry_package = read_str(&mut r)?;
        let entry_module = read_str(&mut r)?;
        let binding_count = read_count(&mut r, MAX_BUNDLE_ITEMS)?;
        let mut bindings = Vec::with_capacity(binding_count);
        for _ in 0..binding_count {
            bindings.push(BundleBinding {
                parent: read_str(&mut r)?,
                name: read_str(&mut r)?,
                package_id: read_str(&mut r)?,
            });
        }
        let entry_count = read_count(&mut r, MAX_BUNDLE_ITEMS)?;
        let mut package_entries = BTreeMap::new();
        for _ in 0..entry_count {
            package_entries.insert(read_str(&mut r)?, read_str(&mut r)?);
        }
        let module_count = read_count(&mut r, MAX_BUNDLE_ITEMS)?;
        let mut modules = BTreeMap::new();
        for _ in 0..module_count {
            let package_id = read_str(&mut r)?;
            let module = read_str(&mut r)?;
            let interface_digest = read_str(&mut r)?;
            let interface = read_blob(&mut r)?;
            let bytecode = read_blob(&mut r)?;
            let dep_count = read_count(&mut r, MAX_BUNDLE_ITEMS)?;
            let mut dependencies = Vec::with_capacity(dep_count);
            for _ in 0..dep_count {
                dependencies.push(read_str(&mut r)?);
            }
            modules.insert(
                (package_id, module),
                BundleModule {
                    interface_digest,
                    interface,
                    bytecode,
                    dependencies,
                },
            );
        }
        if r.position() as usize != r.get_ref().len() {
            return Err("tivb has trailing payload bytes".into());
        }
        Ok(Self {
            compiler,
            entry_package,
            entry_module,
            bindings,
            package_entries,
            modules,
        })
    }
}

fn normalize_virtual(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_str(out: &mut Vec<u8>, value: &str) {
    write_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn write_blob(out: &mut Vec<u8>, value: &[u8]) -> Result<(), String> {
    if value.len() > MAX_BUNDLE_BLOB as usize {
        return Err("bundle blob exceeds maximum size".into());
    }
    write_u32(out, value.len() as u32);
    out.extend_from_slice(value);
    Ok(())
}

fn remaining(r: &Cursor<&[u8]>) -> u64 {
    (r.get_ref().len() as u64).saturating_sub(r.position())
}

fn read_u32(r: &mut Cursor<&[u8]>) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|_| "tivb is truncated".to_string())?;
    Ok(u32::from_le_bytes(buf))
}

fn read_count(r: &mut Cursor<&[u8]>, max: u32) -> Result<usize, String> {
    let n = read_u32(r)?;
    if n > max || u64::from(n) > remaining(r) {
        return Err("tivb count is invalid".into());
    }
    Ok(n as usize)
}

fn read_str(r: &mut Cursor<&[u8]>) -> Result<String, String> {
    let n = read_u32(r)?;
    if n > MAX_BUNDLE_STRING || u64::from(n) > remaining(r) {
        return Err("tivb string is invalid".into());
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)
        .map_err(|_| "tivb is truncated".to_string())?;
    String::from_utf8(buf).map_err(|_| "tivb string is not utf-8".to_string())
}

fn read_blob(r: &mut Cursor<&[u8]>) -> Result<Vec<u8>, String> {
    let n = read_u32(r)?;
    if n > MAX_BUNDLE_BLOB || u64::from(n) > remaining(r) {
        return Err("tivb blob is invalid".into());
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)
        .map_err(|_| "tivb is truncated".to_string())?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bc_cache;

    #[test]
    fn bundle_roundtrip_checksum_and_version() {
        let compiled = crate::compile("export const X = 1\nprint(X)\n").expect("compile");
        let bytecode = bc_cache::encode_program(&compiled).expect("encode bytecode");
        let mut image = BundleImage {
            compiler: "test".into(),
            entry_package: "__root__".into(),
            entry_module: "src/main.tive".into(),
            ..BundleImage::default()
        };
        image
            .package_entries
            .insert("__root__".into(), "src/main.tive".into());
        image.modules.insert(
            ("__root__".into(), "src/main.tive".into()),
            BundleModule {
                interface_digest: "abc".into(),
                interface: b"{}".to_vec(),
                bytecode,
                dependencies: Vec::new(),
            },
        );
        let encoded = image.encode().expect("encode bundle");
        assert_eq!(&encoded[..4], BUNDLE_MAGIC);
        let decoded = BundleImage::decode(&encoded).expect("decode bundle");
        assert_eq!(decoded.entry_module, "src/main.tive");
        assert!(decoded.get("__root__", "src/main.tive").is_some());

        let mut corrupt = encoded.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        let err = BundleImage::decode(&corrupt).expect_err("corrupt checksum");
        assert!(err.contains("checksum"), "{err}");

        let mut other_version = encoded;
        other_version[4..8].copy_from_slice(&99u32.to_le_bytes());
        let err = BundleImage::decode(&other_version).expect_err("bad version");
        assert!(err.contains("unsupported tivb version 99"), "{err}");
    }
}
