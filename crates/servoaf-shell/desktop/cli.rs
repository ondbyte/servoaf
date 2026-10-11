/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::{env, panic};

use crate::desktop::app::App;
use crate::desktop::event_loop::ServoShellEventLoop;
use crate::panic_hook;
use crate::prefs::{ArgumentParsingResult, parse_command_line_arguments};

pub fn main() {
    use_utf8_compose_locale(); // servoaf
    crate::crash_handler::install();
    crate::init_crypto();

    // TODO: once log-panics is released, can this be replaced by
    // log_panics::init()?
    panic::set_hook(Box::new(panic_hook::panic_hook));

    // Skip the first argument, which is the binary name.
    let args: Vec<String> = env::args().skip(1).collect();
    let (opts, preferences, servoshell_preferences) = match parse_command_line_arguments(&*args) {
        #[cfg(not(feature = "multiprocess"))]
        ArgumentParsingResult::ContentProcess(_) => {
            log::error!("IPC not enabled, cannot use contentprocesses");
            return;
        },
        #[cfg(feature = "multiprocess")]
        ArgumentParsingResult::ContentProcess(token) => {
            return servo::run_content_process(token);
        },
        ArgumentParsingResult::ChromeProcess(opts, preferences, servoshell_preferences) => {
            (opts, preferences, servoshell_preferences)
        },
        ArgumentParsingResult::Exit => {
            std::process::exit(0);
        },
        ArgumentParsingResult::ErrorParsing => {
            std::process::exit(1);
        },
    };

    crate::init_tracing(servoshell_preferences.tracing_filter.as_deref());

    let clean_shutdown = servoshell_preferences.clean_shutdown;
    let event_loop = match servoshell_preferences.headless {
        true => ServoShellEventLoop::headless(),
        false => ServoShellEventLoop::headed(),
    };

    {
        let mut app = App::new(opts, preferences, servoshell_preferences, &event_loop);
        event_loop.run_app(&mut app);
    }

    crate::platform::deinit(clean_shutdown)
}

/// servoaf: the locale libxkbcommon uses for the keyboard Compose table, if
/// not a UTF-8 one, gets a UTF-8 replacement for this process.
///
/// winit loads the Compose (dead-key) table for `LC_ALL`, else `LC_CTYPE`, else
/// `LANG`. A non-UTF-8 locale name such as `en_US` maps to
/// `iso8859-1/Compose`, which current libxkbcommon refuses to parse, printing a
/// screen of "xkbcommon: ERROR" lines and disabling dead keys. Servo takes the
/// page language from `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG`, so overriding
/// `LC_CTYPE` (or only the charset of `LC_ALL`) leaves `navigator.language`
/// unchanged.
fn use_utf8_compose_locale() {
    let get = |name: &str| env::var(name).ok().filter(|value| !value.is_empty());
    let (variable, value) = match get("LC_ALL") {
        Some(value) => ("LC_ALL", value),
        None => match get("LC_CTYPE").or_else(|| get("LANG")) {
            Some(value) => ("LC_CTYPE", value),
            None => return, // "C": xkbcommon handles it.
        },
    };
    let lower = value.to_ascii_lowercase();
    if lower == "c" || lower == "posix" || lower.contains("utf-8") || lower.contains("utf8") {
        return;
    }
    // en_US.ISO-8859-1@euro -> en_US.UTF-8, if libX11 knows that locale.
    let base = value.split(['.', '@']).next().unwrap_or_default();
    let candidate = format!("{base}.UTF-8");
    let known = std::fs::read_to_string("/usr/share/X11/locale/compose.dir")
        .map(|dir| {
            dir.lines()
                .any(|line| line.split_whitespace().nth(1) == Some(candidate.as_str()))
        })
        .unwrap_or(false);
    let replacement = if known {
        candidate
    } else {
        "C.UTF-8".to_owned()
    };
    // SAFETY: called first thing in main(), before any other thread exists.
    unsafe { env::set_var(variable, &replacement) };
}
