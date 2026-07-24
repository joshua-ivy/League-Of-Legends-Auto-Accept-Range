//! Background auto-fix for user-imported custom mods — the in-app port of
//! `tools/scope-champion-mod.py` (champion skins) plus a sweep-time hook for
//! `announcer_fix` (announcer packs dropped into the folder by hand rather
//! than downloaded through the Library).
//!
//! WHY: cslol `mkoverlay` merges every mod entry into EVERY game WAD whose
//! table-of-contents contains that entry's path-hash. Community fantomes
//! routinely include copies of widely-shared assets (a particle texture in
//! 227 game WADs, `item_metadata.rec` in 173...), turning a single-champion
//! skin into a 20+ GB, 2+ minute full-game rebuild at loadout time — the
//! League-session-wedging incident this fixes. Measured on one real mod:
//! raw 205 WADs / 22 GB / 164 s -> scoped 21 WADs / 4 GB / 4 s.
//!
//! Per-entry rule (path-hash = xxh64 of the lowercased path):
//!   KEEP  brand-new hashes, and entries in <= `MAX_ENTRY_WADS` game WADs —
//!         cheap and almost certainly intentional (own WAD, locale VO
//!         sibling, event twins, or ANOTHER champion in a multi-champion
//!         pack — learned the hard way: an "Ahri" pack legitimately carried
//!         Akali files in exactly 1 WAD; an ownership/family test wrongly strips that)
//!   DROP  only entries present in many WADs — truly shared assets the game
//!         already has everywhere. A skin never intends a global override.
//!
//! Sweeps run on a blocking thread at app startup and on every ChampSelect
//! entry, tracked in `state/mod_scope_state.json` so an already-processed
//! mod costs one metadata stat. Originals are kept as `<name>.bak`.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::skins::injection::tools::cslol_tools_dir;
use crate::skins::injection::zips::safe_extractall;
use crate::skins::slog::{log_error, log_info, log_warn};
use crate::skins::{announcer_fix, lcu_ext, paths};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// An entry in <= this many game WADs is specifically-targetable content
/// (own WAD, locale sibling, event twins, other champs in a multi-champ
/// pack) — presumed intentional. Beyond it, shared junk (34-227 WADs observed).
const MAX_ENTRY_WADS: u32 = 5;
/// Bump to force every mod through the scoper again after a rule change.
/// v2: count-only rule — v1's WAD-family membership requirement wrongly
/// stripped cross-champion content from multi-champion packs.
const SCOPE_RULE_VERSION: u32 = 2;

/// One sweep at a time — startup and ChampSelect entry can race.
static SWEEP_RUNNING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Processed-state tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScopeState {
    #[serde(default)]
    version: u32,
    /// mod path -> "mtime_secs:size" stamp at the time it was processed.
    #[serde(default)]
    processed: HashMap<String, String>,
}

fn state_path() -> PathBuf {
    paths::state_dir().join("mod_scope_state.json")
}

fn load_state() -> ScopeState {
    let state: ScopeState = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if state.version != SCOPE_RULE_VERSION {
        return ScopeState { version: SCOPE_RULE_VERSION, processed: HashMap::new() };
    }
    state
}

fn save_state(state: &ScopeState) {
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::create_dir_all(paths::state_dir());
        let _ = std::fs::write(state_path(), text);
    }
}

fn stamp(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{mtime}:{}", meta.len()))
}

// ---------------------------------------------------------------------------
// Game WAD index
// ---------------------------------------------------------------------------

struct GameIndex {
    /// path-hash -> number of game WADs containing it.
    counts: HashMap<u64, u32>,
}

/// Parse a WAD v3 table of contents into its entry path-hash set.
fn wad_toc(path: &Path) -> Option<HashSet<u64>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 2];
    f.read_exact(&mut magic).ok()?;
    if &magic != b"RW" {
        return None;
    }
    f.seek(SeekFrom::Start(268)).ok()?;
    let mut count_buf = [0u8; 4];
    f.read_exact(&mut count_buf).ok()?;
    let count = u32::from_le_bytes(count_buf) as usize;
    if count > 1_000_000 {
        return None; // implausible — refuse rather than allocate wild
    }
    let mut table = vec![0u8; count * 32];
    f.read_exact(&mut table).ok()?;
    Some((0..count).map(|i| u64::from_le_bytes(table[i * 32..i * 32 + 8].try_into().unwrap())).collect())
}

fn collect_wads(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_wads(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_lowercase().ends_with(".wad.client")) {
            out.push(path);
        }
    }
}

fn build_game_index(game_dir: &Path) -> Option<GameIndex> {
    let root = game_dir.join("DATA").join("FINAL");
    let mut wads = Vec::new();
    collect_wads(&root, &mut wads);
    if wads.is_empty() {
        log_warn!("[MOD_SCOPE] No game WADs under {} - skipping sweep", root.display());
        return None;
    }
    let mut counts: HashMap<u64, u32> = HashMap::new();
    let mut indexed = 0u32;
    for w in &wads {
        let Some(hashes) = wad_toc(w) else { continue };
        for &h in &hashes {
            *counts.entry(h).or_insert(0) += 1;
        }
        indexed += 1;
    }
    log_info!("[MOD_SCOPE] Indexed {indexed} game WADs ({} distinct entries)", counts.len());
    Some(GameIndex { counts })
}

impl GameIndex {
    fn count(&self, h: u64) -> u32 {
        self.counts.get(&h).copied().unwrap_or(0)
    }
}

// ── Game-index cache ─────────────────────────────────────────────────────────
// The game WADs only change on a Riot patch, so re-reading hundreds of WAD TOCs
// on every mod sweep (~11s) was pure waste — and, when it overlapped an
// injection, could push a heavy overlay build past the game-suspend safety
// window (skins then skipped -> default). Build once per patch, reuse from
// memory (session) and disk (across restarts), and — via the cloud store below
// — from a per-patch index the operator publishes so even the first build is free.
static INDEX_CACHE: OnceLock<Mutex<Option<(u64, Arc<GameIndex>)>>> = OnceLock::new();
fn index_cache() -> &'static Mutex<Option<(u64, Arc<GameIndex>)>> {
    INDEX_CACHE.get_or_init(|| Mutex::new(None))
}

/// Cheap fingerprint of the game WAD set (paths + sizes) — changes on a Riot
/// patch, stable otherwise. Metadata-only, so ~instant vs reading every TOC.
fn game_dir_fingerprint(game_dir: &Path) -> u64 {
    let root = game_dir.join("DATA").join("FINAL");
    let mut wads = Vec::new();
    collect_wads(&root, &mut wads);
    wads.sort();
    fn mix(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in &wads {
        mix(&mut h, w.to_string_lossy().as_bytes());
        let len = std::fs::metadata(w).map(|m| m.len()).unwrap_or(0);
        mix(&mut h, &len.to_le_bytes());
    }
    h
}

fn index_cache_path() -> PathBuf {
    crate::skins::paths::state_dir().join("game_wad_index.bin")
}

/// Wire/body format shared by the disk cache and the Cloudflare store:
/// `[count u64]` then count×`[hash u64][n u32]`. Machine-independent — the
/// per-machine disk file prefixes a fingerprint u64; the cloud blob does not.
fn serialize_index_wire(idx: &GameIndex) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8 + idx.counts.len() * 12);
    buf.extend_from_slice(&(idx.counts.len() as u64).to_le_bytes());
    for (&h, &c) in &idx.counts {
        buf.extend_from_slice(&h.to_le_bytes());
        buf.extend_from_slice(&c.to_le_bytes());
    }
    buf
}

fn parse_index_wire(body: &[u8]) -> Option<GameIndex> {
    let n = u64::from_le_bytes(body.get(0..8)?.try_into().ok()?) as usize;
    let rows = body.get(8..)?;
    if rows.len() < n.checked_mul(12)? {
        return None;
    }
    let mut counts = HashMap::with_capacity(n);
    for i in 0..n {
        let o = i * 12;
        counts.insert(
            u64::from_le_bytes(rows[o..o + 8].try_into().ok()?),
            u32::from_le_bytes(rows[o + 8..o + 12].try_into().ok()?),
        );
    }
    Some(GameIndex { counts })
}

/// Disk cache: `[fingerprint u64]` + the wire body. The fingerprint gates reuse
/// to the same game install/patch.
fn load_persisted_index(fp: u64) -> Option<Arc<GameIndex>> {
    let data = std::fs::read(index_cache_path()).ok()?;
    if data.len() < 8 || u64::from_le_bytes(data[0..8].try_into().ok()?) != fp {
        return None;
    }
    let idx = parse_index_wire(data.get(8..)?)?;
    log_info!("[MOD_SCOPE] Reused cached game index ({} entries) from disk", idx.counts.len());
    Some(Arc::new(idx))
}

fn persist_index(fp: u64, idx: &GameIndex) {
    let mut buf = Vec::with_capacity(8);
    buf.extend_from_slice(&fp.to_le_bytes());
    buf.extend_from_slice(&serialize_index_wire(idx));
    let path = index_cache_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(path, buf);
}

// ── Cloudflare-hosted per-patch index (Phase 2) ──────────────────────────────
// The index is identical for every user on a Riot patch, so computing it locally
// (~11s of TOC reads) once per machine per patch is wasted work N times over.
// The operator's client (the one holding the upload token) publishes the index
// keyed by an install-INDEPENDENT patch signature; every other client downloads
// it and never builds locally. Fetch/publish are best-effort — any failure falls
// straight back to the local build, so the app is never worse than Phase 1.
const CLOUD_INDEX_BASE: &str = "https://chud-index.jivy26.workers.dev";

/// Install-independent identity of the current game patch: FNV-1a over the
/// sorted (relative WAD path, size) set. Unlike [`game_dir_fingerprint`] this
/// strips the absolute install prefix (drive/root differ per machine) so it is
/// the SAME string for every user on a given patch — the cloud store key.
fn patch_signature(game_dir: &Path) -> String {
    let root = game_dir.join("DATA").join("FINAL");
    let mut wads = Vec::new();
    collect_wads(&root, &mut wads);
    let mut rel: Vec<(String, u64)> = wads
        .iter()
        .map(|w| {
            let r = w.strip_prefix(game_dir).unwrap_or(w).to_string_lossy().replace('\\', "/").to_lowercase();
            (r, std::fs::metadata(w).map(|m| m.len()).unwrap_or(0))
        })
        .collect();
    rel.sort();
    fn mix(h: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *h ^= b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (r, len) in &rel {
        mix(&mut h, r.as_bytes());
        mix(&mut h, &len.to_le_bytes());
    }
    format!("{h:016x}")
}

fn cloud_index_url(sig: &str) -> String {
    format!("{CLOUD_INDEX_BASE}/i/{sig}")
}

/// Operator upload token, present only on the publisher's machine (never shipped
/// in the app): env `CHUD_INDEX_UPLOAD_TOKEN` or `state/index-upload.token`.
/// Absent -> this client only ever downloads.
fn index_upload_token() -> Option<String> {
    if let Ok(t) = std::env::var("CHUD_INDEX_UPLOAD_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let t = std::fs::read_to_string(paths::state_dir().join("index-upload.token")).ok()?.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

/// Try the Cloudflare store for this patch's index. `None` on 404 / any error
/// (network down, worker down) -> caller builds locally.
fn fetch_cloud_index(game_dir: &Path) -> Option<Arc<GameIndex>> {
    let sig = patch_signature(game_dir);
    let allowed = crate::net::built_in_allowed_origins();
    let client = crate::net::build_external_client(15.0, allowed.clone());
    let url = cloud_index_url(&sig);
    let bytes = tauri::async_runtime::block_on(crate::net::get_bytes_checked(&client, &url, &allowed, 32 * 1024 * 1024)).ok()?;
    let idx = parse_index_wire(&bytes)?;
    log_info!("[MOD_SCOPE] Fetched cloud game index ({} entries) for patch {sig}", idx.counts.len());
    Some(Arc::new(idx))
}

/// Publish a freshly-built index so other clients on this patch skip the build.
/// No-op without the operator token. Only ever reached after a cloud fetch just
/// 404'd (a genuine miss), and the per-patch blob is immutable, so we publish
/// unconditionally — a redundant re-upload of identical bytes is harmless.
fn publish_cloud_index(game_dir: &Path, idx: &GameIndex) {
    let Some(token) = index_upload_token() else { return };
    let sig = patch_signature(game_dir);
    let allowed = crate::net::built_in_allowed_origins();
    let client = crate::net::build_external_client(60.0, allowed.clone());
    let url = cloud_index_url(&sig);
    let body = serialize_index_wire(idx);
    match tauri::async_runtime::block_on(crate::net::put_bytes_checked_authed(&client, &url, &allowed, body, &token)) {
        Ok(()) => log_info!("[MOD_SCOPE] Published game index for patch {sig} to cloud"),
        Err(e) => log_warn!("[MOD_SCOPE] Cloud index publish failed (harmless): {e}"),
    }
}

/// Cached `build_game_index`: memory hit -> disk hit (same patch) -> cloud fetch
/// (same patch, any machine) -> build once locally (and publish if operator).
fn build_game_index_cached(game_dir: &Path) -> Option<Arc<GameIndex>> {
    let fp = game_dir_fingerprint(game_dir);
    if let Some((k, idx)) = index_cache().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *k == fp {
            return Some(idx.clone());
        }
    }
    let (idx, from_disk) = match load_persisted_index(fp) {
        Some(idx) => (idx, true),
        None => {
            let idx = fetch_cloud_index(game_dir).or_else(|| {
                let arc = Arc::new(build_game_index(game_dir)?);
                publish_cloud_index(game_dir, &arc);
                Some(arc)
            })?;
            (idx, false)
        }
    };
    if !from_disk {
        persist_index(fp, &idx);
    }
    *index_cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((fp, idx.clone()));
    Some(idx)
}

/// Best-effort background pre-warm at startup so the ~11s first build never
/// lands inside an injection's suspend window. No-op if the game dir isn't
/// resolvable yet (League closed) — the champ-select sweep builds it then.
pub fn prewarm_game_index() {
    std::thread::spawn(|| {
        if let Some(dir) = crate::skins::lcu_ext::resolve_game_dir() {
            let _ = build_game_index_cached(&dir);
        }
    });
}

/// Path-hash for a mod entry: hex-named files carry their hash literally;
/// everything else hashes its lowercased relative path.
fn path_hash(rel: &str) -> u64 {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    if stem.len() == 16 && stem.chars().all(|c| c.is_ascii_hexdigit()) {
        if let Ok(h) = u64::from_str_radix(stem, 16) {
            return h;
        }
    }
    xxhash_rust::xxh64::xxh64(rel.replace('\\', "/").to_lowercase().as_bytes(), 0)
}

// ---------------------------------------------------------------------------
// Per-mod scoping
// ---------------------------------------------------------------------------

fn run_tool(exe: &Path, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut cmd = Command::new(exe);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().map_err(|e| format!("{}: {e}", exe.display()))?;
    if !out.status.success() {
        return Err(format!("{} exited {:?}: {}", exe.display(), out.status.code(), String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// Walk `dir`, deleting entries that fail the scoping rule.
/// Returns (kept, dropped-descriptions).
fn scope_dir(dir: &Path, index: &GameIndex) -> (u32, Vec<String>) {
    fn walk(dir: &Path, root: &Path, index: &GameIndex, kept: &mut u32, dropped: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, index, kept, dropped);
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if rel == "hashed_files.json" {
                continue;
            }
            let n = index.count(path_hash(&rel));
            if n <= MAX_ENTRY_WADS {
                *kept += 1;
            } else {
                dropped.push(format!("{rel} ({n} wads)"));
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let mut kept = 0;
    let mut dropped = Vec::new();
    walk(dir, dir, index, &mut kept, &mut dropped);
    (kept, dropped)
}

/// Scope one champion-skin fantome in place. Returns the number of dropped
/// shared entries (0 = mod was already clean and is left byte-identical).
fn scope_fantome(path: &Path, index: &GameIndex) -> Result<usize, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let tmp = std::env::temp_dir().join(format!("chud_scope_{}", std::process::id())).join(
        path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "mod".into()),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let cleanup = TempCleanup(tmp.clone());

    let staging = tmp.join("x");
    // Write to a temp file first — safe_extractall takes a path.
    let zip_copy = tmp.join("src.zip");
    std::fs::write(&zip_copy, &bytes).map_err(|e| e.to_string())?;
    safe_extractall(&zip_copy, &staging).map_err(|e| e.to_string())?;

    let wad_root = staging.join("WAD");
    if !wad_root.is_dir() {
        return Ok(0); // nothing recognizable — leave alone
    }

    let tools = cslol_tools_dir();
    let hashes_file = tools.join("hashes.game.txt");
    let mut total_dropped: Vec<String> = Vec::new();
    let mut packed_members: Vec<(String, PathBuf)> = Vec::new();

    let members: Vec<PathBuf> = std::fs::read_dir(&wad_root).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    for member_path in members {
        let member = member_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !member.to_lowercase().ends_with(".wad.client") {
            continue;
        }

        let filter_dir = if member_path.is_file() {
            // Packed member: unpack to filter its entries.
            let extracted = tmp.join("unpacked").join(&member);
            let mut args: Vec<&std::ffi::OsStr> = vec![member_path.as_os_str(), extracted.as_os_str()];
            if hashes_file.exists() {
                args.push(hashes_file.as_os_str());
            }
            run_tool(&tools.join("wad-extract.exe"), &args)?;
            extracted
        } else {
            member_path.clone()
        };

        let (kept, dropped) = scope_dir(&filter_dir, index);
        log_info!("[MOD_SCOPE] {member}: kept {kept}, dropped {}", dropped.len());
        let member_dropped = dropped.len();
        total_dropped.extend(dropped);

        // Repack from the filtered dir when anything was dropped or the
        // member was RAW — a packed member with zero drops keeps its file.
        if member_path.is_file() && member_dropped == 0 {
            packed_members.push((member, member_path));
        } else {
            let packed = tmp.join("packed").join(&member);
            std::fs::create_dir_all(packed.parent().unwrap()).map_err(|e| e.to_string())?;
            run_tool(&tools.join("wad-make.exe"), &[filter_dir.as_os_str(), packed.as_os_str()])?;
            packed_members.push((member, packed));
        }
    }

    if total_dropped.is_empty() {
        drop(cleanup);
        return Ok(0);
    }

    // Rewrap: everything outside WAD/ preserved, WAD members repacked.
    let bak = path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("fantome")
    ));
    if !bak.exists() {
        std::fs::copy(path, &bak).map_err(|e| e.to_string())?;
    }

    use std::io::Write as _;
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).map_err(|e| e.to_string())?;
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();
    for name in names {
        if name.starts_with("WAD/") || name.ends_with('/') {
            continue;
        }
        let mut data = Vec::new();
        use std::io::Read as _;
        archive.by_name(&name).map_err(|e| e.to_string())?.read_to_end(&mut data).map_err(|e| e.to_string())?;
        zw.start_file(name, opts).map_err(|e| e.to_string())?;
        zw.write_all(&data).map_err(|e| e.to_string())?;
    }
    for (member, packed_path) in packed_members {
        let data = std::fs::read(&packed_path).map_err(|e| e.to_string())?;
        zw.start_file(format!("WAD/{member}"), opts).map_err(|e| e.to_string())?;
        zw.write_all(&data).map_err(|e| e.to_string())?;
    }
    let rewritten = zw.finish().map_err(|e| e.to_string())?.into_inner();

    let tmp_out = path.with_extension("chud_new");
    std::fs::write(&tmp_out, &rewritten).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_out, path).map_err(|e| e.to_string())?;

    for d in total_dropped.iter().take(6) {
        log_info!("[MOD_SCOPE]   dropped: {d}");
    }
    drop(cleanup);
    Ok(total_dropped.len())
}

struct TempCleanup(PathBuf);
impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Sweep
// ---------------------------------------------------------------------------

fn is_mod_archive(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref(),
        Some("fantome") | Some("zip")
    )
}

fn collect_mod_archives(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_mod_archives(&path, out);
        } else if is_mod_archive(&path) {
            out.push(path);
        }
    }
}

/// Sweep user-imported mods: scope champion skins, retarget hand-dropped
/// announcer packs. Cheap when nothing changed (one metadata stat per mod).
/// Runs at app startup and on every ChampSelect entry; call from a blocking
/// thread. `app` (when present) gets a toast about what was fixed.
pub fn sweep_imported_mods(app: Option<&AppHandle>) {
    if SWEEP_RUNNING.swap(true, Ordering::SeqCst) {
        return; // a sweep is already in flight
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep_inner(app)));
    SWEEP_RUNNING.store(false, Ordering::SeqCst);
    if result.is_err() {
        log_error!("[MOD_SCOPE] Sweep panicked - state untouched, will retry next sweep");
    }
}

fn sweep_inner(app: Option<&AppHandle>) {
    let mods_root = paths::mods_dir();
    let mut skins = Vec::new();
    collect_mod_archives(&mods_root.join("skins"), &mut skins);
    let mut announcers = Vec::new();
    collect_mod_archives(&mods_root.join("announcers"), &mut announcers);

    let mut state = load_state();
    let pending: Vec<(PathBuf, bool)> = skins
        .into_iter()
        .map(|p| (p, false))
        .chain(announcers.into_iter().map(|p| (p, true)))
        .filter(|(p, _)| {
            let key = p.to_string_lossy().into_owned();
            match (stamp(p), state.processed.get(&key)) {
                (Some(s), Some(prev)) => s != *prev,
                (Some(_), None) => true,
                (None, _) => false,
            }
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    log_info!("[MOD_SCOPE] {} new/changed imported mod(s) to check", pending.len());

    // Champion skins need the game index; announcers don't.
    let needs_index = pending.iter().any(|(_, is_announcer)| !is_announcer);
    let index = if needs_index {
        match lcu_ext::resolve_game_dir() {
            Some(dir) => build_game_index_cached(&dir),
            None => {
                // Announcers below still process; skins stay pending for the
                // next sweep (ChampSelect entry guarantees the client is up).
                log_info!("[MOD_SCOPE] League game dir unavailable (client not running?) - skin scoping deferred");
                None
            }
        }
    } else {
        None
    };

    let mut fixed: Vec<String> = Vec::new();
    for (path, is_announcer) in &pending {
        let key = path.to_string_lossy().into_owned();
        let display = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| key.clone());

        if *is_announcer {
            match std::fs::read(path) {
                Ok(bytes) => {
                    if let Some(rewritten) = announcer_fix::retarget_announcer_pack(&bytes) {
                        let bak = path.with_extension(format!(
                            "{}.bak",
                            path.extension().and_then(|e| e.to_str()).unwrap_or("fantome")
                        ));
                        if !bak.exists() {
                            let _ = std::fs::copy(path, &bak);
                        }
                        if std::fs::write(path, &rewritten).is_ok() {
                            log_info!("[MOD_SCOPE] Retargeted announcer pack for all modes: {display}");
                            fixed.push(format!("{display} (announcer, all modes)"));
                        }
                    }
                }
                Err(e) => log_warn!("[MOD_SCOPE] Could not read {display}: {e}"),
            }
            if let Some(s) = stamp(path) {
                state.processed.insert(key, s);
            }
            continue;
        }

        let Some(index) = index.as_ref() else { continue }; // skins deferred without index
        match scope_fantome(path, index) {
            Ok(0) => {}
            Ok(dropped) => {
                log_info!("[MOD_SCOPE] Scoped '{display}': dropped {dropped} shared entries");
                fixed.push(format!("{display} ({dropped} shared entries removed)"));
            }
            Err(e) => log_error!("[MOD_SCOPE] Failed to scope '{display}' (left untouched): {e}"),
        }
        if let Some(s) = stamp(path) {
            state.processed.insert(key, s);
        }
    }

    save_state(&state);

    if !fixed.is_empty() {
        if let Some(app) = app {
            let _ = app.emit(
                "notification",
                serde_json::json!({
                    "title": "Custom mods optimized",
                    "message": format!("Fixed for fast injection: {}", fixed.join(", ")),
                    "tone": "success",
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_hash_uses_hex_stem_or_xxh64_of_lowercased_path() {
        assert_eq!(path_hash("12d723dc0e745f8a.bin"), 0x12d723dc0e745f8a);
        assert_eq!(path_hash("sub/dir/AbCdEf0123456789.dds"), 0xabcdef0123456789);
        // Known pair from hashes.game.txt: the en_us global announcer wpk.
        assert_eq!(
            path_hash("assets/sounds/wwise2016/vo/en_us/shared/announcer_global_female1_vo_audio.wpk"),
            0xccdbafd023095999
        );
        // Case-insensitive + separator-normalizing.
        assert_eq!(
            path_hash("Assets\\Sounds\\wwise2016\\vo\\en_us\\shared\\announcer_global_female1_vo_audio.wpk"),
            0xccdbafd023095999
        );
    }


    /// Full-pipeline proof against a real mod + real League install. Skips
    /// silently when either is absent (CI); run manually with
    /// `cargo test --lib mod_scope -- --ignored`.
    #[test]
    #[ignore = "requires a local League install and a .bak'd real mod"]
    fn scope_real_fantome_end_to_end() {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let bak = Path::new(&local).join("Chud/mods/skins/4000/Rouxls Kaard Twisted Fate.fantome.bak");
        let game = Path::new(r"C:\Riot Games\League of Legends\Game");
        if !bak.exists() || !game.is_dir() {
            return;
        }
        let Some(index) = build_game_index(game) else { return };

        let work = std::env::temp_dir().join("chud_mod_scope_e2e");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("Rouxls.fantome");
        std::fs::copy(&bak, &target).unwrap();

        let dropped = scope_fantome(&target, &index).expect("scoping must succeed");
        assert!(dropped >= 20, "expected the known ~24 shared entries, got {dropped}");

        // Result must be a valid zip whose WAD member is now a packed file.
        let bytes = std::fs::read(&target).unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())).unwrap();
        let names: Vec<String> = z.file_names().map(str::to_string).collect();
        assert!(names.iter().any(|n| n == "WAD/TwistedFate.wad.client"), "packed member missing: {names:?}");
        assert!(names.iter().any(|n| n.starts_with("META/")), "META lost");
        use std::io::Read as _;
        let mut wad = Vec::new();
        z.by_name("WAD/TwistedFate.wad.client").unwrap().read_to_end(&mut wad).unwrap();
        assert_eq!(&wad[..2], b"RW", "member must be a packed WAD");

        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn scope_dir_keeps_new_own_and_cross_champion_entries_drops_shared() {
        let root = std::env::temp_dir().join("chud_mod_scope_test_dir");
        let _ = std::fs::remove_dir_all(&root);
        for rel in [
            "assets/characters/jhin/skins/base/jhin.skn",   // own champion (2 wads)
            "assets/characters/akali/skins/base/akali.skn", // cross-champ in a multi-champ pack (1 wad)
            "assets/shared/particles/defaultfalloff.tex",   // shared junk (226 wads)
            "newfile.custom",                               // brand-new (0 wads)
        ] {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        let mut counts = HashMap::new();
        counts.insert(path_hash("assets/characters/jhin/skins/base/jhin.skn"), 2u32);
        counts.insert(path_hash("assets/characters/akali/skins/base/akali.skn"), 1u32);
        counts.insert(path_hash("assets/shared/particles/defaultfalloff.tex"), 226u32);
        let idx = GameIndex { counts };

        let (kept, dropped) = scope_dir(&root, &idx);
        assert_eq!(kept, 3, "own + cross-champion + brand-new entries all survive");
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].contains("defaultfalloff.tex"));
        assert!(root.join("assets/characters/akali/skins/base/akali.skn").exists(), "v1 regression: multi-champ pack content must survive");
        assert!(!root.join("assets/shared/particles/defaultfalloff.tex").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
