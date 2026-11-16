//! Windows resource setup for the `gorilla-bot` build script.
//!
//! Every shipped `.exe` needs the same icon and the same version block, so the
//! logic lives here instead of in `build.rs`. What *differs* between a window app
//! and a terminal app is passed in through [`Options`]:
//!
//! * [`Options::no_console`] switches the binary to the GUI subsystem. It must
//!   stay off for `gorilla-bot`: the same exe also runs `login`, `scan`, `check`
//!   and the bot itself from a terminal, and the GUI flag already comes from
//!   `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` in
//!   `main.rs` — which keeps the console in debug builds, where it is needed.
//! * [`Options::require_admin`] is off: nothing here writes to system-wide
//!   locations.
//!
//! The descriptive fields (`FileDescription`, `ProductName`, …) are not set
//! here: `winres` reads them from `[package.metadata.winres]` of the crate being
//! built.
//!
//! On non-Windows targets [`apply`] is a no-op, so callers need no `cfg`.

/// The application logo, owned by this crate so both the `.exe` icon and the
/// window icon come from one file.
///
/// It is a PNG: the source artwork only exists as one big bitmap, so the
/// multi-size `.ico` for the executable is generated at build time and the
/// window icon is decoded straight from these bytes at startup.
pub const LOGO: &[u8] = include_bytes!("../assets/logo.png");

/// The same file as a path, for build scripts that render their own `.ico`.
///
/// `CARGO_MANIFEST_DIR` expands here — while *this* crate is compiled — so the
/// value is this crate's own directory no matter which build script reads it.
pub const LOGO_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/logo.png");

/// What to stamp into a Windows executable.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Marks release builds as needing administrator rights.
    pub require_admin: bool,
    /// Switches the binary to the GUI subsystem. Only for window-only apps.
    pub no_console: bool,
}

/// A release build must be elevated; a debug build has to stay usable without a
/// UAC prompt, so a developer can attach a debugger.
#[cfg(windows)]
fn is_release() -> bool {
    std::env::var("PROFILE").is_ok_and(|profile| profile == "release")
}

/// Packs a dotted version string into the four 16-bit fields Windows expects.
///
/// Missing or unparsable components become `0`, so `0.2.0` and a full
/// `0.2.0.1` both produce a valid resource.
#[cfg(windows)]
fn version_number() -> u64 {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let parts: Vec<u64> = version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    let get = |index: usize| parts.get(index).copied().unwrap_or(0);
    (get(0) << 48) | (get(1) << 32) | (get(2) << 16) | get(3)
}

/// The manifest that marks the binary as needing administrator rights.
///
/// It replaces the default manifest wholesale, which is what `winres` does for
/// `set_manifest`.
#[cfg(windows)]
const ELEVATE_MANIFEST: &str = r#"
    <assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
    <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
        <security>
            <requestedPrivileges>
                <requestedExecutionLevel level="requireAdministrator" uiAccess="false" />
            </requestedPrivileges>
        </security>
    </trustInfo>
    </assembly>
    "#;

/// Compiles the resources into the calling crate's binary.
///
/// `icon` is a `.ico` file the caller produced (see [`LOGO`] for the source
/// artwork); `winres` only accepts a filename, not bytes.
///
/// Fails the build on error: a binary without its icon is not the artifact the
/// release expects, and shipping one silently is worse than not shipping it.
#[cfg(windows)]
pub fn apply(options: Options, icon: impl AsRef<std::path::Path>) {
    let mut res = winres::WindowsResource::new();
    res.set_icon(icon.as_ref().to_str().expect("icon path is not UTF-8"));

    if options.require_admin && is_release() {
        res.set_manifest(ELEVATE_MANIFEST);
    }
    if options.no_console {
        res.set("NO_CONSOLE", "1");
    }

    let version = version_number();
    res.set_version_info(winres::VersionInfo::PRODUCTVERSION, version)
        .set_version_info(winres::VersionInfo::FILEVERSION, version);

    if let Err(e) = res.compile() {
        eprintln!("Failed to compile Windows resources: {e}");
        std::process::exit(1);
    }
}

/// Non-Windows no-op, so callers do not need their own `cfg`.
#[cfg(not(windows))]
pub fn apply(_options: Options, _icon: impl AsRef<std::path::Path>) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logo is embedded at compile time, so a missing or empty file would
    /// produce a binary with no icon and nothing else would notice.
    #[test]
    fn the_logo_is_embedded() {
        assert!(!LOGO.is_empty(), "logo.png must be embedded");
        // PNG magic, so the window icon and the build script agree on the format.
        assert_eq!(&LOGO[..4], b"\x89PNG", "not a PNG file");
    }

    /// The build script reads the artwork by path, so a path that does not
    /// resolve would fail deep inside the resource compiler instead.
    #[test]
    fn the_logo_path_points_at_the_embedded_logo() {
        assert!(
            LOGO_PATH.ends_with("assets/logo.png"),
            "unexpected logo path: {LOGO_PATH}",
        );
        let on_disk = std::fs::read(LOGO_PATH).expect("logo.png must exist on disk");
        assert_eq!(
            on_disk, LOGO,
            "the path and the embedded bytes must be the same file",
        );
    }

    /// The version packing is platform-independent logic worth pinning down:
    /// the field layout is what Windows reads back in Explorer.
    #[test]
    fn version_number_packs_four_components() {
        fn pack(version: &str) -> u64 {
            let parts: Vec<u64> = version
                .split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect();
            let get = |index: usize| parts.get(index).copied().unwrap_or(0);
            (get(0) << 48) | (get(1) << 32) | (get(2) << 16) | get(3)
        }
        const MAJOR: u64 = 1 << 48;
        const MINOR: u64 = 1 << 32;
        const BUILD: u64 = 1 << 16;
        const REVISION: u64 = 1;

        assert_eq!(
            pack("3.2.1.4"),
            3 * MAJOR | 2 * MINOR | BUILD | 4 * REVISION,
            "all four components",
        );
        assert_eq!(pack("3.2.1"), 3 * MAJOR | 2 * MINOR | BUILD);
        assert_eq!(pack("3.2"), 3 * MAJOR | 2 * MINOR);
        assert_eq!(
            pack("3.2.1.4.9"),
            3 * MAJOR | 2 * MINOR | BUILD | 4 * REVISION,
        );
        assert_eq!(pack("1.x.0"), MAJOR);
        assert_eq!(pack(""), 0);
    }
}