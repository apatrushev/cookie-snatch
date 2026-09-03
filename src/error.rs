use std::time::Duration;

/// Everything that can go wrong while capturing a browser session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The login URL did not start with `http://` or `https://`.
    #[error("login url must start with http:// or https://")]
    InvalidLoginUrl,
    /// No browser binary was found and no override was set.
    #[error("{browser} binary not found; set the {env_var} env var or install it")]
    BrowserNotFound {
        /// Browser that was being looked for, e.g. `chrome`.
        browser: &'static str,
        /// Environment variable that can point at the binary, e.g. `CHROME`.
        env_var: &'static str,
    },
    /// The browser process failed to spawn.
    #[error("failed to launch the browser")]
    Launch(#[source] std::io::Error),
    /// The browser's remote debug endpoint did not come up within the startup timeout.
    #[error("the browser debug endpoint did not come up within {0:?}")]
    EndpointTimeout(Duration),
    /// The WebSocket connection to the browser failed.
    #[error("websocket transport error")]
    Transport(#[source] tungstenite::Error),
    /// A remote-protocol command returned an error response.
    #[error("{protocol} command `{method}` failed: {message}")]
    Command {
        /// Protocol the command belongs to (`cdp` or `bidi`).
        protocol: &'static str,
        /// The command method that failed.
        method: String,
        /// The error message reported by the browser.
        message: String,
    },
    /// A protocol response was missing an expected field or had the wrong shape.
    #[error("malformed {protocol} response: {detail}")]
    Malformed {
        /// Protocol the response belongs to (`cdp`, `bidi`, or `ws`).
        protocol: &'static str,
        /// What was wrong with the response.
        detail: String,
    },
    /// The target cookie did not appear before the timeout.
    #[error("timed out after {timeout:?} waiting for cookie `{name}`")]
    CookieTimeout {
        /// Cookie name that was awaited.
        name: String,
        /// Timeout that elapsed.
        timeout: Duration,
    },
    /// A cookie value used an encoding this crate does not decode (e.g. `base64`).
    #[error("unsupported cookie value encoding `{encoding}`")]
    UnsupportedCookieEncoding {
        /// The encoding tag reported by the browser.
        encoding: String,
    },
    /// The cookie list could not be deserialized into [`crate::Cookie`] values.
    #[error("failed to decode cookies")]
    DecodeCookies(#[source] serde_json::Error),
    /// The OS default browser is a product this crate cannot drive (e.g. Safari).
    #[error("the default browser `{name}` is not supported for capture")]
    UnsupportedDefaultBrowser {
        /// The OS-reported handler name.
        name: String,
    },
    /// No default web browser is configured, or it could not be resolved.
    #[error("no default web browser is configured")]
    NoDefaultBrowser,
    /// A filesystem or process I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Convenience alias for a [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
