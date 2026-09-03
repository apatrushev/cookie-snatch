use crate::{
    Browser,
    BrowserKind,
    DefaultBrowser,
    Engine,
    error::{Error, Result},
};
use std::path::{Path, PathBuf};

/// One known browser product: the names to look for on `PATH` and the absolute locations it is
/// installed to per OS. This catalog is the single source of truth for "where browsers live",
/// replacing the per-backend hardcoded candidate lists.
struct Catalog {
    kind: BrowserKind,
    exe_names: &'static [&'static str],
    app_paths: &'static [&'static str],
}

const CATALOG: &[Catalog] = &[
    Catalog {
        kind: BrowserKind::Chrome,
        exe_names: &["google-chrome", "google-chrome-stable", "chrome"],
        app_paths: &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ],
    },
    Catalog {
        kind: BrowserKind::Chromium,
        exe_names: &["chromium", "chromium-browser"],
        app_paths: &["/Applications/Chromium.app/Contents/MacOS/Chromium"],
    },
    Catalog {
        kind: BrowserKind::Edge,
        exe_names: &["microsoft-edge", "microsoft-edge-stable"],
        app_paths: &[
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ],
    },
    Catalog {
        kind: BrowserKind::Brave,
        exe_names: &["brave-browser", "brave"],
        app_paths: &[
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
        ],
    },
    Catalog {
        kind: BrowserKind::Opera,
        exe_names: &["opera"],
        app_paths: &[
            "/Applications/Opera.app/Contents/MacOS/Opera",
            r"C:\Program Files\Opera\opera.exe",
        ],
    },
    Catalog {
        kind: BrowserKind::Vivaldi,
        exe_names: &["vivaldi", "vivaldi-stable"],
        app_paths: &[
            "/Applications/Vivaldi.app/Contents/MacOS/Vivaldi",
            r"C:\Program Files\Vivaldi\Application\vivaldi.exe",
        ],
    },
    Catalog {
        kind: BrowserKind::Firefox,
        exe_names: &["firefox", "firefox-bin", "firefox-esr"],
        app_paths: &[
            "/Applications/Firefox.app/Contents/MacOS/firefox",
            r"C:\Program Files\Mozilla Firefox\firefox.exe",
            r"C:\Program Files (x86)\Mozilla Firefox\firefox.exe",
        ],
    },
];

/// Brand tokens matched (case-insensitively, in order) against an OS handler identifier -
/// a Windows `ProgId`, a Linux `.desktop` basename, or a macOS bundle id / `.app` name.
/// Order matters: `chromium` must be tried before `chrome`.
const BRAND_TOKENS: &[(&str, BrowserKind)] = &[
    ("firefox", BrowserKind::Firefox),
    ("edg", BrowserKind::Edge),
    ("brave", BrowserKind::Brave),
    ("vivaldi", BrowserKind::Vivaldi),
    ("opera", BrowserKind::Opera),
    ("chromium", BrowserKind::Chromium),
    ("chrome", BrowserKind::Chrome),
];

/// Classify an OS handler identifier into a [`BrowserKind`]. Pure; Safari and anything unknown
/// return `None`.
fn kind_from_identifier(id: &str) -> Option<BrowserKind> {
    let id = id.to_ascii_lowercase();
    BRAND_TOKENS
        .iter()
        .find(|(token, _)| id.contains(token))
        .map(|(_, kind)| *kind)
}

/// Resolve a catalog entry to a concrete executable: first hit on `PATH` (the `which` crate
/// handles Windows `PATHEXT`), else the first existing absolute install path. Canonicalized
/// best-effort so alias names collapse to one path.
fn locate(entry: &Catalog) -> Option<PathBuf> {
    let found = entry
        .exe_names
        .iter()
        .find_map(|name| which::which(name).ok())
        .or_else(|| {
            entry
                .app_paths
                .iter()
                .map(Path::new)
                .find(|path| path.is_file())
                .map(Path::to_path_buf)
        })?;
    Some(std::fs::canonicalize(&found).unwrap_or(found))
}

fn locate_kind(kind: BrowserKind) -> Option<PathBuf> {
    CATALOG
        .iter()
        .find(|entry| entry.kind == kind)
        .and_then(locate)
}

/// Every driveable browser found on `PATH` or in a known install location. Best-effort: an empty
/// vec means none were found, not an error.
#[must_use]
pub fn installed_browsers() -> Vec<Browser> {
    CATALOG
        .iter()
        .filter_map(|entry| {
            locate(entry).map(|path| Browser {
                kind: entry.kind,
                path,
            })
        })
        .collect()
}

/// Resolve a binary for `engine`: an existing override in `env_var` first, else the first
/// installed browser whose kind is driven by `engine`.
///
/// # Errors
///
/// Returns [`Error::BrowserNotFound`] if neither the override nor any catalog entry resolves.
pub fn find(
    engine: Engine,
    env_var: &'static str,
    browser: &'static str,
) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(env_var) {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Ok(path);
        }
    }
    CATALOG
        .iter()
        .filter(|entry| entry.kind.engine() == engine)
        .find_map(locate)
        .ok_or(Error::BrowserNotFound { browser, env_var })
}

/// Query the OS for its default web-browser handler.
///
/// # Errors
///
/// Returns [`Error::Io`] if the OS query itself fails (subprocess spawn, registry access, or
/// `LaunchServices` call). A *missing* or *unrecognized* default is reported through the
/// [`DefaultBrowser`] variants, not as an error.
pub fn default_browser() -> Result<DefaultBrowser> {
    let id = match os_default()? {
        Some(id) if !id.is_empty() => id,
        _ => return Ok(DefaultBrowser::Unknown),
    };
    match kind_from_identifier(&id)
        .and_then(|kind| locate_kind(kind).map(|path| (kind, path)))
    {
        Some((kind, path)) => Ok(DefaultBrowser::Supported(Browser { kind, path })),
        None => Ok(DefaultBrowser::Unsupported { name: id }),
    }
}

#[cfg(target_os = "linux")]
fn os_default() -> Result<Option<String>> {
    // A fast, local, non-network subprocess with a fixed argv (no shell, no interpolation).
    let output = match std::process::Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
    {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!id.is_empty()).then_some(id))
}

#[cfg(windows)]
fn os_default() -> Result<Option<String>> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    let sub = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice";
    let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(sub) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    match key.get_value::<String, _>("ProgId") {
        Ok(progid) => Ok(Some(progid)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(target_os = "macos")]
fn os_default() -> Result<Option<String>> {
    use core_foundation::{base::TCFType, string::CFString, url::CFURL};
    use core_foundation_sys::{base::kCFAllocatorDefault, url::CFURLRef};
    use std::{ffi::c_void, ptr};

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyDefaultApplicationURLForURL(
            in_url: CFURLRef,
            in_role_mask: u32,
            out_error: *mut *const c_void,
        ) -> CFURLRef;
    }
    const K_LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

    let probe = CFString::new("https://a.example");
    // SAFETY: `probe` is a live CFString for the whole block. `CFURLCreateWithString` with a null
    // base URL is valid for an absolute string; both CF objects returned by *Create* functions are
    // taken under the Create rule so they are released exactly once on drop. `out_error` is written
    // by LaunchServices and read only as an opaque pointer we discard.
    let app = unsafe {
        let probe_url = core_foundation_sys::url::CFURLCreateWithString(
            kCFAllocatorDefault,
            probe.as_concrete_TypeRef(),
            ptr::null(),
        );
        if probe_url.is_null() {
            return Err(Error::Io(std::io::Error::other(
                "failed to construct the LaunchServices probe URL",
            )));
        }
        let probe_url = CFURL::wrap_under_create_rule(probe_url);
        let mut error: *const c_void = ptr::null();
        let app_url = LSCopyDefaultApplicationURLForURL(
            probe_url.as_concrete_TypeRef(),
            K_LS_ROLES_ALL,
            &raw mut error,
        );
        if app_url.is_null() {
            return Ok(None);
        }
        CFURL::wrap_under_create_rule(app_url)
    };
    Ok(app.to_path().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn os_default() -> Result<Option<String>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{CATALOG, kind_from_identifier};
    use crate::{BrowserKind, Engine};
    use rstest::rstest;

    #[rstest]
    #[case("ChromeHTML", BrowserKind::Chrome)]
    #[case("com.google.chrome", BrowserKind::Chrome)]
    #[case("google-chrome.desktop", BrowserKind::Chrome)]
    #[case("FirefoxURL", BrowserKind::Firefox)]
    #[case("firefox.desktop", BrowserKind::Firefox)]
    #[case("org.mozilla.firefox", BrowserKind::Firefox)]
    #[case("MSEdgeHTM", BrowserKind::Edge)]
    #[case("com.microsoft.edgemac", BrowserKind::Edge)]
    #[case("BraveHTML", BrowserKind::Brave)]
    #[case("com.operasoftware.Opera", BrowserKind::Opera)]
    #[case("Vivaldi.app", BrowserKind::Vivaldi)]
    #[case("chromium-browser.desktop", BrowserKind::Chromium)]
    fn classifies_known_identifiers(#[case] id: &str, #[case] expected: BrowserKind) {
        assert_eq!(kind_from_identifier(id), Some(expected));
    }

    #[rstest]
    #[case("Safari.app")]
    #[case("com.apple.Safari")]
    #[case("")]
    #[case("some-random-thing")]
    fn rejects_unknown_identifiers(#[case] id: &str) {
        assert!(kind_from_identifier(id).is_none());
    }

    #[rstest]
    #[case(BrowserKind::Chrome, Engine::Chrome)]
    #[case(BrowserKind::Chromium, Engine::Chrome)]
    #[case(BrowserKind::Edge, Engine::Chrome)]
    #[case(BrowserKind::Brave, Engine::Chrome)]
    #[case(BrowserKind::Opera, Engine::Chrome)]
    #[case(BrowserKind::Vivaldi, Engine::Chrome)]
    #[case(BrowserKind::Firefox, Engine::Firefox)]
    fn maps_kind_to_engine(#[case] kind: BrowserKind, #[case] engine: Engine) {
        assert_eq!(kind.engine(), engine);
    }

    #[rstest]
    #[case(BrowserKind::Chrome)]
    #[case(BrowserKind::Chromium)]
    #[case(BrowserKind::Edge)]
    #[case(BrowserKind::Brave)]
    #[case(BrowserKind::Opera)]
    #[case(BrowserKind::Vivaldi)]
    #[case(BrowserKind::Firefox)]
    fn catalog_contains_kind(#[case] kind: BrowserKind) {
        assert!(CATALOG.iter().any(|entry| entry.kind == kind));
    }
}
