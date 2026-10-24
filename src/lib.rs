//! Per-function resource-cost snapshots for Soroban contracts.
//!
//! In a test, after a contract call, record what that call cost:
//!
//! ```ignore
//! client.transfer(&a, &b, &100);
//! soroban_cost_snapshot::cost_snapshot!(env, "token::transfer");
//! ```
//!
//! Each call writes one small JSON file into the snapshot directory
//! (`SOROBAN_COST_SNAPSHOT_DIR`, default `target/soroban-cost-snapshot`).
//! The `soroban-cost` CLI merges those files into one snapshot and diffs two
//! snapshots (for example the committed baseline against a pull request).
//!
//! This crate does not depend on `soroban-sdk`: the macro expands to code
//! that uses *your* `soroban-sdk` (it needs `Env::cost_estimate()`, which is
//! available in soroban-sdk 29 and, as far as we know, some earlier versions).

#![forbid(unsafe_code)]

pub mod diff;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Snapshot file format version.
pub const FORMAT_VERSION: u32 = 1;
/// Environment variable that selects the directory for recorded entries.
pub const DIR_ENV: &str = "SOROBAN_COST_SNAPSHOT_DIR";
/// Default directory (relative to the current directory, i.e. the package
/// root when run through `cargo test`).
pub const DEFAULT_DIR: &str = "target/soroban-cost-snapshot";
/// Maximum length of an entry name. Kept so that the hex-encoded file name
/// plus temp-file prefix stays well under the common 255-byte file-name limit.
pub const MAX_NAME_LEN: usize = 96;
/// Maximum size of any JSON file this crate reads (16 MiB).
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum number of entries in one snapshot.
pub const MAX_ENTRIES: usize = 100_000;

/// The measured cost of one contract invocation.
///
/// All values are non-negative. `fee_stroops` is the SDK's *estimate* using a
/// hardcoded mainnet fee table, not the exact network fee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub instructions: u64,
    pub mem_bytes: u64,
    pub read_entries: u64,
    pub write_entries: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub events_bytes: u64,
    pub fee_stroops: u64,
}

/// A full snapshot: entry name -> cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    #[serde(deserialize_with = "deserialize_unique_entries")]
    pub entries: BTreeMap<String, Entry>,
}

/// Like the default map deserializer, but rejects duplicate keys (serde_json
/// would otherwise silently keep the last one, which could hide an edited
/// value from a reviewer) and caps the number of entries.
fn deserialize_unique_entries<'de, D>(d: D) -> Result<BTreeMap<String, Entry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = BTreeMap<String, Entry>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a map of entry names to costs")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut m: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(k) = m.next_key::<String>()? {
                if out.contains_key(&k) {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate entry name {:?}",
                        k
                    )));
                }
                if out.len() >= MAX_ENTRIES {
                    return Err(serde::de::Error::custom(format!(
                        "more than {} entries",
                        MAX_ENTRIES
                    )));
                }
                let v: Entry = m.next_value()?;
                out.insert(k, v);
            }
            Ok(out)
        }
    }
    d.deserialize_map(V)
}

/// One recorded entry as written by [`record`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFile {
    version: u32,
    name: String,
    entry: Entry,
}

/// Errors from this crate.
#[derive(Debug)]
pub enum Error {
    Io(PathBuf, io::Error),
    Json(PathBuf, serde_json::Error),
    InvalidName(String),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(p, e) => write!(f, "{}: {}", p.display(), e),
            Error::Json(p, e) => write!(f, "{}: invalid JSON: {}", p.display(), e),
            Error::InvalidName(n) => write!(
                f,
                "invalid entry name {:?}: use 1-{} characters from A-Z a-z 0-9 _ . : - /",
                n, MAX_NAME_LEN
            ),
            Error::Invalid(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for Error {}

/// Returns true if `name` is a valid entry name.
///
/// Names are deliberately restricted so they are safe to use in Markdown,
/// HTML, terminals and log annotations without escaping surprises.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-' | b'/'))
}

/// File name used for a recorded entry: hex of the name, so any valid name
/// maps to a unique, path-safe file name.
fn record_file_name(name: &str) -> String {
    let mut s = String::with_capacity(name.len() * 2 + 5);
    for b in name.bytes() {
        s.push_str(&format!("{:02x}", b));
    }
    s.push_str(".json");
    s
}

/// Directory where [`record`] writes entries.
pub fn snapshot_dir() -> PathBuf {
    match std::env::var_os(DIR_ENV) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(DEFAULT_DIR),
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Record `entry` under `name` in [`snapshot_dir`].
///
/// Writes atomically (temp file + rename), so parallel tests are safe.
/// Recording the same name twice keeps the last value.
pub fn record(name: &str, entry: Entry) -> Result<(), Error> {
    record_in(&snapshot_dir(), name, entry)
}

/// Like [`record`], with an explicit directory.
pub fn record_in(dir: &Path, name: &str, entry: Entry) -> Result<(), Error> {
    if !is_valid_name(name) {
        return Err(Error::InvalidName(name.to_string()));
    }
    fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    let rec = RecordFile {
        version: FORMAT_VERSION,
        name: name.to_string(),
        entry,
    };
    let json = serde_json::to_vec_pretty(&rec).map_err(|e| Error::Json(dir.to_path_buf(), e))?;
    let target = dir.join(record_file_name(name));
    let tmp = dir.join(format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        record_file_name(name)
    ));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| Error::Io(tmp.clone(), e))?;
        f.write_all(&json).map_err(|e| Error::Io(tmp.clone(), e))?;
        f.sync_all().map_err(|e| Error::Io(tmp.clone(), e))?;
    }
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::Io(target.clone(), e)
    })
}

/// Read a file with a size cap.
fn read_capped(path: &Path) -> Result<Vec<u8>, Error> {
    let f = fs::File::open(path).map_err(|e| Error::Io(path.to_path_buf(), e))?;
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| Error::Io(path.to_path_buf(), e))?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::Invalid(format!(
            "{}: file larger than {} bytes",
            path.display(),
            MAX_FILE_BYTES
        )));
    }
    Ok(buf)
}

/// Merge all recorded entry files in `dir` into one snapshot.
///
/// Only regular files ending in `.json` are read; temp files and anything
/// else are ignored. Symlinks are not followed.
pub fn collect(dir: &Path) -> Result<Snapshot, Error> {
    let mut snap = Snapshot {
        version: FORMAT_VERSION,
        entries: BTreeMap::new(),
    };
    let rd = fs::read_dir(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
    let mut paths: Vec<PathBuf> = Vec::new();
    for item in rd {
        let item = item.map_err(|e| Error::Io(dir.to_path_buf(), e))?;
        let ft = item.file_type().map_err(|e| Error::Io(item.path(), e))?;
        let fname = item.file_name();
        let fname = fname.to_string_lossy();
        if ft.is_file() && fname.ends_with(".json") && !fname.starts_with('.') {
            paths.push(item.path());
        }
    }
    paths.sort();
    for p in paths {
        let bytes = read_capped(&p)?;
        let rec: RecordFile =
            serde_json::from_slice(&bytes).map_err(|e| Error::Json(p.clone(), e))?;
        if rec.version != FORMAT_VERSION {
            return Err(Error::Invalid(format!(
                "{}: unsupported version {}",
                p.display(),
                rec.version
            )));
        }
        if !is_valid_name(&rec.name) {
            return Err(Error::InvalidName(rec.name));
        }
        if snap.entries.len() >= MAX_ENTRIES {
            return Err(Error::Invalid(format!("more than {} entries", MAX_ENTRIES)));
        }
        if snap.entries.contains_key(&rec.name) {
            return Err(Error::Invalid(format!(
                "{}: duplicate entry name {:?}",
                p.display(),
                rec.name
            )));
        }
        snap.entries.insert(rec.name, rec.entry);
    }
    Ok(snap)
}

impl Snapshot {
    /// Load and validate a snapshot file.
    pub fn load(path: &Path) -> Result<Snapshot, Error> {
        let bytes = read_capped(path)?;
        Self::from_json(&bytes).map_err(|e| match e {
            Error::Json(_, j) => Error::Json(path.to_path_buf(), j),
            Error::Invalid(m) => Error::Invalid(format!("{}: {}", path.display(), m)),
            other => other,
        })
    }

    /// Parse and validate a snapshot from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Snapshot, Error> {
        let snap: Snapshot =
            serde_json::from_slice(bytes).map_err(|e| Error::Json(PathBuf::from("<input>"), e))?;
        if snap.version != FORMAT_VERSION {
            return Err(Error::Invalid(format!(
                "unsupported version {}",
                snap.version
            )));
        }
        if snap.entries.len() > MAX_ENTRIES {
            return Err(Error::Invalid(format!("more than {} entries", MAX_ENTRIES)));
        }
        if let Some(bad) = snap.entries.keys().find(|k| !is_valid_name(k)) {
            return Err(Error::InvalidName(bad.clone()));
        }
        Ok(snap)
    }

    /// Serialize as stable, pretty JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("snapshot serializes");
        s.push('\n');
        s
    }

    /// Write the snapshot to `path`.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        fs::write(path, self.to_json()).map_err(|e| Error::Io(path.to_path_buf(), e))
    }
}

/// Convert a possibly negative SDK value into a `u64` (negative -> 0).
#[doc(hidden)]
pub fn __nonneg<T: TryInto<u64>>(v: T) -> u64 {
    v.try_into().unwrap_or(0)
}

/// Record the cost of the last top-level contract invocation in `env`.
///
/// ```ignore
/// client.increment(&5);
/// soroban_cost_snapshot::cost_snapshot!(env, "counter::increment");
/// ```
///
/// Panics (failing the test) if the name is invalid, if no invocation has
/// happened yet, or if the file cannot be written. Returns the [`Entry`].
#[macro_export]
macro_rules! cost_snapshot {
    ($env:expr, $name:expr) => {{
        let __ce = $env.cost_estimate();
        let __r = __ce.resources();
        let __f = __ce.fee();
        let __entry = $crate::Entry {
            instructions: $crate::__nonneg(__r.instructions),
            mem_bytes: $crate::__nonneg(__r.mem_bytes),
            read_entries: $crate::__nonneg(__r.disk_read_entries)
                .saturating_add($crate::__nonneg(__r.memory_read_entries)),
            write_entries: $crate::__nonneg(__r.write_entries),
            read_bytes: $crate::__nonneg(__r.disk_read_bytes),
            write_bytes: $crate::__nonneg(__r.write_bytes),
            events_bytes: $crate::__nonneg(__r.contract_events_size_bytes),
            fee_stroops: $crate::__nonneg(__f.total),
        };
        if let Err(e) = $crate::record($name, __entry) {
            panic!("soroban-cost-snapshot: {}", e);
        }
        __entry
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: u64) -> Entry {
        Entry {
            instructions: i,
            fee_stroops: i / 10,
            ..Default::default()
        }
    }

    #[test]
    fn names() {
        assert!(is_valid_name("token::transfer"));
        assert!(is_valid_name("a/b-c.d_e"));
        assert!(!is_valid_name(""));
        assert!(is_valid_name("../x")); // valid as a *name*; files use hex, see below
        assert!(!is_valid_name("a b"));
        assert!(!is_valid_name("a\nb"));
        assert!(!is_valid_name("`x`"));
        assert!(!is_valid_name("a|b"));
        assert!(!is_valid_name("<script>"));
        assert!(!is_valid_name("naïve"));
        assert!(!is_valid_name(&"x".repeat(MAX_NAME_LEN + 1)));
        assert!(is_valid_name(&"x".repeat(MAX_NAME_LEN)));
    }

    #[test]
    fn file_name_is_path_safe() {
        let f = record_file_name("../../etc/passwd");
        assert!(f.ends_with(".json"));
        let stem = f.trim_end_matches(".json");
        assert!(stem.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(record_file_name("a:b"), record_file_name("a/b"));
    }

    #[test]
    fn record_and_collect_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "a::x", e(100)).unwrap();
        record_in(d.path(), "b::y", e(200)).unwrap();
        record_in(d.path(), "a::x", e(150)).unwrap(); // overwrite
        let s = collect(d.path()).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert_eq!(s.entries["a::x"].instructions, 150);
        let back = Snapshot::from_json(s.to_json().as_bytes()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn record_rejects_bad_name() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            record_in(d.path(), "evil name", e(1)),
            Err(Error::InvalidName(_))
        ));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn collect_ignores_tmp_and_other_files() {
        let d = tempfile::tempdir().unwrap();
        record_in(d.path(), "ok", e(1)).unwrap();
        fs::write(d.path().join(".tmp-1-2-x.json"), "garbage").unwrap();
        fs::write(d.path().join("notes.txt"), "hi").unwrap();
        fs::create_dir(d.path().join("sub.json")).unwrap();
        let s = collect(d.path()).unwrap();
        assert_eq!(s.entries.len(), 1);
    }

    #[test]
    fn collect_rejects_tampered_record() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("x.json"),
            r#"{"version":1,"name":"a|b","entry":{"instructions":1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}}"#,
        )
        .unwrap();
        assert!(matches!(collect(d.path()), Err(Error::InvalidName(_))));
    }

    #[test]
    fn from_json_validation() {
        assert!(Snapshot::from_json(b"{").is_err());
        assert!(Snapshot::from_json(br#"{"version":2,"entries":{}}"#).is_err());
        assert!(Snapshot::from_json(br#"{"version":1,"entries":{},"extra":1}"#).is_err());
        assert!(Snapshot::from_json(br#"{"version":1,"entries":{"<b>":{"instructions":1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}}}"#).is_err());
        // negative numbers are rejected by u64
        assert!(Snapshot::from_json(br#"{"version":1,"entries":{"a":{"instructions":-1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}}}"#).is_err());
        assert!(Snapshot::from_json(br#"{"version":1,"entries":{}}"#).is_ok());
    }

    #[test]
    fn load_rejects_oversized_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.json");
        let f = fs::File::create(&p).unwrap();
        f.set_len(MAX_FILE_BYTES + 10).unwrap();
        assert!(matches!(Snapshot::load(&p), Err(Error::Invalid(_))));
    }

    #[test]
    fn duplicate_keys_rejected() {
        let e = r#"{"instructions":1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}"#;
        let j = format!(r#"{{"version":1,"entries":{{"a":{e},"a":{e}}}}}"#);
        assert!(Snapshot::from_json(j.as_bytes()).is_err());
        let ok = format!(r#"{{"version":1,"entries":{{"a":{e},"b":{e}}}}}"#);
        assert!(Snapshot::from_json(ok.as_bytes()).is_ok());
    }

    #[test]
    fn collect_rejects_two_files_with_same_name() {
        let d = tempfile::tempdir().unwrap();
        let body = r#"{"version":1,"name":"zz","entry":{"instructions":1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}}"#;
        fs::write(d.path().join("aaa.json"), body).unwrap();
        fs::write(d.path().join("bbb.json"), body).unwrap();
        assert!(collect(d.path()).is_err());
    }

    #[test]
    fn nonneg() {
        assert_eq!(__nonneg(-5i64), 0);
        assert_eq!(__nonneg(7u32), 7);
        assert_eq!(__nonneg(i64::MAX), i64::MAX as u64);
    }

    #[test]
    fn parallel_records_are_safe() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let hs: Vec<_> = (0..16)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..50u64 {
                        record_in(&dir, &format!("t{}::n{}", t, i % 5), e(i)).unwrap();
                        record_in(&dir, "shared", e(i)).unwrap();
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let s = collect(&dir).unwrap();
        assert_eq!(s.entries.len(), 16 * 5 + 1);
        // no temp files left behind
        assert!(fs::read_dir(&dir).unwrap().all(|f| !f
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp-")));
    }
}
