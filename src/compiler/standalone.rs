//! Self-contained executable footer used by `Optive build --exe`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"OPTIVEXE";
const VERSION: u32 = 1;
const FOOTER_LEN: usize = 8 + 4 + 8 + 8 + 32;

#[must_use]
pub fn attach(runner: &[u8], bundle: &[u8]) -> Vec<u8> {
    let offset = runner.len() as u64;
    let length = bundle.len() as u64;
    let mut output = Vec::with_capacity(runner.len() + bundle.len() + FOOTER_LEN);
    output.extend_from_slice(runner);
    output.extend_from_slice(bundle);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&VERSION.to_le_bytes());
    output.extend_from_slice(&offset.to_le_bytes());
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&Sha256::digest(bundle));
    output
}

pub fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = File::open(path).map_err(|error| format!("cannot read executable: {error}"))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("cannot inspect executable: {error}"))?
        .len();
    if file_len < FOOTER_LEN as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
        .map_err(|error| format!("cannot seek executable footer: {error}"))?;
    let mut footer = [0_u8; FOOTER_LEN];
    file.read_exact(&mut footer)
        .map_err(|error| format!("cannot read executable footer: {error}"))?;
    if &footer[..8] != MAGIC {
        return Ok(None);
    }
    let version = u32::from_le_bytes(footer[8..12].try_into().expect("footer version"));
    if version != VERSION {
        return Err(format!(
            "unsupported Optive executable version {version} (expected {VERSION})"
        ));
    }
    let offset = u64::from_le_bytes(footer[12..20].try_into().expect("footer offset"));
    let length = u64::from_le_bytes(footer[20..28].try_into().expect("footer length"));
    let offset = usize::try_from(offset).map_err(|_| "invalid embedded bundle offset")?;
    let length = usize::try_from(length).map_err(|_| "invalid embedded bundle length")?;
    let end = offset
        .checked_add(length)
        .ok_or("invalid embedded bundle range")?;
    let payload_end = usize::try_from(file_len - FOOTER_LEN as u64)
        .map_err(|_| "executable is too large for this host")?;
    if end != payload_end {
        return Err("invalid embedded bundle range".into());
    }
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|error| format!("cannot seek embedded bundle: {error}"))?;
    let mut bundle = vec![0_u8; length];
    file.read_exact(&mut bundle)
        .map_err(|error| format!("cannot read embedded bundle: {error}"))?;
    if footer[28..] != *Sha256::digest(&bundle) {
        return Err("embedded bundle checksum mismatch".into());
    }
    Ok(Some(bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn footer_roundtrip_and_checksum() {
        let bytes = attach(b"runner", b"bundle");
        let path =
            std::env::temp_dir().join(format!("optive-standalone-{}.bin", std::process::id()));
        fs::write(&path, &bytes).unwrap();
        assert_eq!(read(&path).unwrap().unwrap(), b"bundle");
        let mut damaged = bytes;
        damaged[7] ^= 1;
        fs::write(&path, damaged).unwrap();
        assert!(read(&path).unwrap_err().contains("checksum"));
        let _ = fs::remove_file(path);
    }
}
