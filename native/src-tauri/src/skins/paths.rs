//! `%LOCALAPPDATA%\Chud` data-tree paths, plus elevation-aware desktop-user
//! resolution (ported from `utils\core\paths.py`).
//!
//! The elevation dance matters because the Tauri webview/overlay run
//! unelevated, as the interactive desktop user, while Chud's own process may
//! be elevated ("Run as administrator", needed for injection). Naively
//! reading `%LOCALAPPDATA%` from an elevated process would resolve the admin
//! account's directory and the two halves of the app would write to
//! different trees. Instead we find the desktop user via explorer.exe's
//! token and use that account's `AppData\Local`.

#![allow(dead_code)] // consumed by S2+

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::skins::slog::log_info;

/// Cached desktop-user `AppData\Local` path (None = no mismatch detected, or
/// detection failed — caller falls back to this process's own env vars).
static DESKTOP_LOCALAPPDATA: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Cached resolved data root (default or user-relocated) — read once per
/// process. A relocation via `write_pointer`/`remove_pointer` only takes
/// effect on the next launch; see `relocate_data_root`/`reset_data_root`.
static RESOLVED_DATA_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Root of Chud's writable data tree — the default `%LOCALAPPDATA%\Chud`,
/// unless the user relocated it (see `pointer_file_path`).
pub fn data_root() -> PathBuf {
    RESOLVED_DATA_ROOT.get_or_init(resolve_data_root).clone()
}

/// The fixed, never-relocated default data root.
pub fn default_data_root() -> PathBuf {
    local_appdata().join("Chud")
}

/// Read `pointer_file_path()` and use it as the data root if it points at an
/// absolute path that exists as a directory (or can be created) — else fall
/// back to the default.
fn resolve_data_root() -> PathBuf {
    let default = default_data_root();
    let Some(custom) = read_pointer() else {
        log_info!("[paths] Data root: {} (default)", default.display());
        return default;
    };

    if pointer_target_usable(&custom) {
        log_info!("[paths] Data root: {} (custom, via {})", custom.display(), pointer_file_path().display());
        custom
    } else {
        log_info!("[paths] Data root: {} (default — pointer target {} unusable)", default.display(), custom.display());
        default
    }
}

/// A pointer target is usable if it's already a directory, or we can create it.
fn pointer_target_usable(candidate: &Path) -> bool {
    candidate.is_dir() || std::fs::create_dir_all(candidate).is_ok()
}

/// The pointer file's location — always the DEFAULT `%LOCALAPPDATA%\Chud`
/// (elevation-resolved via `local_appdata()`, never the raw `LOCALAPPDATA` env
/// var). This must stay fixed regardless of any relocation, or an elevated
/// injection launch and the unelevated webview would read two different
/// pointers and diverge onto different data trees.
pub fn pointer_file_path() -> PathBuf {
    local_appdata().join("Chud").join("dataroot.txt")
}

/// Read the pointer file into an absolute path. Existence/dir checks happen
/// in `resolve_data_root` — this just locates the file on disk.
fn read_pointer() -> Option<PathBuf> {
    let text = std::fs::read_to_string(pointer_file_path()).ok()?;
    parse_pointer_text(&text)
}

/// Parse pointer-file contents into an absolute path. Split out from
/// `read_pointer` so it's unit-testable without touching the real
/// `%LOCALAPPDATA%\Chud\dataroot.txt` (see tests below).
fn parse_pointer_text(text: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(text.trim());
    candidate.is_absolute().then_some(candidate)
}

/// Point `data_root()` at `custom_root` from the next launch onward. Callers
/// must have already copied and verified the data at `custom_root` — this
/// function only flips the pointer.
pub fn write_pointer(custom_root: &Path) -> std::io::Result<()> {
    let pointer_dir = local_appdata().join("Chud");
    std::fs::create_dir_all(&pointer_dir)?;
    std::fs::write(pointer_file_path(), custom_root.to_string_lossy().as_bytes())
}

/// Remove the pointer file, reverting `data_root()` to the default from the
/// next launch onward. Not-found is not an error (already at default).
pub fn remove_pointer() -> std::io::Result<()> {
    match std::fs::remove_file(pointer_file_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn skins_dir() -> PathBuf {
    data_root().join("skins")
}

pub fn state_dir() -> PathBuf {
    data_root().join("state")
}

pub fn injection_dir() -> PathBuf {
    data_root().join("injection")
}

pub fn injection_mods_dir() -> PathBuf {
    injection_dir().join("mods")
}

pub fn injection_overlay_dir() -> PathBuf {
    injection_dir().join("overlay")
}

pub fn injection_extract_cache_dir() -> PathBuf {
    injection_dir().join(".extract_cache")
}

pub fn mods_dir() -> PathBuf {
    data_root().join("mods")
}

pub fn resources_dir() -> PathBuf {
    data_root().join("resources")
}

pub fn logs_dir() -> PathBuf {
    data_root().join("logs")
}

/// Create the full data-dir tree. Best-effort per directory: the first
/// failure (e.g. a locked-down profile) aborts and surfaces to the caller,
/// which logs it non-fatally (see `skins::init`).
pub fn ensure_tree() -> std::io::Result<()> {
    for dir in [
        skins_dir(),
        state_dir(),
        injection_mods_dir(),
        injection_overlay_dir(),
        injection_extract_cache_dir(),
        mods_dir(),
        resources_dir(),
        logs_dir(),
    ] {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

/// Resolve `%LOCALAPPDATA%`, preferring the desktop user's profile over this
/// process's own environment when elevation caused a mismatch.
fn local_appdata() -> PathBuf {
    if let Some(desktop) = desktop_local_appdata() {
        return desktop;
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Local"))
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

#[cfg(windows)]
fn desktop_local_appdata() -> Option<PathBuf> {
    DESKTOP_LOCALAPPDATA.get_or_init(desktop_local_appdata_uncached).clone()
}

#[cfg(not(windows))]
fn desktop_local_appdata() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn desktop_local_appdata_uncached() -> Option<PathBuf> {
    let (desktop_user, profile_dir) = desktop_user_info()?;
    let current_user = std::env::var("USERNAME").unwrap_or_default();
    if desktop_user.eq_ignore_ascii_case(&current_user) {
        return None; // no mismatch — use this process's own env vars
    }
    let candidate = PathBuf::from(profile_dir).join("AppData").join("Local");
    candidate.is_dir().then_some(candidate)
}

/// Find the desktop user's username + profile directory via explorer.exe's
/// process token (always the interactive user, even when this process is
/// elevated as a different account). `None` on any failure — callers fall back.
#[cfg(windows)]
fn desktop_user_info() -> Option<(String, String)> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, TokenUser, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_INFORMATION};
    use windows::Win32::UI::Shell::GetUserProfileDirectoryW;
    use windows::core::{PCWSTR, PWSTR};

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let pid = explorer_pid()?;

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid).ok()?;
        let _process_guard = HandleGuard(process);

        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;
        let _token_guard = HandleGuard(token);

        // First call sizes the buffer (expected to "fail" with the required length).
        let mut needed: u32 = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), needed, &mut needed)
            .ok()?;
        let token_user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let sid = token_user.User.Sid;

        let mut name = [0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain = [0u16; 256];
        let mut domain_len = domain.len() as u32;
        let mut sid_type = SID_NAME_USE(0);
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            PWSTR(name.as_mut_ptr()),
            &mut name_len,
            PWSTR(domain.as_mut_ptr()),
            &mut domain_len,
            &mut sid_type,
        )
        .ok()?;
        let username = String::from_utf16_lossy(&name[..name_len as usize]);

        let mut profile = [0u16; 260];
        let mut profile_len = profile.len() as u32;
        let profile_dir = if GetUserProfileDirectoryW(
            token,
            PWSTR(profile.as_mut_ptr()),
            &mut profile_len,
        )
        .is_ok()
        {
            let end = profile.iter().position(|&c| c == 0).unwrap_or(profile.len());
            String::from_utf16_lossy(&profile[..end])
        } else {
            format!("C:\\Users\\{username}")
        };

        Some((username, profile_dir))
    }
}

/// Locate explorer.exe's PID (always the interactive desktop user).
#[cfg(windows)]
fn explorer_pid() -> Option<u32> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes()
        .values()
        .find(|p| p.name().to_string_lossy().eq_ignore_ascii_case("explorer.exe"))
        .map(|p| p.pid().as_u32())
}

/// Base assets directory: exe-relative `resources/assets`, with a dev-mode
/// fallback to the source tree so `cargo run` works without a bundled build.
fn assets_dir() -> PathBuf {
    let exe_candidate = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("resources").join("assets")));
    let dev_candidate =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("assets");
    let cwd_candidate =
        std::env::current_dir().unwrap_or_default().join("resources").join("assets");

    [exe_candidate.clone(), Some(dev_candidate), Some(cwd_candidate)]
        .into_iter()
        .flatten()
        .find(|p| p.is_dir())
        .or(exe_candidate)
        .unwrap_or_else(|| PathBuf::from("resources/assets"))
}

/// Resolve an asset by relative name. Defense-in-depth against path
/// traversal: rejects absolute paths, drive letters, and `.`/`..`/empty
/// components lexically, then requires the resolved path to still live
/// under the assets dir even after symlink/junction resolution. Returns a
/// guaranteed-missing path for invalid input rather than `Option` (callers
/// already treat "doesn't exist" as not-found).
pub fn get_asset_path(name: &str) -> PathBuf {
    let assets = assets_dir();
    let invalid = assets.join("__invalid_asset_path__");

    let cleaned = name.replace('\\', "/");
    let cleaned = cleaned.trim_start_matches('/');
    if cleaned.is_empty() || cleaned.contains(':') {
        return invalid;
    }

    let candidate = Path::new(cleaned);
    let has_bad_component = candidate.components().any(|c| {
        matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)
            || c.as_os_str().is_empty()
    });
    if candidate.is_absolute() || candidate.has_root() || has_bad_component {
        return invalid;
    }

    let asset_path = assets.join(candidate);
    match asset_path.canonicalize() {
        Ok(resolved) => {
            let assets_resolved = assets.canonicalize().unwrap_or_else(|_| assets.clone());
            if resolved.starts_with(&assets_resolved) {
                asset_path
            } else {
                invalid
            }
        }
        // Doesn't exist yet (e.g. not-yet-downloaded asset) — the lexical
        // component check above already ruled out traversal.
        Err(_) => asset_path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `resolve_data_root`/`data_root` cache into a process-global `OnceLock`
    // and `read_pointer` always targets the real `%LOCALAPPDATA%\Chud\dataroot.txt`,
    // so exercising the full resolution here would be either a no-op (once
    // cached) or would touch the real user profile. Test the pure pieces
    // (`parse_pointer_text`, `pointer_target_usable`) instead — together they
    // cover both branches `resolve_data_root` takes.

    #[test]
    fn parse_pointer_text_no_pointer_falls_back_to_default() {
        // Empty/whitespace content is what a missing-or-blank pointer file
        // parses to — same "no pointer" branch `resolve_data_root` takes.
        assert_eq!(parse_pointer_text(""), None);
        assert_eq!(parse_pointer_text("   \n"), None);
    }

    #[test]
    fn parse_pointer_text_rejects_relative_paths() {
        assert_eq!(parse_pointer_text("Chud"), None);
        assert_eq!(parse_pointer_text("some\\relative\\path"), None);
    }

    #[test]
    fn parse_pointer_text_accepts_absolute_path_trimmed() {
        let dir = std::env::temp_dir().join("chud_paths_test_pointer");
        let text = format!("  {}  \n", dir.display());
        assert_eq!(parse_pointer_text(&text), Some(dir));
    }

    #[test]
    fn pointer_target_usable_for_existing_and_creatable_dirs() {
        let dir = std::env::temp_dir().join("chud_paths_test_pointer_target");
        let _ = std::fs::remove_dir_all(&dir); // start clean
        assert!(!dir.is_dir());
        // Not yet existing, but creatable — same case `resolve_data_root`
        // treats as usable (an absolute path picked via the folder dialog
        // that hasn't been created yet).
        assert!(pointer_target_usable(&dir));
        assert!(dir.is_dir());
        // Already existing.
        assert!(pointer_target_usable(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
