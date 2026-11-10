//! Content-addressed blob store on the filesystem (BLAKE3, lowercase hex).
//!
//! Layout: `<root>/<aa>/<full-hash>` with `<root>/tmp/` for in-flight writes and
//! `<root>/partial/<hash>.part` for resumable downloads (used by sync).

use almond_types::store::BlobStore;
use almond_types::{BlobHash, Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const CHUNK: usize = 64 * 1024;

pub struct FsBlobStore {
    root: PathBuf,
}

/// Reject anything that isn't a 64-char lowercase hex BLAKE3 digest (hashes come from peers, so
/// this also prevents path traversal).
pub fn validate_hash(h: &BlobHash) -> Result<()> {
    let ok = h.0.len() == 64
        && h.0
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("bad blob hash {:?}", h.0)))
    }
}

impl FsBlobStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        fs::create_dir_all(root.join("partial"))?;
        Ok(FsBlobStore { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file_path(&self, h: &BlobHash) -> PathBuf {
        self.root.join(&h.0[..2]).join(&h.0)
    }

    fn persist(&self, tmp: tempfile::NamedTempFile, h: &BlobHash) -> Result<()> {
        let dst = self.file_path(h);
        if dst.exists() {
            return Ok(()); // same content already stored
        }
        fs::create_dir_all(dst.parent().expect("blob path has parent"))?;
        tmp.persist(&dst).map_err(|e| Error::Io(e.error))?;
        Ok(())
    }

    /// Stream a reader into the store.
    pub fn put_reader(&self, mut r: impl Read) -> Result<BlobHash> {
        let mut tmp = tempfile::NamedTempFile::new_in(self.root.join("tmp"))?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            tmp.write_all(&buf[..n])?;
        }
        tmp.as_file().sync_all()?;
        let h = BlobHash(hasher.finalize().to_hex().to_string());
        self.persist(tmp, &h)?;
        Ok(h)
    }

    pub fn size(&self, h: &BlobHash) -> Result<Option<u64>> {
        validate_hash(h)?;
        match fs::metadata(self.file_path(h)) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Read up to `len` bytes starting at `offset` (sync serves blobs in chunks).
    pub fn read_range(&self, h: &BlobHash, offset: u64, len: usize) -> Result<Option<Vec<u8>>> {
        validate_hash(h)?;
        let mut f = match File::open(self.file_path(h)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::with_capacity(len.min(16 << 20));
        f.take(len as u64).read_to_end(&mut buf)?;
        Ok(Some(buf))
    }

    pub fn delete(&self, h: &BlobHash) -> Result<()> {
        validate_hash(h)?;
        match fs::remove_file(self.file_path(h)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    // ---- resumable download support ----

    fn partial_path(&self, h: &BlobHash) -> PathBuf {
        self.root.join("partial").join(format!("{}.part", h.0))
    }

    /// Bytes already downloaded for `h` (resume offset).
    pub fn partial_len(&self, h: &BlobHash) -> Result<u64> {
        validate_hash(h)?;
        Ok(fs::metadata(self.partial_path(h))
            .map(|m| m.len())
            .unwrap_or(0))
    }

    /// Append a chunk at `offset` (must equal the current partial length).
    pub fn write_partial(&self, h: &BlobHash, offset: u64, data: &[u8]) -> Result<()> {
        validate_hash(h)?;
        let p = self.partial_path(h);
        let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
        let cur = f.metadata()?.len();
        if cur != offset {
            return Err(Error::Invalid(format!(
                "partial {} is at {cur}, chunk offset {offset}",
                h.0
            )));
        }
        f.write_all(data)?;
        Ok(())
    }

    /// Verify the partial file hashes to `h` and move it into the store. On mismatch the partial
    /// is discarded and an error returned.
    pub fn finish_partial(&self, h: &BlobHash) -> Result<()> {
        validate_hash(h)?;
        let p = self.partial_path(h);
        let mut hasher = blake3::Hasher::new();
        hasher.update_reader(File::open(&p)?)?;
        let got = hasher.finalize().to_hex().to_string();
        if got != h.0 {
            let _ = fs::remove_file(&p);
            return Err(Error::Invalid(format!(
                "blob hash mismatch: want {}, got {got}",
                h.0
            )));
        }
        let dst = self.file_path(h);
        fs::create_dir_all(dst.parent().expect("parent"))?;
        if dst.exists() {
            fs::remove_file(&p)?;
        } else {
            fs::rename(&p, &dst)?;
        }
        Ok(())
    }

    pub fn discard_partial(&self, h: &BlobHash) -> Result<()> {
        validate_hash(h)?;
        let _ = fs::remove_file(self.partial_path(h));
        Ok(())
    }
}

impl BlobStore for FsBlobStore {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        let h = BlobHash(blake3::hash(bytes).to_hex().to_string());
        if self.file_path(&h).exists() {
            return Ok(h);
        }
        let mut tmp = tempfile::NamedTempFile::new_in(self.root.join("tmp"))?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        self.persist(tmp, &h)?;
        Ok(h)
    }

    fn put_file(&self, path: &Path) -> Result<BlobHash> {
        self.put_reader(File::open(path)?)
    }

    fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
        validate_hash(hash)?;
        match fs::read(self.file_path(hash)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn has(&self, hash: &BlobHash) -> Result<bool> {
        validate_hash(hash)?;
        Ok(self.file_path(hash).is_file())
    }

    fn path(&self, hash: &BlobHash) -> Result<Option<PathBuf>> {
        validate_hash(hash)?;
        let p = self.file_path(hash);
        Ok(p.is_file().then_some(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_file_and_partials() {
        let dir = tempfile::tempdir().unwrap();
        let bs = FsBlobStore::open(dir.path().join("blobs")).unwrap();
        let h = bs.put(b"hello").unwrap();
        assert_eq!(h.0, blake3::hash(b"hello").to_hex().to_string());
        assert_eq!(bs.get(&h).unwrap().unwrap(), b"hello");
        assert!(bs.has(&h).unwrap());
        assert_eq!(bs.put(b"hello").unwrap(), h, "idempotent");

        // streaming put_file of a multi-chunk file
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let src = dir.path().join("rec.opus");
        fs::write(&src, &big).unwrap();
        let hb = bs.put_file(&src).unwrap();
        assert_eq!(hb.0, blake3::hash(&big).to_hex().to_string());
        assert_eq!(
            bs.path(&hb).unwrap().unwrap().metadata().unwrap().len(),
            big.len() as u64
        );
        assert_eq!(
            bs.read_range(&hb, 1000, 10).unwrap().unwrap(),
            &big[1000..1010]
        );

        // resumable download into a second store
        let bs2 = FsBlobStore::open(dir.path().join("blobs2")).unwrap();
        bs2.write_partial(&hb, 0, &big[..100_000]).unwrap();
        assert_eq!(bs2.partial_len(&hb).unwrap(), 100_000);
        assert!(
            bs2.write_partial(&hb, 5, b"x").is_err(),
            "offset must match"
        );
        bs2.write_partial(&hb, 100_000, &big[100_000..]).unwrap();
        bs2.finish_partial(&hb).unwrap();
        assert_eq!(bs2.get(&hb).unwrap().unwrap(), big);

        // corrupt partial is rejected
        bs2.write_partial(&h, 0, b"hellx").unwrap();
        assert!(bs2.finish_partial(&h).is_err());
        assert!(!bs2.has(&h).unwrap());
    }

    #[test]
    fn rejects_bad_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let bs = FsBlobStore::open(dir.path()).unwrap();
        assert!(bs.get(&BlobHash("../../etc/passwd".into())).is_err());
        assert!(bs.has(&BlobHash("AB".repeat(32))).is_err());
    }
}
