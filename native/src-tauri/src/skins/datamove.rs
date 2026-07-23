//! Data-root migration primitives for the user-relocatable data folder
//! (Phase 1: `paths::data_root()`'s tree only). `lib.rs`'s
//! `relocate_data_root`/`reset_data_root` own the copy -> verify -> write-
//! pointer ordering and the safety guards; this module is just the
//! mechanical size/copy/verify parts, kept free of any `AppState`/Tauri
//! dependency so it's plain-`std` testable.

use std::path::Path;

use crate::skins::slog::{log_info, log_warn};

/// Recursive sum of file sizes under `root`. Best-effort: a per-entry error
/// (permission, a concurrent writer) is skipped rather than failing the
/// whole walk — this feeds a progress total / free-space estimate, not an
/// exact accounting.
pub fn dir_size(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    let mut total = 0u64;
    for entry in entries.flatten() {
        total += match entry.file_type() {
            Ok(ft) if ft.is_dir() => dir_size(&entry.path()),
            Ok(_) => entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        };
    }
    total
}

/// Top-level directories we do NOT migrate: `logs` is transient debug output
/// that's held open and appended to continuously — including by this very copy
/// operation — so copying it risks a Windows sharing violation and makes a
/// byte-for-byte `verify_copy` impossible (the source grows mid-copy). The new
/// location simply starts a fresh log.
const MIGRATE_SKIP: &[&str] = &["logs"];

fn skipped(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| MIGRATE_SKIP.iter().any(|s| n.eq_ignore_ascii_case(s)))
}

/// Total bytes of everything EXCEPT the skipped top-level dirs — the real
/// amount `copy_tree` moves (keeps progress + verification consistent).
fn migratable_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    entries
        .flatten()
        .filter(|e| !skipped(&e.file_name()))
        .map(|e| match e.file_type() {
            Ok(ft) if ft.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Recursive copy of `src` into `dst` (mirrors `injection::zips::copy_dir_recursive`),
/// reporting `(bytes_copied_so_far, total_bytes)` through `on_progress` after
/// each file. Skips `MIGRATE_SKIP` top-level dirs. The first I/O error aborts;
/// the partial `dst` left behind is the caller's to verify or discard, never
/// trusted as a complete copy on its own.
pub fn copy_tree(src: &Path, dst: &Path, on_progress: &mut dyn FnMut(u64, u64)) -> std::io::Result<()> {
    let total = migratable_bytes(src);
    log_info!("[datamove] Copying {} -> {} ({total} bytes, excluding {MIGRATE_SKIP:?})", src.display(), dst.display());
    let mut copied = 0u64;
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            if skipped(&entry.file_name()) {
                continue;
            }
            let target = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree_inner(&entry.path(), &target, total, &mut copied, on_progress)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
                copied += entry.metadata().map(|m| m.len()).unwrap_or(0);
                on_progress(copied, total);
            }
        }
        Ok(())
    })();
    match &result {
        Ok(()) => log_info!("[datamove] Copy complete: {copied} bytes"),
        Err(e) => log_warn!("[datamove] Copy aborted after {copied}/{total} bytes: {e}"),
    }
    result
}

fn copy_tree_inner(
    src: &Path,
    dst: &Path,
    total: u64,
    copied: &mut u64,
    on_progress: &mut dyn FnMut(u64, u64),
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree_inner(&entry.path(), &target, total, copied, on_progress)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
            *copied += entry.metadata().map(|m| m.len()).unwrap_or(0);
            on_progress(*copied, total);
        }
    }
    Ok(())
}

/// Cheap post-copy check: file COUNT and total bytes match between `src` and
/// `dst`. Not a per-file hash comparison (that's Phase 2's manifest
/// verification) — good enough to catch a truncated/aborted copy before the
/// pointer file gets written.
pub fn verify_copy(src: &Path, dst: &Path) -> Result<(), String> {
    // Source excludes the skipped top-level dirs (logs) since they weren't
    // copied; the destination has none of them, so a full count matches.
    let (src_count, src_bytes) = migratable_count_and_size(src);
    let (dst_count, dst_bytes) = count_and_size(dst);
    if src_count != dst_count || src_bytes != dst_bytes {
        return Err(format!(
            "Copy verification failed: source has {src_count} file(s)/{src_bytes} bytes, destination has {dst_count} file(s)/{dst_bytes} bytes"
        ));
    }
    Ok(())
}

fn count_and_size(root: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(root) else { return (0, 0) };
    let mut count = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                let (c, b) = count_and_size(&entry.path());
                count += c;
                bytes += b;
            }
            Ok(_) => {
                count += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            Err(_) => {}
        }
    }
    (count, bytes)
}

/// `count_and_size` excluding the skipped top-level dirs — the source side of
/// `verify_copy`, matching exactly what `copy_tree` moved.
fn migratable_count_and_size(root: &Path) -> (u64, u64) {
    let Ok(entries) = std::fs::read_dir(root) else { return (0, 0) };
    let mut count = 0u64;
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        if skipped(&entry.file_name()) {
            continue;
        }
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => {
                let (c, b) = count_and_size(&entry.path());
                count += c;
                bytes += b;
            }
            Ok(_) => {
                count += 1;
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
            Err(_) => {}
        }
    }
    (count, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chud_datamove_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn dir_size_sums_nested_files() {
        let root = temp_dir("dir_size");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"12345").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"1234567890").unwrap();

        assert_eq!(dir_size(&root), 15);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copy_tree_then_verify_round_trips() {
        let src = temp_dir("copy_src");
        let dst = temp_dir("copy_dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), b"world!").unwrap();

        let mut calls = Vec::new();
        let mut on_progress = |done, total| calls.push((done, total));
        copy_tree(&src, &dst, &mut on_progress).unwrap();

        assert!(dst.join("a.txt").is_file());
        assert!(dst.join("sub").join("b.txt").is_file());
        assert!(!calls.is_empty());
        assert_eq!(verify_copy(&src, &dst), Ok(()));

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dst).unwrap();
    }

    #[test]
    fn copy_tree_skips_logs_and_verify_still_passes() {
        let src = temp_dir("skip_src");
        let dst = temp_dir("skip_dst");
        std::fs::create_dir_all(src.join("mods")).unwrap();
        std::fs::create_dir_all(src.join("logs")).unwrap();
        std::fs::write(src.join("mods").join("m.fantome"), b"modbytes").unwrap();
        std::fs::write(src.join("logs").join("run.log"), b"log-noise-that-would-grow").unwrap();

        let mut on_progress = |_d, _t| {};
        copy_tree(&src, &dst, &mut on_progress).unwrap();

        assert!(dst.join("mods").join("m.fantome").is_file());
        assert!(!dst.join("logs").exists(), "logs must not be migrated");
        // Verify passes even though src has a logs/ dir the dst doesn't.
        assert_eq!(verify_copy(&src, &dst), Ok(()));

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dst).unwrap();
    }

    #[test]
    fn verify_copy_detects_missing_file() {
        let src = temp_dir("verify_src");
        let dst = temp_dir("verify_dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        // dst left empty — simulates an aborted copy.

        assert!(verify_copy(&src, &dst).is_err());

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dst).unwrap();
    }
}
