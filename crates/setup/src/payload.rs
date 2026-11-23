//! The embedded `Phase.exe`: compressed data and its checksums.
//!
//! There are two checks: the checksum of the compressed data — before anything is written to disk; the checksum of
//! the unpacked file — right after unpacking, while it lies in the temporary folder.

use std::{
    io::{self, Read, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};

use crate::error::{Result, SetupError};

const CHUNK: usize = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Payload<'a> {
    pub packed: &'a [u8],
    pub packed_sha256: &'a str,
    pub raw_sha256: &'a str,
    pub raw_size: u64,
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(SetupError::Cancelled);
    }
    Ok(())
}

impl Payload<'_> {
    /// Verifies the compressed data checksum. `progress` receives a fraction from 0 to 1.
    pub fn verify(&self, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<()> {
        if self.packed.is_empty() || self.raw_size == 0 {
            return Err(SetupError::CorruptPayload);
        }
        let mut hasher = Sha256::new();
        let mut done = 0usize;
        for chunk in self.packed.chunks(CHUNK * 4) {
            check_cancel(cancel)?;
            hasher.update(chunk);
            done += chunk.len();
            progress(done as f32 / self.packed.len() as f32);
        }
        if to_hex(&hasher.finalize()) != self.packed_sha256 {
            return Err(SetupError::CorruptPayload);
        }
        Ok(())
    }

    /// Unpacks into `out`, verifying size and checksum. Returns the file's checksum.
    pub fn extract(
        &self,
        out: &mut dyn Write,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(f32),
    ) -> Result<String> {
        let mut reader = brotli_decompressor::Decompressor::new(self.packed, 64 * 1024);
        let mut buffer = vec![0u8; CHUNK];
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        loop {
            check_cancel(cancel)?;
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                // The checksum already matched but the data can't be read: treat it as corrupted.
                Err(_) => return Err(SetupError::CorruptPayload),
            };
            written += count as u64;
            if written > self.raw_size {
                return Err(SetupError::CorruptPayload);
            }
            hasher.update(&buffer[..count]);
            out.write_all(&buffer[..count]).map_err(|err| {
                SetupError::from_io("extract", std::path::Path::new(crate::APP_EXE), &err)
            })?;
            progress(written as f32 / self.raw_size as f32);
        }
        let hash = to_hex(&hasher.finalize());
        if written != self.raw_size || hash != self.raw_sha256 {
            return Err(SetupError::CorruptPayload);
        }
        Ok(hash)
    }
}
