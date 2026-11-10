//! Content-addressed storage for submission code and task text, so agents
//! and buyers without their own hosting can use Sphinx. The file name is the
//! SHA-256 of the bytes, so a stored file can never be swapped for different
//! text under the same URI.
//!
//! Uploads are bounded: a rate per client and in total (see
//! [`crate::limits::ClientLimiter`]), a cap on the store's total size, and
//! [`CodeStore::collect`], which removes uploads that nothing on chain refers
//! to once they are a day old.

use sphinx_protocol::{Code, Hash32, SphinxError};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use crate::limits::ClientLimiter;

/// Largest submission accepted, matching the grader's own limit.
pub const MAX_CODE_BYTES: usize = 64 * 1024;
/// Default cap on the store's total size.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug)]
pub struct CodeStore {
    dir: PathBuf,
    max_total: u64,
    /// Bytes stored, counted from the folder on first use.
    used: Mutex<Option<u64>>,
    uploads: ClientLimiter,
    /// Lets the relayer read submissions that are not yet public.
    reader_token: Option<String>,
}

impl CodeStore {
    /// A store in `dir` with the default limits and no reader token.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            max_total: DEFAULT_MAX_TOTAL_BYTES,
            used: Mutex::new(None),
            uploads: ClientLimiter::new(10, 30, 600, Instant::now()),
            reader_token: None,
        }
    }

    #[must_use]
    pub fn with_max_total(mut self, bytes: u64) -> Self {
        self.max_total = bytes;
        self
    }

    #[must_use]
    pub fn with_reader_token(mut self, token: Option<String>) -> Self {
        self.reader_token = token.filter(|t| !t.is_empty());
        self
    }

    /// Whether `presented` is the reader token. Both are hashed first, so the
    /// comparison takes the same time whatever the presented value's length.
    pub fn is_reader(&self, presented: Option<&str>) -> bool {
        let (Some(token), Some(given)) = (&self.reader_token, presented) else { return false };
        let (a, b) = (Hash32::digest(token.as_bytes()), Hash32::digest(given.as_bytes()));
        a.as_bytes().iter().zip(b.as_bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    /// Stores `code` from `client` and returns its hash.
    ///
    /// # Errors
    /// `SPHINX_E_API_RATE_LIMITED` when the client or the store is over its
    /// limits; `SPHINX_E_GRADER_CODE_TOO_LARGE` for oversized code;
    /// `SPHINX_E_CODE_SYNTAX` for non-UTF-8 bytes;
    /// `SPHINX_E_INTERNAL_INVARIANT` if the file cannot be written.
    pub async fn put(&self, code: &[u8], client: &str) -> Result<Hash32, SphinxError> {
        self.uploads.admit(client, Instant::now())?;
        if code.len() > MAX_CODE_BYTES {
            return Err(SphinxError::new(
                Code::GraderCodeTooLarge,
                format!("submission is {} bytes; the maximum is {MAX_CODE_BYTES}", code.len()),
            )
            .ctx("bytes", code.len())
            .ctx("max", MAX_CODE_BYTES));
        }
        if std::str::from_utf8(code).is_err() {
            return Err(SphinxError::new(Code::CodeSyntax, "submission is not valid UTF-8 text"));
        }
        let hash = Hash32::digest(code);
        let path = self.path(&hash);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(hash);
        }
        tokio::fs::create_dir_all(&self.dir).await.map_err(io_err)?;
        let used = self.used_bytes().await?;
        let len = code.len() as u64;
        if used.saturating_add(len) > self.max_total {
            return Err(SphinxError::new(
                Code::ApiRateLimited,
                "the code store is full; uploads nothing refers to are removed after a day",
            )
            .ctx("retry_after_secs", 3600));
        }
        tokio::fs::write(&path, code).await.map_err(io_err)?;
        self.add_used(len, true);
        Ok(hash)
    }

    /// Reads stored code, or `None` if no code has that hash.
    ///
    /// # Errors
    /// `SPHINX_E_INTERNAL_INVARIANT` on read failures other than not-found.
    pub async fn get(&self, hash: &Hash32) -> Result<Option<Vec<u8>>, SphinxError> {
        match tokio::fs::read(self.path(hash)).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(e)),
        }
    }

    /// Removes stored files older than `min_age` whose hash is not in
    /// `referenced`. Returns how many were removed.
    ///
    /// # Errors
    /// `SPHINX_E_INTERNAL_INVARIANT` if the folder cannot be read.
    pub async fn collect(
        &self,
        referenced: &HashSet<Hash32>,
        min_age: Duration,
    ) -> Result<usize, SphinxError> {
        let mut dir = match tokio::fs::read_dir(&self.dir).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io_err(e)),
        };
        let mut removed = 0;
        while let Some(entry) = dir.next_entry().await.map_err(io_err)? {
            let name = entry.file_name();
            let Some(hash) =
                name.to_str().and_then(|n| n.strip_suffix(".js")).and_then(|h| h.parse().ok())
            else {
                continue;
            };
            if referenced.contains(&hash) {
                continue;
            }
            let Ok(meta) = entry.metadata().await else { continue };
            let old = meta
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age >= min_age);
            if old && tokio::fs::remove_file(entry.path()).await.is_ok() {
                self.add_used(meta.len(), false);
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn used_bytes(&self) -> Result<u64, SphinxError> {
        if let Some(n) = *self.used.lock().unwrap_or_else(PoisonError::into_inner) {
            return Ok(n);
        }
        let mut total = 0u64;
        let mut dir = tokio::fs::read_dir(&self.dir).await.map_err(io_err)?;
        while let Some(entry) = dir.next_entry().await.map_err(io_err)? {
            total = total.saturating_add(entry.metadata().await.map_or(0, |m| m.len()));
        }
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(*used.get_or_insert(total))
    }

    fn add_used(&self, bytes: u64, grow: bool) {
        if let Some(n) = self.used.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            *n = if grow { n.saturating_add(bytes) } else { n.saturating_sub(bytes) };
        }
    }

    fn path(&self, hash: &Hash32) -> PathBuf {
        self.dir.join(format!("{hash}.js"))
    }
}

#[allow(clippy::needless_pass_by_value, reason = "used as a map_err callback")]
fn io_err(e: std::io::Error) -> SphinxError {
    SphinxError::new(Code::InternalInvariant, format!("code store I/O failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> CodeStore {
        let dir = std::env::temp_dir().join(format!("sphinx-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        CodeStore::new(dir)
    }

    #[tokio::test]
    async fn the_store_stays_within_its_quota() {
        let s = store("quota").with_max_total(10);
        s.put(b"12345", "a").await.unwrap();
        s.put(b"12345", "a").await.unwrap();
        let e = s.put(b"abcdefgh", "a").await.unwrap_err();
        assert_eq!(e.code(), Code::ApiRateLimited);
    }

    #[tokio::test]
    async fn unreferenced_uploads_are_collected_and_referenced_ones_kept() {
        let s = store("collect");
        let keep = s.put(b"kept", "a").await.unwrap();
        let drop = s.put(b"dropped", "a").await.unwrap();
        let referenced = HashSet::from([keep]);
        assert_eq!(s.collect(&referenced, Duration::from_secs(3600)).await.unwrap(), 0, "too new");
        assert_eq!(s.collect(&referenced, Duration::ZERO).await.unwrap(), 1);
        assert!(s.get(&keep).await.unwrap().is_some());
        assert!(s.get(&drop).await.unwrap().is_none());
    }

    #[test]
    fn only_the_exact_token_reads_private_code() {
        let s = store("token").with_reader_token(Some("s3cret-token".into()));
        assert!(s.is_reader(Some("s3cret-token")));
        assert!(!s.is_reader(Some("s3cret-tokeN")));
        assert!(!s.is_reader(Some("s3cret")));
        assert!(!s.is_reader(None));
        assert!(!store("none").with_reader_token(Some(String::new())).is_reader(Some("")));
    }
}
