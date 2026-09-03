//! CLI example: pick an installed browser and drive it to capture a session cookie.
//!
//! ```text
//! cargo run --example capture -- <login-url> [target-cookie-name]
//! ```
//! On start it lists the browsers found on the system and lets you choose one; pressing Enter
//! selects the OS default. With a target cookie name it prints only that cookie's value (fits
//! `VAR=$(...)`); without one it waits for Enter and prints the full cookie set as JSON.

use anyhow::{Context, bail};
use cookie_snatch::{
    Browser,
    DefaultBrowser,
    LaunchOptions,
    Session,
    default_browser,
    installed_browsers,
};
use std::{io::Write, time::Duration};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

enum Trigger {
    Manual,
    WaitForCookie(String),
}

struct Args {
    login_url: String,
    trigger: Trigger,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut positionals = std::env::args().skip(1);
    let login_url = positionals
        .next()
        .context("usage: capture <login-url> [target-cookie-name]")?;
    let trigger = match positionals.next() {
        Some(name) if !name.is_empty() => Trigger::WaitForCookie(name),
        _ => Trigger::Manual,
    };
    Ok(Args { login_url, trigger })
}

/// Index of the OS default within `browsers`, matched by executable path. `None` if the default
/// is unsupported, unset, or not among the discovered browsers.
fn os_default_index(browsers: &[Browser], default: &DefaultBrowser) -> Option<usize> {
    match default {
        DefaultBrowser::Supported(browser) => {
            browsers.iter().position(|b| b.path == browser.path)
        }
        DefaultBrowser::Unsupported { .. } | DefaultBrowser::Unknown => None,
    }
}

/// Resolve a raw menu line into a 0-based index. Empty input selects `enter_index`; otherwise the
/// input must be a 1-based number within `1..=count`.
fn parse_choice(
    input: &str,
    count: usize,
    enter_index: usize,
) -> anyhow::Result<usize> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(enter_index);
    }
    let choice: usize = trimmed
        .parse()
        .with_context(|| format!("`{trimmed}` is not a browser number"))?;
    match choice {
        n if (1..=count).contains(&n) => Ok(n - 1),
        n => bail!("choice {n} is out of range 1..={count}"),
    }
}

fn select_browser(browsers: &[Browser]) -> anyhow::Result<usize> {
    let default = default_browser()?;
    let os_default = os_default_index(browsers, &default);
    let enter_index = os_default.unwrap_or(0);

    eprintln!("Available browsers:");
    for (i, browser) in browsers.iter().enumerate() {
        let marker = if Some(i) == os_default {
            " (system default)"
        } else {
            ""
        };
        eprintln!("  [{}] {:?}{marker}", i + 1, browser.kind);
    }
    eprint!("Select browser [Enter = {}]: ", enter_index + 1);
    std::io::stderr().flush()?;

    let mut choice = String::new();
    std::io::stdin().read_line(&mut choice)?;
    parse_choice(&choice, browsers.len(), enter_index)
}

fn main() -> anyhow::Result<()> {
    let args = parse_args()?;
    let browsers = installed_browsers();
    let index = match browsers.as_slice() {
        [] => bail!(
            "no supported browser found; install Chrome/Chromium or Firefox, or set CHROME / FIREFOX"
        ),
        _ => select_browser(&browsers)?,
    };
    let browser = browsers
        .get(index)
        .context("selected browser index out of range")?;

    let mut session =
        Session::launch(&LaunchOptions::for_browser(browser, args.login_url))?;

    let cookies = match &args.trigger {
        Trigger::Manual => {
            eprintln!(
                "Log in in the browser window, then press Enter here to capture cookies..."
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            session.cookies()?
        }
        Trigger::WaitForCookie(name) => {
            eprintln!("Log in in the browser window; waiting for cookie `{name}`...");
            session.wait_for_cookie(name, CAPTURE_TIMEOUT)?
        }
    };

    match &args.trigger {
        Trigger::WaitForCookie(name) => {
            let cookie = cookies
                .iter()
                .find(|c| &c.name == name)
                .context("captured set did not contain the target cookie")?;
            println!("{}", cookie.value);
        }
        Trigger::Manual => println!("{}", serde_json::to_string_pretty(&cookies)?),
    }
    Ok(())
}
