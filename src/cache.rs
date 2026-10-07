//! Persistent per-repository parse cache.
//!
//! One file per mapped root under `$REPOMAP_CACHE_DIR`, else
//! `$XDG_CACHE_HOME/repomap`, else `~/.cache/repomap`; `REPOMAP_CACHE=off`
//! disables it. Entries are keyed by relative path and invalidated by
//! modification time and size. The file is salted with the running
//! executable's identity, so a rebuilt binary (whose record layout may have
//! changed) never decodes an older binary's bytes. Payloads are opaque here;
//! `repo_map` owns the record encoding.

use std::{
    collections::HashMap,
    env, fs,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

const MAGIC: &[u8; 8] = b"RMAPC\x01\0\0";
/// Cache files kept per cache directory before the least recently written go.
const MAX_CACHE_FILES: usize = 256;

pub(crate) struct Entry {
    pub modified: u64,
    pub len: u64,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub(crate) struct Store {
    file: Option<PathBuf>,
    entries: HashMap<String, IndexedEntry>,
    snapshot: Option<Backing>,
    spool: Option<Backing>,
    dirty: bool,
}

pub(crate) fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub(crate) fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn directory() -> Option<PathBuf> {
    if env::var("REPOMAP_CACHE").is_ok_and(|value| value.eq_ignore_ascii_case("off")) {
        return None;
    }
    if cfg!(test) {
        return Some(env::temp_dir().join(format!("repomap-test-cache-{}", std::process::id())));
    }
    if let Some(explicit) = env::var_os("REPOMAP_CACHE_DIR") {
        return Some(PathBuf::from(explicit));
    }
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
        .map(|base| base.join("repomap"))
}

fn salt() -> u64 {
    static SALT: OnceLock<u64> = OnceLock::new();
    *SALT.get_or_init(|| {
        let identity = env::current_exe()
            .and_then(fs::metadata)
            .map(|metadata| {
                format!(
                    "{}:{}",
                    metadata.len(),
                    metadata.modified().map(nanos).unwrap_or(0)
                )
            })
            .unwrap_or_default();
        fnv64(format!("{}:{identity}", env!("CARGO_PKG_VERSION")).as_bytes())
    })
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        if self.0.len() < count {
            return None;
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Some(head)
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u64()?).ok()?;
        self.take(len)
    }
}

// Only names and locations are resident. Payloads stay on disk until get.
#[derive(Clone, Copy)]
struct IndexedEntry {
    modified: u64,
    len: u64,
    offset: u64,
    payload_len: u64,
    spooled: bool,
}

struct Backing {
    handle: Mutex<fs::File>,
    // Published snapshots have no temporary name to clean up.
    temporary: Option<PathBuf>,
}

impl Drop for Backing {
    fn drop(&mut self) {
        if let Some(path) = &self.temporary {
            let _ = fs::remove_file(path);
        }
    }
}

impl Backing {
    fn temporary(directory: &Path) -> io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(directory)?;
        loop {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".repomap-{}-{id}.tmp", std::process::id()));
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(handle) => {
                    return Ok(Self {
                        handle: Mutex::new(handle),
                        temporary: Some(path),
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, fs::File> {
        self.handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn invalid_cache() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid parse cache")
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn scan(handle: &mut fs::File) -> io::Result<HashMap<String, IndexedEntry>> {
    let size = handle.metadata()?.len();
    handle.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(handle);
    let mut magic = [0; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC || read_u64(&mut reader)? != salt() {
        return Err(invalid_cache());
    }
    let count = read_u64(&mut reader)?;
    // Every entry has at least four u64s. Never reserve from untrusted counts.
    if count > size.saturating_sub(24) / 32 {
        return Err(invalid_cache());
    }
    let mut entries = HashMap::new();
    for _ in 0..count {
        let name_len = read_u64(&mut reader)?;
        let position = reader.stream_position()?;
        // Paths cannot consume an arbitrary corrupt file's size in memory.
        if name_len > 1 << 20 || name_len > size.saturating_sub(position).saturating_sub(24) {
            return Err(invalid_cache());
        }
        let mut name = vec![0; name_len as usize];
        reader.read_exact(&mut name)?;
        let relative = String::from_utf8(name).map_err(|_| invalid_cache())?;
        let modified = read_u64(&mut reader)?;
        let len = read_u64(&mut reader)?;
        let payload_len = read_u64(&mut reader)?;
        let offset = reader.stream_position()?;
        let end = offset
            .checked_add(payload_len)
            .filter(|end| *end <= size)
            .ok_or_else(invalid_cache)?;
        reader.seek(SeekFrom::Start(end))?;
        entries.insert(
            relative,
            IndexedEntry {
                modified,
                len,
                offset,
                payload_len,
                spooled: false,
            },
        );
    }
    if reader.stream_position()? != size {
        return Err(invalid_cache());
    }
    Ok(entries)
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

impl Store {
    fn load(root: &Path) -> Self {
        let Some(directory) = directory() else {
            return Self::default();
        };
        Self::load_file(directory.join(format!(
            "{:016x}.bin",
            fnv64(root.as_os_str().as_encoded_bytes())
        )))
    }

    fn load_file(file: PathBuf) -> Self {
        let mut store = Self {
            file: Some(file.clone()),
            ..Self::default()
        };
        if let Ok(mut handle) = fs::File::open(file) {
            if let Ok(entries) = scan(&mut handle) {
                store.entries = entries;
                store.snapshot = Some(Backing {
                    handle: Mutex::new(handle),
                    temporary: None,
                });
            }
        }
        store
    }

    fn backing(&self, entry: &IndexedEntry) -> Option<&Backing> {
        if entry.spooled {
            self.spool.as_ref()
        } else {
            self.snapshot.as_ref()
        }
    }

    pub fn get(&self, relative: &str, modified: u64, len: u64) -> Option<Vec<u8>> {
        let entry = self
            .entries
            .get(relative)
            .filter(|entry| entry.modified == modified && entry.len == len)?;
        let mut handle = self.backing(entry)?.lock();
        let end = entry.offset.checked_add(entry.payload_len)?;
        if end > handle.metadata().ok()?.len() {
            return None;
        }
        let count = usize::try_from(entry.payload_len).ok()?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(count).ok()?;
        payload.resize(count, 0);
        handle.seek(SeekFrom::Start(entry.offset)).ok()?;
        handle.read_exact(&mut payload).ok()?;
        Some(payload)
    }

    pub fn insert(&mut self, relative: String, entry: Entry) {
        // An unavailable/disabled cache is a miss, not a RAM payload store.
        let result = (|| -> io::Result<IndexedEntry> {
            if self.spool.is_none() {
                let directory = self
                    .file
                    .as_ref()
                    .and_then(|file| file.parent())
                    .ok_or_else(invalid_cache)?;
                self.spool = Some(Backing::temporary(directory)?);
            }
            let mut handle = self.spool.as_ref().unwrap().lock();
            let offset = handle.seek(SeekFrom::End(0))?;
            handle.write_all(&entry.payload)?;
            Ok(IndexedEntry {
                modified: entry.modified,
                len: entry.len,
                offset,
                payload_len: entry.payload.len() as u64,
                spooled: true,
            })
        })();
        match result {
            Ok(indexed) => {
                self.entries.insert(relative, indexed);
                self.dirty = true;
            }
            Err(_) => {
                // Do not keep an older value after a failed replacement.
                self.dirty |= self.entries.remove(&relative).is_some();
            }
        }
    }

    /// Drop entries for files that no longer exist.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.entries.len();
        self.entries.retain(|relative, _| keep(relative));
        self.dirty |= self.entries.len() != before;
    }

    /// Stream a sorted snapshot and atomically publish it. All failures are
    /// cache misses: the cache is an optimization, never a correctness input.
    pub fn save(&mut self) {
        let Some(file) = self.file.as_ref().filter(|_| self.dirty) else {
            return;
        };
        let Some(directory) = file.parent() else {
            return;
        };
        let fresh = !file.exists();
        let result = (|| -> io::Result<(Backing, HashMap<String, IndexedEntry>)> {
            let mut output = Backing::temporary(directory)?;
            let mut index = HashMap::new();
            {
                let mut handle = output.lock();
                let mut writer = BufWriter::new(&mut *handle);
                writer.write_all(MAGIC)?;
                writer.write_all(&salt().to_le_bytes())?;
                writer.write_all(&(self.entries.len() as u64).to_le_bytes())?;
                let mut keys = self.entries.keys().collect::<Vec<_>>();
                keys.sort();
                let mut position = 24u64;
                for relative in keys {
                    let entry = self.entries[relative];
                    writer.write_all(&(relative.len() as u64).to_le_bytes())?;
                    writer.write_all(relative.as_bytes())?;
                    writer.write_all(&entry.modified.to_le_bytes())?;
                    writer.write_all(&entry.len.to_le_bytes())?;
                    writer.write_all(&entry.payload_len.to_le_bytes())?;
                    position = position
                        .checked_add(32)
                        .and_then(|p| p.checked_add(relative.len() as u64))
                        .ok_or_else(invalid_cache)?;
                    let mut source = self.backing(&entry).ok_or_else(invalid_cache)?.lock();
                    source.seek(SeekFrom::Start(entry.offset))?;
                    let copied =
                        io::copy(&mut (&mut *source).take(entry.payload_len), &mut writer)?;
                    if copied != entry.payload_len {
                        return Err(invalid_cache());
                    }
                    index.insert(
                        relative.clone(),
                        IndexedEntry {
                            offset: position,
                            spooled: false,
                            ..entry
                        },
                    );
                    position = position
                        .checked_add(entry.payload_len)
                        .ok_or_else(invalid_cache)?;
                }
                writer.flush()?;
            }
            fs::rename(output.temporary.as_ref().unwrap(), file)?;
            output.temporary = None;
            Ok((output, index))
        })();
        if let Ok((snapshot, entries)) = result {
            self.snapshot = Some(snapshot);
            self.entries = entries;
            self.spool = None;
            self.dirty = false;
            if fresh {
                prune(directory);
            }
        }
    }
}

fn prune(directory: &Path) {
    let Ok(listing) = fs::read_dir(directory) else {
        return;
    };
    let mut files = listing
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "bin"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect::<Vec<_>>();
    if files.len() <= MAX_CACHE_FILES {
        return;
    }
    files.sort();
    for (_, path) in &files[..files.len() - MAX_CACHE_FILES] {
        let _ = fs::remove_file(path);
    }
}

/// The store for `root`, loaded from disk on first use in this process and
/// then kept as a disk-backed index, so repeated maps scan the snapshot only
/// once and read just the payloads they need.
pub(crate) fn open(root: &Path) -> impl std::ops::DerefMut<Target = Store> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Store>>> = OnceLock::new();
    let guard = STORES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    StoreGuard {
        guard,
        root: root.to_path_buf(),
    }
    .init()
}

struct StoreGuard {
    guard: MutexGuard<'static, HashMap<PathBuf, Store>>,
    root: PathBuf,
}

impl StoreGuard {
    fn init(mut self) -> Self {
        if !self.guard.contains_key(&self.root) {
            if self.guard.len() >= 64 {
                self.guard.clear();
            }
            let store = Store::load(&self.root);
            self.guard.insert(self.root.clone(), store);
        }
        self
    }
}

impl std::ops::Deref for StoreGuard {
    type Target = Store;
    fn deref(&self) -> &Store {
        &self.guard[&self.root]
    }
}

impl std::ops::DerefMut for StoreGuard {
    fn deref_mut(&mut self) -> &mut Store {
        self.guard
            .get_mut(&self.root)
            .expect("store inserted by init")
    }
}

/// Record encoding helpers shared with `repo_map`.
pub(crate) mod codec {
    pub fn put_u64(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    pub fn put_str(out: &mut Vec<u8>, value: &str) {
        super::put_bytes(out, value.as_bytes());
    }
    pub fn put_strs(out: &mut Vec<u8>, values: &[String]) {
        put_u64(out, values.len() as u64);
        for value in values {
            put_str(out, value);
        }
    }
    pub fn put_u64s(out: &mut Vec<u8>, values: &[u64]) {
        put_u64(out, values.len() as u64);
        for value in values {
            put_u64(out, *value);
        }
    }

    pub struct Reader<'a>(super::Reader<'a>);

    impl<'a> Reader<'a> {
        pub fn new(bytes: &'a [u8]) -> Self {
            Self(super::Reader(bytes))
        }
        pub fn u64(&mut self) -> Option<u64> {
            self.0.u64()
        }
        pub fn str(&mut self) -> Option<String> {
            String::from_utf8(self.0.bytes()?.to_vec()).ok()
        }
        pub fn strs(&mut self) -> Option<Vec<String>> {
            let count = usize::try_from(self.u64()?).ok()?;
            (0..count).map(|_| self.str()).collect()
        }
        pub fn u64s(&mut self) -> Option<Vec<u64>> {
            let count = usize::try_from(self.u64()?).ok()?;
            (0..count).map(|_| self.u64()).collect()
        }
        pub fn skip_strs(&mut self) -> Option<()> {
            let count = usize::try_from(self.u64()?).ok()?;
            for _ in 0..count {
                self.0.bytes()?;
            }
            Some(())
        }
        pub fn skip_u64s(&mut self) -> Option<()> {
            let count = usize::try_from(self.u64()?).ok()?;
            self.0.take(count.checked_mul(8)?)?;
            Some(())
        }
        pub fn finished(&self) -> bool {
            self.0 .0.is_empty()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(modified: u64, len: u64, payload: &[u8]) -> Entry {
        Entry {
            modified,
            len,
            payload: payload.to_vec(),
        }
    }

    fn encoded(name: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&salt().to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        put_bytes(&mut bytes, name);
        bytes.extend_from_slice(&7u64.to_le_bytes());
        bytes.extend_from_slice(&11u64.to_le_bytes());
        put_bytes(&mut bytes, payload);
        bytes
    }

    #[test]
    fn saved_entries_load_back_and_stale_salt_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let mut store = Store {
            file: Some(file.clone()),
            ..Store::default()
        };
        store.insert("src/lib.rs".into(), entry(7, 11, &[1, 2, 3]));
        let spool_path = store
            .spool
            .as_ref()
            .unwrap()
            .temporary
            .as_ref()
            .unwrap()
            .clone();
        assert_eq!(spool_path.parent(), Some(directory.path()));
        assert_eq!(fs::read(&spool_path).unwrap(), [1, 2, 3]);
        assert_eq!(store.get("src/lib.rs", 7, 11), Some(vec![1, 2, 3]));
        store.save();
        assert!(!store.dirty);
        assert!(store.spool.is_none());
        assert!(!spool_path.exists());
        assert_eq!(store.get("src/lib.rs", 7, 11), Some(vec![1, 2, 3]));
        let loaded = Store::load_file(file.clone());
        assert_eq!(loaded.get("src/lib.rs", 7, 11), Some(vec![1, 2, 3]));
        assert_eq!(
            (
                loaded.entries["src/lib.rs"].modified,
                loaded.entries["src/lib.rs"].len
            ),
            (7, 11)
        );
        assert_eq!(
            fs::read(&file).unwrap(),
            encoded(b"src/lib.rs", &[1, 2, 3]),
            "unchanged outer format"
        );

        let mut foreign = fs::read(&file).unwrap();
        foreign[MAGIC.len()] ^= 1;
        fs::write(&file, foreign).unwrap();
        assert!(
            Store::load_file(file.clone()).snapshot.is_none(),
            "another binary's cache must not decode"
        );
        fs::write(&file, &encoded(b"src/lib.rs", &[1, 2, 3])[..20]).unwrap();
        assert!(Store::load_file(file).snapshot.is_none(), "truncated file");
    }

    #[test]
    fn same_process_edits_fingerprints_and_retain() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let mut store = Store::load_file(file.clone());
        store.insert("z".into(), entry(1, 3, b"old"));
        store.insert("a".into(), entry(2, 0, b""));
        store.save();
        assert_eq!(store.get("z", 2, 3), None);
        assert_eq!(store.get("z", 1, 4), None);
        assert_eq!(store.get("missing", 1, 3), None);
        assert_eq!(store.get("a", 2, 0), Some(vec![]));
        store.insert("z".into(), entry(2, 4, b"edit"));
        assert_eq!(store.get("z", 1, 3), None);
        assert_eq!(store.get("z", 2, 4), Some(b"edit".to_vec()));
        store.save(); // Mix a snapshot entry with a spooled replacement.
        let mut expected = encoded(b"a", b"");
        expected[16..24].copy_from_slice(&2u64.to_le_bytes());
        expected[33..41].copy_from_slice(&2u64.to_le_bytes());
        expected[41..49].copy_from_slice(&0u64.to_le_bytes());
        let mut second = encoded(b"z", b"edit")[24..].to_vec();
        second[9..17].copy_from_slice(&2u64.to_le_bytes());
        second[17..25].copy_from_slice(&4u64.to_le_bytes());
        expected.extend(second);
        assert_eq!(fs::read(&file).unwrap(), expected, "sorted serialization");
        store.retain(|name| name == "z");
        assert!(store.dirty);
        store.save();
        let loaded = Store::load_file(file);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.get("a", 2, 0), None);
        assert_eq!(loaded.get("z", 2, 4), Some(b"edit".to_vec()));
        store.retain(|_| false);
        store.save();
        assert!(Store::load_file(store.file.clone().unwrap())
            .entries
            .is_empty());
    }

    #[test]
    fn snapshot_survives_other_writers_atomic_rename() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let mut first = Store::load_file(file.clone());
        first.insert("x".into(), entry(1, 3, b"old"));
        first.save();
        let observer = Store::load_file(file.clone());
        let mut second = Store::load_file(file.clone());
        second.insert("a".into(), entry(2, 5, b"shift"));
        second.insert("x".into(), entry(1, 3, b"new"));
        second.save();
        assert_eq!(observer.get("x", 1, 3), Some(b"old".to_vec()));
        assert_eq!(first.get("x", 1, 3), Some(b"old".to_vec()));
        assert_eq!(
            Store::load_file(file.clone()).get("x", 1, 3),
            Some(b"new".to_vec())
        );
        first.insert("z".into(), entry(3, 1, b"z"));
        first.save();
        assert_eq!(Store::load_file(file).get("x", 1, 3), Some(b"old".to_vec()));
        assert_eq!(second.get("x", 1, 3), Some(b"new".to_vec()));
    }

    #[test]
    fn corrupt_lengths_counts_encoding_and_truncation_are_misses() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let valid = encoded(b"x", b"payload");
        for end in 0..valid.len() {
            fs::write(&file, &valid[..end]).unwrap();
            assert!(
                Store::load_file(file.clone()).snapshot.is_none(),
                "accepted truncation at {end}"
            );
        }
        for offset in [16, 24, 49] {
            // count, name length, payload length
            let mut corrupt = valid.clone();
            corrupt[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            fs::write(&file, corrupt).unwrap();
            assert!(Store::load_file(file.clone()).snapshot.is_none());
        }
        let mut bad_name = valid.clone();
        bad_name[32] = 0xff;
        fs::write(&file, bad_name).unwrap();
        assert!(Store::load_file(file.clone()).snapshot.is_none());
        let mut bad_magic = valid.clone();
        bad_magic[0] ^= 1;
        fs::write(&file, bad_magic).unwrap();
        assert!(Store::load_file(file.clone()).snapshot.is_none());
        let mut trailing = valid;
        trailing.push(0);
        fs::write(&file, trailing).unwrap();
        assert!(Store::load_file(file).snapshot.is_none());
    }

    #[test]
    fn unavailable_storage_disabled_store_and_temporary_cleanup() {
        let mut disabled = Store::default();
        disabled.insert("x".into(), entry(1, 1, b"x"));
        disabled.save();
        assert!(disabled.entries.is_empty());
        assert!(disabled.spool.is_none());
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, b"file").unwrap();
        let mut unavailable = Store::load_file(blocker.join("root.bin"));
        unavailable.insert("x".into(), entry(1, 1, b"x"));
        unavailable.save();
        assert_eq!(unavailable.get("x", 1, 1), None);
        let mut store = Store::load_file(directory.path().join("root.bin"));
        store.insert("x".into(), entry(1, 1, b"x"));
        let path = store.spool.as_ref().unwrap().temporary.clone().unwrap();
        drop(store);
        assert!(!path.exists());
    }

    #[test]
    fn failed_save_preserves_published_file_and_spooled_reads() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let mut store = Store::load_file(file.clone());
        store.insert("x".into(), entry(1, 3, b"old"));
        store.save();
        let original = fs::read(&file).unwrap();
        store.insert("y".into(), entry(2, 3, b"new"));
        store.spool.as_ref().unwrap().lock().set_len(1).unwrap();
        assert_eq!(store.get("y", 2, 3), None);
        store.save();
        assert!(store.dirty);
        assert_eq!(fs::read(&file).unwrap(), original);
        assert_eq!(store.get("x", 1, 3), Some(b"old".to_vec()));
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            2,
            "failed output cleaned up"
        );
        store.insert("y".into(), entry(3, 5, b"fixed"));
        store.save();
        assert!(!store.dirty);
        assert_eq!(
            Store::load_file(file).get("y", 3, 5),
            Some(b"fixed".to_vec())
        );
    }

    #[test]
    fn many_payloads_are_disk_backed_and_stream_saved() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("root.bin");
        let mut store = Store::load_file(file.clone());
        let count = 32u64;
        let size = 256 * 1024;
        for id in 0..count {
            store.insert(
                format!("{id:02}"),
                Entry {
                    modified: id,
                    len: size,
                    payload: vec![id as u8; size as usize],
                },
            );
        }
        assert_eq!(std::mem::size_of::<IndexedEntry>(), 40);
        assert_eq!(
            store
                .spool
                .as_ref()
                .unwrap()
                .lock()
                .metadata()
                .unwrap()
                .len(),
            count * size
        );
        assert!(store.entries.values().all(|entry| entry.spooled));
        store.save();
        assert!(store.entries.values().all(|entry| !entry.spooled));
        let loaded = Store::load_file(file);
        for id in 0..count {
            assert_eq!(
                loaded.get(&format!("{id:02}"), id, size),
                Some(vec![id as u8; size as usize])
            );
        }
    }

    #[test]
    fn codec_skip_u64s_preserved_and_bounded() {
        let mut bytes = Vec::new();
        codec::put_u64s(&mut bytes, &[1, 2, 3]);
        codec::put_u64(&mut bytes, 9);
        let mut reader = codec::Reader::new(&bytes);
        assert_eq!(reader.skip_u64s(), Some(()));
        assert_eq!(reader.u64(), Some(9));
        assert!(reader.finished());
        assert_eq!(codec::Reader::new(&bytes[..31]).skip_u64s(), None);
        assert_eq!(
            codec::Reader::new(&u64::MAX.to_le_bytes()).skip_u64s(),
            None
        );
    }
}
