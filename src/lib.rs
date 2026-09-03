//! Capture a browser session's cookies by driving an already-installed browser over its remote
//! protocol (Chrome `DevTools` Protocol or Firefox `WebDriver BiDi`). Launch a [`Session`], let the
//! user log in, then read [`Session::cookies`] or block on [`Session::wait_for_cookie`].
//!
//! Cookies are read over the debug protocol, so `HttpOnly` session cookies invisible to page
//! JavaScript are captured too. The browser runs as a fresh instance in a throwaway profile; the
//! [`Session`] closes it and removes the profile on drop.
//!
//! # Example
//!
//! ```rust,no_run
//! use cookie_snatch::{Engine, LaunchOptions, Session};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), cookie_snatch::Error> {
//! let options = LaunchOptions::new(Engine::Chrome, "https://service.example/login");
//! let mut session = Session::launch(&options)?;
//!
//! // Blocks until the cookie appears and the set stabilises, or the timeout elapses.
//! let cookies = session.wait_for_cookie("_oauth2_proxy", Duration::from_secs(120))?;
//! if let Some(cookie) = cookies.iter().find(|c| c.name == "_oauth2_proxy") {
//!     println!("{}", cookie.value);
//! }
//! # Ok(())
//! # }
//! ```
#![deny(missing_docs)]

mod chrome;
mod discover;
mod error;
mod firefox;
mod transport;

pub use discover::{default_browser, installed_browsers};
pub use error::{Error, Result};

use serde::{Deserialize, Serialize};
use std::{
    net::Ipv4Addr,
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};
use transport::Wire;

const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Which browser and remote protocol to drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// Chromium-family browser over the `DevTools` protocol.
    Chrome,
    /// Firefox over `WebDriver BiDi`.
    Firefox,
}

/// A specific browser product that can be discovered on the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrowserKind {
    /// Google Chrome.
    Chrome,
    /// Chromium.
    Chromium,
    /// Microsoft Edge.
    Edge,
    /// Brave.
    Brave,
    /// Opera.
    Opera,
    /// Vivaldi.
    Vivaldi,
    /// Mozilla Firefox.
    Firefox,
}

impl BrowserKind {
    /// The remote-protocol [`Engine`] used to drive this browser. Every Chromium-family browser
    /// is driven over CDP as [`Engine::Chrome`]; Firefox over `WebDriver BiDi`.
    #[must_use]
    pub fn engine(self) -> Engine {
        match self {
            BrowserKind::Firefox => Engine::Firefox,
            BrowserKind::Chrome
            | BrowserKind::Chromium
            | BrowserKind::Edge
            | BrowserKind::Brave
            | BrowserKind::Opera
            | BrowserKind::Vivaldi => Engine::Chrome,
        }
    }
}

/// A browser discovered on the system: its product [`BrowserKind`] and the executable path.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Browser {
    /// Which browser product this is.
    pub kind: BrowserKind,
    /// Absolute path to the browser executable.
    pub path: PathBuf,
}

impl Browser {
    /// The remote-protocol [`Engine`] used to drive this browser.
    #[must_use]
    pub fn engine(&self) -> Engine {
        self.kind.engine()
    }
}

/// The OS default web-browser handler, classified for capture. The three outcomes - a browser we
/// can drive, one we cannot, or none - are exhaustive, so callers may `match` without a catch-all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultBrowser {
    /// A default this crate can drive.
    Supported(Browser),
    /// A default this crate cannot drive (e.g. Safari), named for diagnostics.
    Unsupported {
        /// The OS-reported handler name.
        name: String,
    },
    /// No default is configured, or it could not be resolved.
    Unknown,
}

/// A captured cookie. `expires` is `-1` for session cookies (CDP convention).
///
/// The [`Debug`] impl deliberately redacts [`value`](Cookie::value) - it is a live credential for a
/// session cookie - so logging a `Cookie` never leaks the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Cookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value - the secret for a session cookie.
    pub value: String,
    /// Domain the cookie is scoped to.
    pub domain: String,
    /// Path the cookie is scoped to.
    pub path: String,
    /// Unix expiry in seconds, or `-1` for a session cookie.
    pub expires: f64,
    /// Whether the cookie is `HttpOnly` (invisible to page JavaScript).
    pub http_only: bool,
    /// Whether the cookie is only sent over a secure (HTTPS) connection.
    pub secure: bool,
    /// Whether this is a session cookie with no persistent expiry.
    pub session: bool,
}

impl std::fmt::Debug for Cookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Destructure so a new field forces this redacting impl to be revisited.
        let Self {
            name,
            value,
            domain,
            path,
            expires,
            http_only,
            secure,
            session,
        } = self;
        f.debug_struct("Cookie")
            .field("name", name)
            .field("value", &format_args!("<redacted {} bytes>", value.len()))
            .field("domain", domain)
            .field("path", path)
            .field("expires", expires)
            .field("http_only", http_only)
            .field("secure", secure)
            .field("session", session)
            .finish()
    }
}

/// How to launch a browser for capture.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LaunchOptions {
    /// Which browser and remote protocol to drive.
    pub engine: Engine,
    /// URL opened in the browser for the interactive login.
    pub login_url: String,
    /// How long to wait for the browser's remote endpoint to come up.
    pub startup_timeout: Duration,
    /// Explicit path to the browser executable. When `None`, the binary is discovered from
    /// [`engine`](LaunchOptions::engine).
    pub browser_path: Option<PathBuf>,
}

impl LaunchOptions {
    /// Build options for `engine` opening `login_url`, discovering the binary by engine, with the
    /// default startup timeout.
    #[must_use]
    pub fn new(engine: Engine, login_url: impl Into<String>) -> Self {
        Self {
            engine,
            login_url: login_url.into(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            browser_path: None,
        }
    }

    /// Build options that launch a specific discovered [`Browser`] (its engine and executable
    /// path), opening `login_url`, with the default startup timeout.
    #[must_use]
    pub fn for_browser(browser: &Browser, login_url: impl Into<String>) -> Self {
        Self {
            engine: browser.engine(),
            login_url: login_url.into(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            browser_path: Some(browser.path.clone()),
        }
    }
}

enum Backend {
    Chrome { wire: Wire, session_id: String },
    Firefox { wire: Wire },
}

impl Backend {
    fn cookies(&mut self) -> Result<Vec<Cookie>> {
        match self {
            Backend::Chrome { wire, session_id } => chrome::cookies(wire, session_id),
            Backend::Firefox { wire } => firefox::cookies(wire),
        }
    }

    fn close(&mut self) {
        match self {
            Backend::Chrome { wire, .. } => chrome::close(wire),
            Backend::Firefox { wire } => firefox::close(wire),
        }
    }
}

/// A live browser instance in a throwaway profile. Dropping it closes the browser and removes the
/// profile, so callers never leak a process or a session on disk.
pub struct Session {
    child: Child,
    profile_dir: PathBuf,
    backend: Backend,
}

impl Session {
    /// Launch the browser, open the login URL, and connect to its remote protocol.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLoginUrl`] for a non-http(s) URL, [`Error::BrowserNotFound`] if the
    /// binary is missing, or a launch/transport/protocol error if the browser fails to come up.
    pub fn launch(options: &LaunchOptions) -> Result<Self> {
        if !is_http_url(&options.login_url) {
            return Err(Error::InvalidLoginUrl);
        }
        let profile_dir =
            std::env::temp_dir().join(format!("cookie-snatch-{}", std::process::id()));
        let bin = resolve_binary(options)?;

        match options.engine {
            Engine::Chrome => {
                create_profile_dir(&profile_dir)?;
                let child =
                    spawn_or_clean(&profile_dir, chrome::spawn(&bin, &profile_dir))?;
                finish(child, profile_dir, |dir| {
                    let (wire, session_id) = chrome::connect(
                        dir,
                        &options.login_url,
                        options.startup_timeout,
                    )?;
                    Ok(Backend::Chrome { wire, session_id })
                })
            }
            Engine::Firefox => {
                let port = firefox::free_port()?;
                create_profile_dir(&profile_dir)?;
                let child = spawn_or_clean(
                    &profile_dir,
                    firefox::spawn(&bin, &profile_dir, port),
                )?;
                finish(child, profile_dir, |_dir| {
                    let wire = firefox::connect(
                        port,
                        &options.login_url,
                        options.startup_timeout,
                    )?;
                    Ok(Backend::Firefox { wire })
                })
            }
        }
    }

    /// Launch a capture session in the OS default web browser, opening `login_url`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedDefaultBrowser`] if the default is a browser this crate cannot
    /// drive (e.g. Safari), [`Error::NoDefaultBrowser`] if none is configured, or any error from
    /// [`Session::launch`]. To fall back on your own choice, call [`installed_browsers`] and
    /// [`LaunchOptions::for_browser`] instead.
    pub fn launch_default(login_url: impl Into<String>) -> Result<Self> {
        match discover::default_browser()? {
            DefaultBrowser::Supported(browser) => {
                Self::launch(&LaunchOptions::for_browser(&browser, login_url))
            }
            DefaultBrowser::Unsupported { name } => {
                Err(Error::UnsupportedDefaultBrowser { name })
            }
            DefaultBrowser::Unknown => Err(Error::NoDefaultBrowser),
        }
    }

    /// Read the cookies currently visible to the session.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the protocol call fails or the response cannot be decoded.
    pub fn cookies(&mut self) -> Result<Vec<Cookie>> {
        self.backend.cookies()
    }

    /// Poll until a cookie named `name` is present and the whole set is stable across two reads.
    /// The stability check guards against pre-login and multi-step (2FA) interim cookies.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CookieTimeout`] if the cookie does not appear before `timeout`, or a
    /// protocol error from an underlying read.
    pub fn wait_for_cookie(
        &mut self,
        name: &str,
        timeout: Duration,
    ) -> Result<Vec<Cookie>> {
        let deadline = Instant::now() + timeout;
        let mut previous: Option<Vec<Cookie>> = None;
        loop {
            let current = self.backend.cookies()?;
            let present = current.iter().any(|c| c.name == name);
            let stable = previous
                .as_deref()
                .is_some_and(|prev| cookies_stable(prev, &current));
            if present && stable {
                return Ok(current);
            }
            if Instant::now() >= deadline {
                return Err(Error::CookieTimeout {
                    name: name.to_owned(),
                    timeout,
                });
            }
            previous = Some(current);
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.backend.close();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile_dir);
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// The explicit override when set, else the binary discovered for the engine.
fn resolve_binary(options: &LaunchOptions) -> Result<PathBuf> {
    match &options.browser_path {
        Some(path) => Ok(path.clone()),
        None => match options.engine {
            Engine::Chrome => chrome::find(),
            Engine::Firefox => firefox::find(),
        },
    }
}

/// Order-insensitive equality of two cookie snapshots: `getAllCookies` / `getCookies` do not
/// guarantee a stable order across reads, so comparing raw `Vec`s could loop until the timeout even
/// after the session settled. Compare as sets keyed by `(name, domain, path)`.
fn cookies_stable(previous: &[Cookie], current: &[Cookie]) -> bool {
    fn sorted(cookies: &[Cookie]) -> Vec<&Cookie> {
        let mut refs: Vec<&Cookie> = cookies.iter().collect();
        refs.sort_by(|a, b| {
            (a.name.as_str(), a.domain.as_str(), a.path.as_str()).cmp(&(
                b.name.as_str(),
                b.domain.as_str(),
                b.path.as_str(),
            ))
        });
        refs
    }
    previous.len() == current.len() && sorted(previous) == sorted(current)
}

/// Create the throwaway profile directory owned only by the current user (`0700` on Unix), so the
/// captured session's on-disk state is not world-readable.
fn create_profile_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(Error::from)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path).map_err(Error::from)
    }
}

/// Remove the freshly created profile dir if the browser failed to even spawn.
fn spawn_or_clean(
    profile_dir: &std::path::Path,
    spawned: Result<Child>,
) -> Result<Child> {
    spawned.inspect_err(|_e| {
        let _ = std::fs::remove_dir_all(profile_dir);
    })
}

/// Build the backend; on failure, kill the browser and remove the profile before returning.
fn finish(
    mut child: Child,
    profile_dir: PathBuf,
    build: impl FnOnce(&std::path::Path) -> Result<Backend>,
) -> Result<Session> {
    match build(&profile_dir) {
        Ok(backend) => Ok(Session {
            child,
            profile_dir,
            backend,
        }),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&profile_dir);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cookie, cookies_stable};

    fn cookie(name: &str, domain: &str, value: &str) -> Cookie {
        Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: domain.to_owned(),
            path: "/".to_owned(),
            expires: -1.0,
            http_only: true,
            secure: true,
            session: true,
        }
    }

    #[test]
    fn stable_ignores_read_order() {
        let a = cookie("a", "x.example", "1");
        let b = cookie("b", "y.example", "2");
        let first = vec![a.clone(), b.clone()];
        let second = vec![b, a];
        assert!(cookies_stable(&first, &second));
    }

    #[test]
    fn stable_detects_value_change() {
        let before = vec![cookie("a", "x.example", "1")];
        let after = vec![cookie("a", "x.example", "2")];
        assert!(!cookies_stable(&before, &after));
    }

    #[test]
    fn stable_detects_size_change() {
        let before = vec![cookie("a", "x.example", "1")];
        let after = vec![cookie("a", "x.example", "1"), cookie("b", "x.example", "2")];
        assert!(!cookies_stable(&before, &after));
    }

    #[test]
    fn debug_redacts_secret_value() {
        let rendered = format!("{:?}", cookie("session", "x.example", "top-secret"));
        assert!(!rendered.contains("top-secret"));
        assert!(rendered.contains("<redacted"));
    }
}
