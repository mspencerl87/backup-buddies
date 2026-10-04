//! Encryption keys and streaming encrypt/decrypt.
//!
//! Files used to be encrypted straight to the passphrase (age's scrypt
//! recipient). That runs scrypt — deliberately slow, ~1.5s of CPU — on
//! *every* file, so a folder of 100,000 photos cost ~40 hours of CPU before
//! sending a byte, and restoring it cost the same again. Now the slow
//! passphrase step runs once at startup to derive a long-term X25519 key;
//! every file is encrypted to that key in microseconds. The passphrase alone
//! still recovers everything (the derivation is deterministic), and files
//! encrypted the old way still decrypt: decryption tries both identities.
//!
//! Everything here streams in bounded chunks, so memory use doesn't depend
//! on file size.

use std::io::{self, Read, Write};
use std::sync::OnceLock;

use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result};
use tokio::sync::mpsc;

/// age encrypts in 64 KiB chunks, each with a 16-byte authentication tag.
const AGE_CHUNK: u64 = 64 * 1024;
const AGE_TAG: u64 = 16;

/// Size of the pieces passed between the blocking encrypt/decrypt thread and
/// the network. Memory in flight per transfer is at most
/// PIPE_CHUNK × PIPE_DEPTH (~2 MiB).
pub const PIPE_CHUNK: usize = 256 * 1024;
const PIPE_DEPTH: usize = 8;

// The key derivation. Fixed parameters and salt, so the same passphrase
// always produces the same key on any machine — that's what lets a rebuilt
// device restore with nothing but the passphrase. log_n=18, r=8 is scrypt's
// recommended interactive strength (256 MiB of RAM, ~1s, once at startup).
// Trade-off: with a fixed salt, an attacker holding ciphertext could
// precompute guesses across all users rather than per file, so passphrase
// strength matters (the client warns below 12 characters). Moving to a
// per-account salt is on the launch checklist as later hardening.
const KDF_LOG_N: u8 = 18;
const KDF_R: u32 = 8;
const KDF_P: u32 = 1;
const KDF_SALT: &[u8] = b"backup-buddies/file-key/v1";

pub struct Keys {
    recipient: age::x25519::Recipient,
    identity: age::x25519::Identity,
    /// For files encrypted before this change (client ≤0.4.x).
    legacy: age::scrypt::Identity,
}

static KEYS: OnceLock<Keys> = OnceLock::new();

/// Derives the file key from the passphrase. Call once at startup, before
/// anything is encrypted or decrypted; later calls are no-ops.
pub fn init(passphrase: &SecretString) -> Result<()> {
    if KEYS.get().is_some() {
        return Ok(());
    }
    let mut secret = [0u8; 32];
    let params = scrypt::Params::new(KDF_LOG_N, KDF_R, KDF_P, 32).expect("valid scrypt params");
    scrypt::scrypt(passphrase.expose_secret().as_bytes(), KDF_SALT, &params, &mut secret)
        .expect("32-byte output is valid");

    // age only builds an X25519 identity from its text form, so encode the
    // derived bytes the way age itself does.
    let hrp = bech32::Hrp::parse("age-secret-key-").expect("valid HRP");
    let encoded = bech32::encode::<bech32::Bech32>(hrp, &secret).expect("encodable key");
    secret.fill(0);
    let identity: age::x25519::Identity =
        encoded.to_uppercase().parse().map_err(|e: &str| anyhow::anyhow!("deriving file key: {e}"))?;
    let recipient = identity.to_public();
    let legacy = age::scrypt::Identity::new(passphrase.clone());
    let _ = KEYS.set(Keys { recipient, identity, legacy });
    Ok(())
}

pub fn keys() -> &'static Keys {
    KEYS.get().expect("crypto::init must run at startup")
}

/// Exact encrypted size of a file, given the length of its age header +
/// nonce (`prefix_len`, which varies a little between encryptions — age
/// randomizes the header — so it's measured per file, see `encrypt_file`)
/// and its plaintext length. Lets a sender state the size up front, so the
/// buddy can check pledge and disk space before accepting.
pub fn ciphertext_len(prefix_len: u64, plaintext_len: u64) -> u64 {
    let chunks = plaintext_len.div_ceil(AGE_CHUNK).max(1);
    prefix_len + plaintext_len + chunks * AGE_TAG
}

/// `Write` that hands bytes to an async task in PIPE_CHUNK pieces. Used as
/// age's output on a blocking thread.
struct ChannelWriter {
    tx: mpsc::Sender<Vec<u8>>,
    buf: Vec<u8>,
    written: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.written.fetch_add(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.buf.extend_from_slice(data);
        if self.buf.len() >= PIPE_CHUNK {
            self.flush()?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(PIPE_CHUNK));
            self.tx
                .blocking_send(chunk)
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver went away"))?;
        }
        Ok(())
    }
}

/// `Read` fed by an async task in chunks. Used as age's input on a blocking
/// thread. An `Err` chunk aborts the read with that error.
struct ChannelReader {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.cur.len() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.cur = chunk;
                    self.pos = 0;
                }
                Some(Err(err)) => return Err(err),
                None => return Ok(0), // end of stream
            }
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A file being encrypted on a blocking thread.
pub struct EncryptStream {
    /// Exact total ciphertext size. Known as soon as encryption has started
    /// (before any data is read).
    pub ciphertext_len: tokio::sync::oneshot::Receiver<u64>,
    /// Ciphertext in PIPE_CHUNK pieces.
    pub chunks: mpsc::Receiver<Vec<u8>>,
    /// How encryption ended.
    pub done: tokio::task::JoinHandle<Result<()>>,
}

/// Encrypts exactly `plaintext_len` bytes of the file at `path` on a
/// blocking thread. Fails if the file is shorter than expected (it changed
/// since it was measured), so the stated size always matches what's sent.
pub fn encrypt_file(path: std::path::PathBuf, plaintext_len: u64) -> EncryptStream {
    let (tx, rx) = mpsc::channel(PIPE_DEPTH);
    let (len_tx, len_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::task::spawn_blocking(move || -> Result<()> {
        let file = std::fs::File::open(&path).context("failed to open local file")?;
        let mut input = io::BufReader::with_capacity(PIPE_CHUNK, file).take(plaintext_len);
        let written = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let out = ChannelWriter { tx, buf: Vec::with_capacity(PIPE_CHUNK), written: written.clone() };
        let recipient = &keys().recipient;
        let encryptor = age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient))
            .context("failed to set up encryption")?;
        let mut writer = encryptor.wrap_output(out).context("failed to start encryption")?;
        // wrap_output has written the header and nonce and nothing else
        // yet (age buffers payload until a full chunk), so this is the
        // per-file prefix length.
        let prefix_len = written.load(std::sync::atomic::Ordering::Relaxed);
        let _ = len_tx.send(ciphertext_len(prefix_len, plaintext_len));
        let copied = io::copy(&mut input, &mut writer).context("failed to read local file")?;
        if copied != plaintext_len {
            anyhow::bail!("file changed while it was being sent (expected {plaintext_len} bytes, read {copied}) — will retry next cycle");
        }
        let mut out = writer.finish().context("failed to finish encryption")?;
        out.flush().context("failed to hand off encrypted data")?;
        Ok(())
    });
    EncryptStream { ciphertext_len: len_rx, chunks: rx, done: handle }
}

/// Decrypts a ciphertext stream into `dest` on a blocking thread. Feed it
/// ciphertext chunks through the returned sender (an `Err` aborts), then
/// drop the sender and await the handle. Writes to a temporary file next to
/// `dest` and renames it into place only after the whole file decrypted and
/// authenticated, so a failed restore never leaves a truncated file behind.
pub fn decrypt_to_file(
    dest: std::path::PathBuf,
) -> (mpsc::Sender<io::Result<Vec<u8>>>, tokio::task::JoinHandle<Result<u64>>) {
    let (tx, rx) = mpsc::channel(PIPE_DEPTH);
    let handle = tokio::task::spawn_blocking(move || -> Result<u64> {
        let input = io::BufReader::with_capacity(PIPE_CHUNK, ChannelReader { rx, cur: Vec::new(), pos: 0 });
        let decryptor = age::Decryptor::new_buffered(input).context("not a valid encrypted file")?;
        let k = keys();
        let identities: [&dyn age::Identity; 2] = [&k.identity, &k.legacy];
        let mut reader = decryptor
            .decrypt(identities.into_iter())
            .context("failed to decrypt file — wrong passphrase?")?;

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).context("failed to create restore directory")?;
        }
        let tmp = tmp_sibling(&dest);
        let result = (|| -> Result<u64> {
            let mut out = io::BufWriter::with_capacity(PIPE_CHUNK, std::fs::File::create(&tmp).context("failed to create restored file")?);
            let n = io::copy(&mut reader, &mut out).context("failed to decrypt file")?;
            out.into_inner().map_err(|e| e.into_error()).context("failed to write restored file")?.sync_all().ok();
            Ok(n)
        })();
        match result {
            Ok(n) => {
                std::fs::rename(&tmp, &dest).context("failed to move restored file into place")?;
                Ok(n)
            }
            Err(err) => {
                let _ = std::fs::remove_file(&tmp);
                Err(err)
            }
        }
    });
    (tx, handle)
}

fn tmp_sibling(dest: &std::path::Path) -> std::path::PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dest.with_file_name(format!(".{name}.restoring-{}", std::process::id()))
}

/// Streaming SHA-256 of a local file on a blocking thread — constant memory
/// regardless of size.
pub async fn sha256_file(path: std::path::PathBuf) -> Result<String> {
    tokio::task::spawn_blocking(move || -> Result<String> {
        use sha2::{Digest, Sha256};
        let file = std::fs::File::open(&path).context("failed to read file for hashing")?;
        let mut reader = io::BufReader::with_capacity(PIPE_CHUNK, file);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; PIPE_CHUNK];
        loop {
            let n = reader.read(&mut buf).context("failed to read file for hashing")?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .context("hashing task failed")?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // The key lives in a process-wide OnceLock and tests share a process,
    // so every test (here and in e2e_tests) uses this same passphrase.
    pub(crate) const TEST_PASSPHRASE: &str = "an end to end test passphrase";

    fn setup() {
        init(&SecretString::from(TEST_PASSPHRASE.to_string())).unwrap();
    }

    // The size announced before sending must equal what's actually sent,
    // at chunk boundaries and everywhere between. Several runs per size,
    // since age's header length varies between encryptions.
    #[tokio::test(flavor = "multi_thread")]
    async fn announced_size_matches_sent_bytes() {
        setup();
        let dir = std::env::temp_dir().join(format!("bb-crypto-len-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in [0usize, 1, 17, 65535, 65536, 65537, 131072, 200_000, 262_144, 262_145] {
            let src = dir.join(format!("f{n}"));
            std::fs::write(&src, vec![7u8; n]).unwrap();
            for _ in 0..5 {
                let mut enc = encrypt_file(src.clone(), n as u64);
                let announced = (&mut enc.ciphertext_len).await.unwrap();
                let mut sent = 0u64;
                while let Some(c) = enc.chunks.recv().await {
                    sent += c.len() as u64;
                }
                enc.done.await.unwrap().unwrap();
                assert_eq!(announced, sent, "plaintext {n}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_passphrase_gives_same_key() {
        setup();
        let a = age::encrypt(&keys().recipient, b"hello").unwrap();
        // Re-derive independently and decrypt with that.
        let mut secret = [0u8; 32];
        let params = scrypt::Params::new(KDF_LOG_N, KDF_R, KDF_P, 32).unwrap();
        scrypt::scrypt(TEST_PASSPHRASE.as_bytes(), KDF_SALT, &params, &mut secret).unwrap();
        let hrp = bech32::Hrp::parse("age-secret-key-").unwrap();
        let id: age::x25519::Identity =
            bech32::encode::<bech32::Bech32>(hrp, &secret).unwrap().to_uppercase().parse().unwrap();
        assert_eq!(age::decrypt(&id, &a).unwrap(), b"hello");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streams_round_trip_including_legacy_files() {
        setup();
        let dir = std::env::temp_dir().join(format!("bb-crypto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.bin");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src, &data).unwrap();

        // New format, streamed both ways.
        let mut enc = encrypt_file(src.clone(), data.len() as u64);
        let announced = (&mut enc.ciphertext_len).await.unwrap();
        let (tx, dec) = decrypt_to_file(dir.join("out.bin"));
        let mut total = 0u64;
        while let Some(chunk) = enc.chunks.recv().await {
            total += chunk.len() as u64;
            tx.send(Ok(chunk)).await.unwrap();
        }
        drop(tx);
        enc.done.await.unwrap().unwrap();
        assert_eq!(total, announced);
        assert_eq!(dec.await.unwrap().unwrap(), data.len() as u64);
        assert_eq!(std::fs::read(dir.join("out.bin")).unwrap(), data);

        // A file encrypted the old way (scrypt, client ≤0.4) still restores.
        let legacy = age::encrypt(
            &age::scrypt::Recipient::new(SecretString::from(TEST_PASSPHRASE.to_string())),
            b"old file",
        )
        .unwrap();
        let (tx, dec) = decrypt_to_file(dir.join("old.bin"));
        tx.send(Ok(legacy)).await.unwrap();
        drop(tx);
        dec.await.unwrap().unwrap();
        assert_eq!(std::fs::read(dir.join("old.bin")).unwrap(), b"old file");

        // File shrinks mid-send: refused, never sends a short file.
        let mut enc = encrypt_file(src.clone(), data.len() as u64 + 10);
        while enc.chunks.recv().await.is_some() {}
        assert!(enc.done.await.unwrap().is_err());

        // Wrong passphrase / corrupt data: no file left behind.
        let (tx, dec) = decrypt_to_file(dir.join("bad.bin"));
        tx.send(Ok(b"not an age file at all".to_vec())).await.unwrap();
        drop(tx);
        assert!(dec.await.unwrap().is_err());
        assert!(!dir.join("bad.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
