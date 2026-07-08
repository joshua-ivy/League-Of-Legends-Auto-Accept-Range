# Building the native app (Windows, MSVC)

The native app is a **Tauri 2 / Rust** project. Building it needs the Rust
toolchain plus a C/link toolchain. This machine already has Rust 1.96 and
WebView2; the steps below add the C toolchain and build.

## Why each tool is needed

| Tool | Needed for |
|------|-----------|
| **Rust (MSVC)** | already installed (`rustc`/`cargo` 1.96, `stable-x86_64-pc-windows-msvc`). |
| **VS Build Tools (VCTools + Windows SDK)** | provides `link.exe` (final link) and `rc.exe` (Tauri embeds the app icon/manifest as a Windows resource). |
| **NASM** | `aws-lc-rs` (pulled in by `reqwest` + rustls) and `rav1e` (via `image`) compile x86-64 assembly. |
| **CMake** | `aws-lc-rs` builds its C sources with CMake. |
| **WebView2 runtime** | already installed (the UI host). |

## 1. Install the C toolchain (needs Administrator — UAC)

Run an **elevated** PowerShell, then:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e `
  --accept-package-agreements --accept-source-agreements `
  --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621"

winget install --id NASM.NASM       -e --accept-package-agreements --accept-source-agreements
winget install --id Kitware.CMake   -e --accept-package-agreements --accept-source-agreements
```

NASM installs to `C:\Program Files\NASM`; make sure it (and CMake's `bin`) are on
`PATH` for the build shell. `aws-lc-rs` looks for `nasm` on `PATH`.

## 2. Build

```powershell
cd native
cargo build              # debug
cargo build --release    # optimized single exe (profile in workspace Cargo.toml)
```

`rustc` auto-detects the MSVC tools via vswhere — a normal (non-developer) shell
works once Build Tools are installed.

## 3. Package (MSI + NSIS installers)

```powershell
cargo install tauri-cli --version "^2"   # one-time
cd native
cargo tauri build                         # emits MSI + NSIS under src-tauri/target/release/bundle/
```

`tauri.conf.json` already targets `["msi", "nsis"]` and references the icons in
`src-tauri/icons/` (present).

## Notes

- **OneDrive + git:** this repo lives under `OneDrive\Desktop`. Git ref updates
  can fail while OneDrive is syncing; pause OneDrive (or move the repo out) before
  committing the `native/` tree.
- **No-admin alternative (GNU):** if you can't elevate, a portable MinGW-w64 +
  Rust's `stable-x86_64-pc-windows-gnu` toolchain can build without admin, but
  needs `reqwest` switched to native-TLS (SChannel) and `image` to PNG-only to
  avoid the NASM/CMake C deps. Kept as a fallback only — MSVC is the primary target.
