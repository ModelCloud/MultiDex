//! Privacy-by-default environment for every process MultiDex launches.
//!
//! Agent CLIs, MCP servers, and shell commands emit usage telemetry, crash
//! reports, and update pings unless they see a well-known opt-out variable. A
//! session must not depend on the operator exporting those variables, so every
//! child environment is forced to carry them.
//!
//! The values are forced rather than defaulted: an inherited `DO_NOT_TRACK=0`
//! from a shell profile must not switch telemetry back on for a session. Add a
//! new tool to [`PRIVACY_ENV_VARS`] instead of at its call site, so every launch
//! path picks it up.

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Command;

/// Cross-vendor telemetry opt-out, honored by tools that read the convention.
pub const DO_NOT_TRACK_ENV_VAR: &str = "DO_NOT_TRACK";

/// Second cross-vendor spelling some tools read instead of `DO_NOT_TRACK`.
pub const DISABLE_TELEMETRY_ENV_VAR: &str = "DISABLE_TELEMETRY";

/// Opt-out variables forced into every child process environment.
///
/// The first two entries are the cross-vendor convention. The rest are the
/// per-tool spellings that ignore it: Claude Code's nonessential-traffic
/// switches, the Node toolchain's analytics flags, and the update or analytics
/// notices of CLIs agents commonly invoke.
pub const PRIVACY_ENV_VARS: &[(&str, &str)] = &[
    (DO_NOT_TRACK_ENV_VAR, "1"),
    (DISABLE_TELEMETRY_ENV_VAR, "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_AUTOUPDATER", "1"),
    ("DISABLE_BUG_COMMAND", "1"),
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("CLAUDE_CODE_DISABLE_ERROR_REPORTING", "1"),
    ("NEXT_TELEMETRY_DISABLED", "1"),
    ("TURBO_TELEMETRY_DISABLED", "1"),
    ("GATSBY_TELEMETRY_DISABLED", "1"),
    ("ASTRO_TELEMETRY_DISABLED", "1"),
    ("NG_CLI_ANALYTICS", "false"),
    ("HF_HUB_DISABLE_TELEMETRY", "1"),
    ("DOTNET_CLI_TELEMETRY_OPTOUT", "1"),
    ("POWERSHELL_TELEMETRY_OPTOUT", "1"),
    ("SAM_CLI_TELEMETRY", "0"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
];

/// Force every privacy opt-out into a string-keyed child environment.
pub fn insert_privacy_env(env: &mut HashMap<String, String>) {
    for (name, value) in PRIVACY_ENV_VARS {
        env.insert((*name).to_string(), (*value).to_string());
    }
}

/// Force every privacy opt-out into an OS-string-keyed child environment.
///
/// MCP servers are launched with `OsString` keys because their configured env
/// may carry non-UTF-8 names.
pub fn insert_privacy_env_os(env: &mut HashMap<OsString, OsString>) {
    for (name, value) in PRIVACY_ENV_VARS {
        env.insert(OsString::from(name), OsString::from(value));
    }
}

/// Force every privacy opt-out into this process's own environment.
///
/// Call once during start-up, before worker threads spawn, so descendants that
/// inherit the process environment (an `exec` helper, a `Command` built without
/// an explicit map) cannot re-enable telemetry.
pub fn install_privacy_env() {
    for (name, value) in PRIVACY_ENV_VARS {
        // SAFETY: intended for single-threaded start-up, before any child or
        // worker thread can observe the environment.
        unsafe { std::env::set_var(name, value) };
    }
}

/// Force every privacy opt-out onto a command, overriding inherited values.
pub fn apply_privacy_env(command: &mut Command) {
    for (name, value) in PRIVACY_ENV_VARS {
        command.env(name, value);
    }
}

fn env_var_is_truthy(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes"
        )
    })
}

#[cfg(test)]
#[path = "privacy_env_tests.rs"]
mod tests;
