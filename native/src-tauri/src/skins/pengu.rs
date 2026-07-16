//! Pengu Loader lifecycle management — ported from
//! `utils\integration\pengu_loader.py`. Copies the bundled Pengu Loader
//! payload into the runtime dir (`paths::pengu_loader_dir`), preserving the
//! user's `datastore` and per-plugin enable/disable choices across every
//! sync, then drives its CLI (`--set-league-path`, `--force-activate`/
//! `--force-deactivate`, `--restart-client`).
//!
//! Python's `_resolve_pengu_dir` branched on PyInstaller frozen/dev mode;
//! Tauri always runs from a real executable, so that collapses to one
//! "copy bundled -> writable runtime dir" path every time.

#![allow(dead_code)] // consumed by S5+ (game-flow wiring calls into this)

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use sysinfo::{ProcessesToUpdate, System};

use crate::skins::injection::tools;
use crate::skins::paths;
use crate::skins::slog::{log_error, log_info, log_warn};

const PLUGIN_ENTRYPOINT: &str = "index.js";
const PLUGIN_ENTRYPOINT_DISABLED: &str = "index.js_";
const PLUGIN_ENTRYPOINT_BUNDLED_BACKUP: &str = "index.js.bundled";

/// Client-side process names Pengu Loader interacts with (ported from
/// `_LEAGUE_PROCESSES`).
const LEAGUE_PROCESSES: [&str; 4] =
    ["LeagueClient.exe", "LeagueClientUx.exe", "LeagueClientUxRender.exe", "League of Legends.exe"];
const PENGU_UI_PROCESS: &str = "Pengu Loader.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resolved once per process — every call after the first reuses the
/// already-synced runtime directory instead of re-copying the whole payload.
static PENGU_DIR: OnceLock<PathBuf> = OnceLock::new();

fn pengu_dir() -> &'static Path {
    PENGU_DIR.get_or_init(resolve_pengu_dir)
}

/// Force the bundled->runtime Pengu sync on app startup, so newly-shipped
/// plugins reach the runtime plugins folder even without re-activating
/// Pengu. Idempotent (OnceLock) and preserves datastore + plugin enable/disable state.
pub fn ensure_synced() {
    let _ = pengu_dir();
}

fn pengu_exe(dir: &Path) -> PathBuf {
    dir.join("Pengu Loader.exe")
}

fn is_available(dir: &Path) -> bool {
    pengu_exe(dir).exists()
}

fn hash_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

/// H6: the runtime `Pengu Loader.exe` lives in a per-user-writable dir but is
/// executed with the app's (often ELEVATED) token, so a standard-user attacker
/// who overwrites it would get their binary run as admin. Before executing,
/// verify it byte-matches the read-only BUNDLED copy (admin-writable, trusted);
/// on mismatch, restore the trusted binary and re-verify. Comparing against the
/// bundle (not a pinned hash) auto-adapts to a Pengu version shipped in an app
/// update. Returns false only if the trusted binary can't be established.
fn ensure_pengu_exe_trusted(runtime_dir: &Path) -> bool {
    let runtime_exe = pengu_exe(runtime_dir);
    let bundled_exe = pengu_exe(&tools::pengu_loader_resource_dir());
    let Some(bundled_hash) = hash_file(&bundled_exe) else {
        // No bundled reference (e.g. a dev build without packaged resources) —
        // can't verify against nothing; fall back to existence rather than
        // blocking a legitimate run.
        return runtime_exe.exists();
    };
    if hash_file(&runtime_exe).as_deref() == Some(bundled_hash.as_str()) {
        return true;
    }
    log_warn!("[PENGU] Runtime Pengu Loader.exe does not match the bundled copy — restoring the trusted binary before executing.");
    if std::fs::copy(&bundled_exe, &runtime_exe).is_err() {
        return false;
    }
    hash_file(&runtime_exe).as_deref() == Some(bundled_hash.as_str())
}

fn active_flag_path() -> PathBuf {
    paths::state_dir().join("pengu_active.flag")
}

// ---------------------------------------------------------------------------
// Plugin enable/disable state preservation
// ---------------------------------------------------------------------------

/// Ensure plugin enable/disable state survives the overlay sync.
///
/// Disabling a plugin renames `index.js` -> `index.js_`. The sync below
/// overlays bundled files onto the runtime dir without deleting extras, so a
/// disabled plugin can end up with BOTH files — effectively re-enabling it.
/// Rule: if `index.js_` exists, treat it as authoritative and park/remove
/// any reintroduced `index.js`.
fn sanitize_plugin_entrypoints(pengu_dir: &Path) {
    let plugins_dir = pengu_dir.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else { return };

    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }

        let enabled = plugin_dir.join(PLUGIN_ENTRYPOINT);
        let disabled = plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED);
        if !disabled.exists() || !enabled.exists() {
            continue;
        }

        let backup = plugin_dir.join(PLUGIN_ENTRYPOINT_BUNDLED_BACKUP);
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        match std::fs::rename(&enabled, &backup) {
            Ok(()) => {
                log_info!("[PENGU] Preserved disabled plugin state by parking {} to {}", enabled.display(), backup.display())
            }
            Err(_) => {
                // Couldn't park it (locked/permission) — at least try to delete it.
                if std::fs::remove_file(&enabled).is_ok() {
                    log_info!("[PENGU] Removed reintroduced entrypoint for disabled plugin: {}", enabled.display());
                }
            }
        }
    }
}

/// Snapshot the user's enabled/disabled state for plugins before the
/// overlay sync. "enabled" = `index.js` exists and `index.js_` doesn't;
/// "disabled" = `index.js_` exists (ported from `_snapshot_plugin_enable_state`).
fn snapshot_plugin_enable_state(pengu_dir: &Path) -> (HashSet<String>, HashSet<String>) {
    let mut enabled = HashSet::new();
    let mut disabled = HashSet::new();

    let plugins_dir = pengu_dir.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else { return (enabled, disabled) };

    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        let Some(name) = plugin_dir.file_name().and_then(|n| n.to_str()) else { continue };

        if plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED).exists() {
            disabled.insert(name.to_string());
        } else if plugin_dir.join(PLUGIN_ENTRYPOINT).exists() {
            enabled.insert(name.to_string());
        }
    }

    (enabled, disabled)
}

/// After the overlay sync, restore the user's prior plugin enable/disable
/// choices. Only `enabled` is actually consulted — disabled-state
/// restoration is handled by `sanitize_plugin_entrypoints` below instead.
fn restore_plugin_enable_state(pengu_dir: &Path, enabled: &HashSet<String>, _disabled: &HashSet<String>) {
    let plugins_dir = pengu_dir.join("plugins");

    // If the user had a plugin enabled, prefer enabled state: remove any
    // reintroduced `index.js_` from the bundle.
    for plugin_name in enabled {
        let plugin_dir = plugins_dir.join(plugin_name);
        if !plugin_dir.is_dir() {
            continue;
        }
        let enabled_entry = plugin_dir.join(PLUGIN_ENTRYPOINT);
        let disabled_entry = plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED);
        if enabled_entry.exists() && disabled_entry.exists() && std::fs::remove_file(&disabled_entry).is_ok() {
            log_info!(
                "[PENGU] Preserved enabled plugin state by removing bundled disabled entrypoint: {}",
                disabled_entry.display()
            );
        }
    }

    // If the user had a plugin disabled, keep disabled state authoritative
    // (also handles the "both files exist" case by parking `index.js`).
    sanitize_plugin_entrypoints(pengu_dir);
}

// ---------------------------------------------------------------------------
// Runtime directory resolution (ported from `_get_bundled_pengu_dir` /
// `_resolve_pengu_dir`)
// ---------------------------------------------------------------------------

fn copy_tree_preserving_datastore(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "datastore" {
            continue; // preserved separately below — never overwritten by the sync
        }
        let target = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree_preserving_datastore(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }

    // Seed the datastore once for a brand-new runtime directory. Pengu
    // Loader stores plugin/user settings there; overwriting it on every
    // app update would wipe user preferences.
    let bundled_datastore = src.join("datastore");
    let runtime_datastore = dst.join("datastore");
    if !runtime_datastore.exists() && bundled_datastore.exists() {
        let _ = std::fs::copy(&bundled_datastore, &runtime_datastore);
    }

    Ok(())
}

/// Copy the bundled Pengu Loader payload into the writable runtime
/// directory, preserving `datastore` and plugin enable/disable choices
/// across the sync.
fn resolve_pengu_dir() -> PathBuf {
    let bundled_dir = tools::pengu_loader_resource_dir();
    let runtime_dir = paths::pengu_loader_dir();

    if let Err(e) = std::fs::create_dir_all(&runtime_dir) {
        log_warn!("[PENGU] Failed to create Pengu Loader runtime directory: {e}");
        return bundled_dir;
    }

    if !bundled_dir.is_dir() {
        log_warn!("[PENGU] Bundled Pengu Loader directory not found: {}", bundled_dir.display());
        return runtime_dir;
    }

    // Snapshot plugin enabled/disabled state BEFORE overlaying bundled files.
    let (enabled_plugins, disabled_plugins) = snapshot_plugin_enable_state(&runtime_dir);

    if let Err(e) = copy_tree_preserving_datastore(&bundled_dir, &runtime_dir) {
        log_warn!("[PENGU] Failed to copy Pengu Loader to runtime directory: {e}");
        return bundled_dir;
    }
    log_info!("[PENGU] Synced Pengu Loader to runtime directory (preserving user files): {}", runtime_dir.display());

    // Restore plugin enable/disable state after the overlay sync.
    restore_plugin_enable_state(&runtime_dir, &enabled_plugins, &disabled_plugins);

    runtime_dir
}

// ---------------------------------------------------------------------------
// Process helpers (ported from `_is_league_running` / `_terminate_pengu_ui` /
// `_run_cli`)
// ---------------------------------------------------------------------------

fn is_league_running() -> bool {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().any(|p| {
        let name = p.name().to_string_lossy();
        LEAGUE_PROCESSES.iter().any(|league_name| name.eq_ignore_ascii_case(league_name))
    })
}

fn terminate_pengu_ui() {
    match Command::new("taskkill").args(["/IM", PENGU_UI_PROCESS, "/F"]).creation_flags(CREATE_NO_WINDOW).output() {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            if !matches!(code, 0 | 128 | 255) {
                log_info!("[PENGU] taskkill for Pengu UI returned {code}");
            }
        }
        Err(e) => log_info!("[PENGU] Failed to terminate Pengu Loader UI process: {e}"),
    }
}

/// Execute the Pengu Loader CLI with the given arguments (ported from
/// `_run_cli`). Returns `true` when the command exits with one of `ok_codes`.
fn run_cli(dir: &Path, args: &[&str], ok_codes: &[i32]) -> bool {
    if !is_available(dir) {
        log_info!("[PENGU] Pengu Loader executable not found; skipping command {args:?}");
        return false;
    }
    if !ensure_pengu_exe_trusted(dir) {
        log_error!("[PENGU] Pengu Loader.exe failed integrity verification and could not be restored — refusing to execute {args:?}");
        return false;
    }

    let exe = pengu_exe(dir);
    match Command::new(&exe).args(args).current_dir(dir).creation_flags(CREATE_NO_WINDOW).output() {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            if !ok_codes.contains(&code) {
                log_warn!(
                    "[PENGU] Pengu Loader CLI command {} exited with code {code} (expected {ok_codes:?})",
                    args.join(" ")
                );
                return false;
            }
            true
        }
        Err(e) => {
            log_warn!("[PENGU] Failed to launch Pengu Loader CLI {}: {e}", exe.display());
            false
        }
    }
}

fn write_active_flag() {
    let path = active_flag_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, "active").is_ok() {
        log_info!("[PENGU] Pengu active flag written: {}", path.display());
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set the League path in Pengu Loader configuration (ported from
/// `set_league_path`).
pub fn set_league_path(league_path: &str) -> bool {
    let dir = pengu_dir();
    if !is_available(dir) {
        log_info!("[PENGU] Pengu Loader not available; skipping set-league-path.");
        return false;
    }
    let trimmed = league_path.trim();
    if trimmed.is_empty() {
        log_warn!("[PENGU] Empty league path provided; skipping set-league-path.");
        return false;
    }

    log_info!("[PENGU] Setting League path in Pengu Loader: {trimmed}");
    run_cli(dir, &["--set-league-path", trimmed, "--silent"], &[0])
}

/// Outcome of `activate_on_start`, so the caller can tell the user whether
/// League actually rebooted (Pengu's hook only loads on restart, which hits
/// the LCU and can fail if it's unreachable).
pub struct ActivateResult {
    /// Pengu's `--force-activate` succeeded (the hook is in place on disk).
    pub activated: bool,
    /// League was running, so a client restart was needed to load the hook.
    pub restart_needed: bool,
    /// The client restart actually went through. When `restart_needed` is true
    /// but this is false, the user must restart League manually to load Pengu.
    pub restarted: bool,
}

/// Force-activate Pengu Loader (ported from `activate_on_start`). Places the
/// hook, then restarts the League client so it loads with it — reporting
/// whether that restart succeeded so the UI can fall back to "restart manually".
pub fn activate_on_start(league_path: Option<&str>) -> ActivateResult {
    let dir = pengu_dir();
    if !is_available(dir) {
        log_info!("[PENGU] Pengu Loader not available; skipping activation.");
        return ActivateResult { activated: false, restart_needed: false, restarted: false };
    }

    if let Some(path) = league_path {
        if !set_league_path(path) {
            log_warn!("[PENGU] Failed to set league path in Pengu Loader, continuing with activation anyway.");
        }
    }

    terminate_pengu_ui();
    let restart_needed = is_league_running();

    log_info!("[PENGU] Activating Pengu Loader (restart League client: {restart_needed}).");

    // Write flag *before* activation so it persists even if the process is
    // killed mid-activation or the CLI returns an unexpected exit code.
    write_active_flag();

    let activated = run_cli(dir, &["--force-activate", "--silent"], &[0]);
    let mut restarted = false;
    if activated && restart_needed {
        restarted = run_cli(dir, &["--restart-client", "--silent"], &[0]);
        if !restarted {
            log_warn!(
                "[PENGU] Client restart failed (LCU unreachable) — Pengu is activated but the user must restart League manually to load it."
            );
        }
    }
    ActivateResult { activated, restart_needed, restarted }
}

/// Whether Pengu Loader is currently marked active — the flag file
/// `activate_on_start` writes still exists.
pub fn is_active() -> bool {
    active_flag_path().exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_plugin_entrypoints_parks_reintroduced_index_js() {
        let root = std::env::temp_dir().join("chud_pengu_test_sanitize");
        let _ = std::fs::remove_dir_all(&root);
        let plugin_dir = root.join("plugins").join("CHUD-Test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join(PLUGIN_ENTRYPOINT), "enabled").unwrap();
        std::fs::write(plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED), "disabled").unwrap();

        sanitize_plugin_entrypoints(&root);

        assert!(!plugin_dir.join(PLUGIN_ENTRYPOINT).exists());
        assert!(plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED).exists());
        assert!(plugin_dir.join(PLUGIN_ENTRYPOINT_BUNDLED_BACKUP).exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshot_then_restore_keeps_user_enabled_plugin_enabled() {
        let root = std::env::temp_dir().join("chud_pengu_test_restore");
        let _ = std::fs::remove_dir_all(&root);
        let plugin_dir = root.join("plugins").join("CHUD-Test");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        // User has it enabled locally.
        std::fs::write(plugin_dir.join(PLUGIN_ENTRYPOINT), "enabled").unwrap();

        let (enabled, disabled) = snapshot_plugin_enable_state(&root);
        assert!(enabled.contains("CHUD-Test"));

        // Simulate the bundle re-introducing a disabled marker during sync.
        std::fs::write(plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED), "disabled").unwrap();

        restore_plugin_enable_state(&root, &enabled, &disabled);

        assert!(plugin_dir.join(PLUGIN_ENTRYPOINT).exists());
        assert!(!plugin_dir.join(PLUGIN_ENTRYPOINT_DISABLED).exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
