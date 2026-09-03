# cookie-snatch

Capture a website's session cookies by driving an **already-installed browser** through its remote
protocol, so an automation or AI tool can reuse a human's login when the target system has no SSO or
API token to offer.

You launch a fresh, throwaway browser instance, log in by hand as usual, and `cookie-snatch` reads
the resulting cookies - including `HttpOnly` session cookies that page JavaScript cannot see, because
it reads them over the browser's debug protocol rather than `document.cookie`.

- **Chrome / Chromium** via the Chrome DevTools Protocol (CDP).
- **Firefox** via WebDriver BiDi.

## How it works

1. Launch the browser as a child process with a debug port and a disposable profile - never touching
   your real profile or a running instance.
2. You log in in the window that opens.
3. `cookie-snatch` connects over a WebSocket, reads the cookies, and hands them back.
4. Dropping the session closes the browser and deletes the temporary profile.

## Requirements

- Rust 1.85+ (edition 2024).
- A local, graphical desktop session - the login is interactive, so this does **not** work headless
  or over SSH without a display.
- Chrome/Chromium or Firefox installed. `cookie-snatch` looks on `PATH` and in the usual install
  locations; override with the `CHROME` or `FIREFOX` environment variable pointing at the binary.

## Use as a library

Add it to your `Cargo.toml` (path or git dependency; not yet published to crates.io):

```toml
[dependencies]
cookie-snatch = { path = "../cookie-snatch" }
```

Launch a session, wait for the cookie you care about, and read its value:

```rust,no_run
use cookie_snatch::{Engine, LaunchOptions, Session};
use std::time::Duration;

fn main() -> Result<(), cookie_snatch::Error> {
    let options = LaunchOptions::new(Engine::Chrome, "https://service.example/login");
    let mut session = Session::launch(&options)?;

    // Blocks until `_oauth2_proxy` appears and the cookie set stabilises, or the timeout elapses.
    let cookies = session.wait_for_cookie("_oauth2_proxy", Duration::from_secs(120))?;
    if let Some(cookie) = cookies.iter().find(|c| c.name == "_oauth2_proxy") {
        println!("{}", cookie.value);
    }
    // `session` closes the browser and removes the temp profile when dropped.
    Ok(())
}
```

Prefer to decide "logged in" yourself? Call `session.cookies()` whenever you like and inspect the
returned `Vec<Cookie>`.

Switch to Firefox by passing `Engine::Firefox` to `LaunchOptions::new`.

### Discovering browsers

Instead of naming an `Engine`, you can enumerate what is installed or launch the user's default
(familiar) browser:

```rust,no_run
use cookie_snatch::{DefaultBrowser, Session, default_browser, installed_browsers};

# fn main() -> Result<(), cookie_snatch::Error> {
// Everything driveable found on PATH or in a known install location.
for browser in installed_browsers() {
    println!("{:?} at {}", browser.kind, browser.path.display());
}

// Launch in the OS default browser (errors if it is Safari / not configured).
let mut session = Session::launch_default("https://service.example/login")?;

// Or inspect the default first and decide yourself:
match default_browser()? {
    DefaultBrowser::Supported(browser) => println!("default: {:?}", browser.kind),
    DefaultBrowser::Unsupported { name } => println!("cannot drive {name}"),
    DefaultBrowser::Unknown => println!("no default configured"),
}
# let _ = &mut session;
# Ok(())
# }
```

Any Chromium-family browser (Chrome, Chromium, Edge, Brave, Opera, Vivaldi) is driven over CDP;
Firefox over BiDi. Safari and anything else are reported as `Unsupported` rather than launched.
Default detection is best-effort: `xdg-settings` on Linux, the `UserChoice` registry key on
Windows, and LaunchServices on macOS.

### Public API

| Item | Purpose |
|------|---------|
| `Session::launch(&LaunchOptions)` | Start the browser, open the login URL, connect to the protocol. |
| `Session::launch_default(url)` | Launch in the OS default browser (typed error if it is unsupported). |
| `Session::cookies()` | Read the cookies currently visible to the session. |
| `Session::wait_for_cookie(name, timeout)` | Poll until `name` is present and the set is stable. |
| `LaunchOptions::new(engine, url)` | Build launch config; discover the binary by engine. |
| `LaunchOptions::for_browser(&browser, url)` | Build launch config for a specific discovered `Browser`. |
| `installed_browsers()` | Every driveable browser found on the system. |
| `default_browser()` | The OS default handler, classified as `DefaultBrowser`. |
| `Engine` | `Chrome` or `Firefox` (the remote protocol). |
| `BrowserKind` | Concrete product: `Chrome`, `Chromium`, `Edge`, `Brave`, `Opera`, `Vivaldi`, `Firefox`. |
| `Browser` | A discovered browser (`kind`, `path`); `engine()` maps to its protocol. |
| `DefaultBrowser` | `Supported(Browser)`, `Unsupported { name }`, or `Unknown`. |
| `Cookie` | Captured cookie (`name`, `value`, `domain`, `path`, flags). |
| `Error` / `Result` | Typed error enum and its `Result` alias. |

Full API docs: `cargo doc --open`.

## Command-line example

A ready-to-run CLI lives in `examples/capture.rs`. On start it lists the browsers it found and
lets you pick one; pressing Enter selects the OS default:

```sh
# Print just the value of one cookie (handy for command substitution):
cargo run --example capture -- https://service.example/login _oauth2_proxy

# Omit the cookie name to log in, press Enter, and dump every cookie as JSON:
cargo run --example capture -- https://service.example/login
```

The browser picker prompts on stderr, so command substitution still captures only the cookie value
on stdout.

With a cookie name it prints only that cookie's value to stdout (everything else goes to stderr), so
it drops straight into a shell:

```sh
COOKIE=$(cargo run -q --example capture -- https://service.example/login _oauth2_proxy)
```

## Security & limitations

- The captured value is a **live, full session** - treat it as a credential. It is returned to your
  code and, in the CLI, printed to stdout; it is never logged. Capture it into a variable or file,
  not an `echo`-ed command line, and only use it where you are authorised to.
- Local, single-user, interactive only. No remote/multi-tenant capture, no headless login.
- Firefox cookie values are decoded from both string and `base64` `BytesValue` encodings; a value
  whose bytes are not valid UTF-8 returns an error rather than corrupting the secret.
- Assumes the target authenticates with cookies that replay from a plain HTTP client (not bound to
  User-Agent/IP/device). Verify this for your target before relying on it.
- Firefox's remote agent may reject the connection under a strict origin/host allowlist, or
  `storage.getCookies` may need a partition descriptor for some sites; open an issue if you hit
  either.

## Status

Early, working prototype. Licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option.
