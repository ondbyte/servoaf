// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! servoaf for Linux: a Servo browser window (or a headless instance) with
//! the MCP control server built in.

mod headed;
mod headless;

use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::Receiver;
use servo::{EventLoopWaker, Opts, Servo, ServoBuilder};
use servoaf_core::BrowserConfig;
use servoaf_core::mcp::http::{self, HttpOptions, McpRequest, Wake};
use url::Url;

const USAGE: &str = "\
servoaf — Servo with a built-in MCP control server

usage: servoaf [options] [URL]

  --mcp-port PORT           Serve MCP on http://HOST:PORT/mcp (default 9223; 0 disables)
  --mcp-host HOST           Address to bind (default 127.0.0.1)
  --mcp-token TOKEN         Require 'Authorization: Bearer TOKEN' (or $SERVOAF_MCP_TOKEN)
  --headless                No window; render offscreen (no display needed)
  --size WxH                Window/viewport size in CSS px (default 1280x800)
  --user-data-dir DIR       Keep cookies and storage in DIR (default: temporary)
  --no-cursor-overlay       Don't draw the agent's pointer in the window
  --auto-dismiss-dialogs    Accept alerts, cancel confirm/prompt automatically
  --tool-timeout SECONDS    Give up on a tool call after this long (default 120)
  -h, --help                Show this help

Anyone who can reach the MCP port controls the browser. Keep it on loopback or
use a token.";

/// Set by Ctrl-C / SIGTERM; the event loops exit when they see it.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Exits the event loop on Ctrl-C or SIGTERM so the profile is cleaned up.
pub fn handle_signals(wake: Wake) {
    let result = ctrlc::set_handler(move || {
        if SHUTDOWN.swap(true, Ordering::SeqCst) {
            std::process::exit(130); // Second Ctrl-C: don't wait.
        }
        wake();
    });
    if let Err(error) = result {
        log::warn!("cannot install a signal handler: {error}");
    }
}

/// A fresh profile directory for one run, deleted afterwards.
struct TemporaryProfile(PathBuf);

impl TemporaryProfile {
    fn create() -> Result<Self, String> {
        let suffix: String = std::iter::repeat_with(|| format!("{:x}", rand_nibble())).take(8).collect();
        let path = std::env::temp_dir().join(format!("servoaf-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
        Ok(Self(path))
    }
}

fn rand_nibble() -> u8 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(),
    );
    (hasher.finish() & 0xf) as u8
}

impl Drop for TemporaryProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Options {
    pub mcp: Option<HttpOptions>,
    pub headless: bool,
    pub size: (u32, u32),
    pub user_data_dir: Option<PathBuf>,
    pub start_url: Url,
    pub config: BrowserConfig,
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut port: u16 = 9223;
    let mut host = "127.0.0.1".to_owned();
    let mut token = std::env::var("SERVOAF_MCP_TOKEN").ok().filter(|t| !t.is_empty());
    let mut options = Options {
        mcp: None,
        headless: false,
        size: (1280, 800),
        user_data_dir: None,
        start_url: Url::parse("about:blank").expect("valid"),
        config: BrowserConfig::default(),
    };
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_owned(), Some(value.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value =
            |what: &str| inline.clone().or_else(|| args.next()).ok_or(format!("{name} needs {what}"));
        match name.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            },
            "--mcp-port" => port = value("a port")?.parse().map_err(|_| "invalid --mcp-port")?,
            "--mcp-host" => host = value("a host")?,
            "--mcp-token" => token = Some(value("a token")?),
            "--headless" => options.headless = true,
            "--size" => {
                let size = value("WxH")?;
                let (w, h) = size.split_once('x').ok_or("--size must look like 1280x800")?;
                options.size = (
                    w.parse().map_err(|_| "invalid --size width")?,
                    h.parse().map_err(|_| "invalid --size height")?,
                );
            },
            "--user-data-dir" => options.user_data_dir = Some(PathBuf::from(value("a directory")?)),
            "--no-cursor-overlay" => options.config.cursor_overlay = false,
            "--auto-dismiss-dialogs" => options.config.auto_dismiss_dialogs = true,
            "--tool-timeout" => {
                let seconds: u64 = value("seconds")?.parse().map_err(|_| "invalid --tool-timeout")?;
                options.config.tool_timeout = Duration::from_secs(seconds.max(1));
            },
            _ if name.starts_with('-') => return Err(format!("unknown option {name}")),
            _ => options.start_url = servoaf_core::tools::parse_url(&arg)?,
        }
    }
    if port != 0 {
        options.mcp = Some(HttpOptions { host, port, token });
    }
    Ok(options)
}

/// Starts the MCP server (if enabled) and returns the request queue.
pub fn start_mcp(options: &Options, wake: Wake) -> Result<Receiver<McpRequest>, String> {
    let (sender, receiver) = crossbeam_channel::unbounded();
    if let Some(mcp) = &options.mcp {
        let address = http::start(mcp.clone(), sender, wake)?;
        let auth = if mcp.token.is_some() { " (bearer token required)" } else { "" };
        eprintln!("servoaf: MCP server listening on http://{address}{}{auth}", http::MCP_PATH);
        if !address.ip().is_loopback() && mcp.token.is_none() {
            eprintln!("servoaf: WARNING: MCP is reachable from the network without a token");
        }
    }
    Ok(receiver)
}

pub fn build_servo(options: &Options, waker: Box<dyn EventLoopWaker>) -> Servo {
    let opts = Opts { config_dir: options.user_data_dir.clone(), ..Opts::default() };
    ServoBuilder::default().opts(opts).event_loop_waker(waker).build()
}

pub fn browser_for(
    options: &Options,
    servo: Servo,
    rendering_context: Rc<dyn servo::RenderingContext>,
    scale: f32,
    requests: Receiver<McpRequest>,
) -> Rc<servoaf_core::Browser> {
    let browser = servoaf_core::Browser::new(
        servo,
        rendering_context,
        euclid::Scale::new(scale),
        options.config.clone(),
        servoaf_core::tools::registry(),
        requests,
    );
    browser.create_tab(options.start_url.clone(), true);
    browser
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("error")).init();
    let options = match parse_args() {
        Ok(options) => options,
        Err(error) => {
            eprintln!("servoaf: {error}\n\n{USAGE}");
            return ExitCode::from(2);
        },
    };
    let mut options = options;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut temporary_profile = None;
    match &options.user_data_dir {
        Some(dir) => {
            if let Err(error) = std::fs::create_dir_all(dir) {
                eprintln!("servoaf: cannot create {}: {error}", dir.display());
                return ExitCode::FAILURE;
            }
        },
        None => match TemporaryProfile::create() {
            Ok(profile) => {
                options.user_data_dir = Some(profile.0.clone());
                temporary_profile = Some(profile);
            },
            Err(error) => {
                eprintln!("servoaf: {error}");
                return ExitCode::FAILURE;
            },
        },
    }
    let result = if options.headless { headless::run(options) } else { headed::run(options) };
    drop(temporary_profile);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("servoaf: {error}");
            ExitCode::FAILURE
        },
    }
}
