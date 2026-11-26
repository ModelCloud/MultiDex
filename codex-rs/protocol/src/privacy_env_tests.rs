use std::collections::HashMap;
use std::ffi::OsString;
use std::process::Command;

use pretty_assertions::assert_eq;

use super::*;

fn privacy_value(name: &str) -> &'static str {
    PRIVACY_ENV_VARS
        .iter()
        .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
        .unwrap_or_else(|| panic!("{name} is missing from PRIVACY_ENV_VARS"))
}

#[test]
fn required_opt_outs_are_forced() {
    assert_eq!(privacy_value(DO_NOT_TRACK_ENV_VAR), "1");
    assert_eq!(privacy_value(DISABLE_TELEMETRY_ENV_VAR), "1");
    assert_eq!(
        privacy_value("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        "1"
    );
    assert_eq!(privacy_value("CLAUDE_CODE_DISABLE_ERROR_REPORTING"), "1");
}

#[test]
fn insert_privacy_env_overrides_inherited_values() {
    let mut env = HashMap::from([
        (DO_NOT_TRACK_ENV_VAR.to_string(), "0".to_string()),
        ("KEEP_ME".to_string(), "value".to_string()),
    ]);

    insert_privacy_env(&mut env);

    assert_eq!(env.get(DO_NOT_TRACK_ENV_VAR), Some(&"1".to_string()));
    assert_eq!(
        env.get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        Some(&"1".to_string())
    );
    assert_eq!(env.get("KEEP_ME"), Some(&"value".to_string()));
}

#[test]
fn insert_privacy_env_os_overrides_inherited_values() {
    let mut env = HashMap::from([(
        OsString::from(DISABLE_TELEMETRY_ENV_VAR),
        OsString::from("0"),
    )]);

    insert_privacy_env_os(&mut env);

    assert_eq!(
        env.get(&OsString::from(DISABLE_TELEMETRY_ENV_VAR)),
        Some(&OsString::from("1"))
    );
}

#[test]
fn apply_privacy_env_overrides_command_env() {
    let mut command = Command::new("env");
    command.env(DO_NOT_TRACK_ENV_VAR, "0");

    apply_privacy_env(&mut command);

    let configured: HashMap<OsString, OsString> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_os_string(),
                value.unwrap_or_default().to_os_string(),
            )
        })
        .collect();
    assert_eq!(
        configured.get(&OsString::from(DO_NOT_TRACK_ENV_VAR)),
        Some(&OsString::from("1"))
    );
    assert_eq!(
        configured.get(&OsString::from("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")),
        Some(&OsString::from("1"))
    );
}

#[test]
fn shell_environment_carries_the_opt_outs() {
    use crate::config_types::ShellEnvironmentPolicy;
    use crate::shell_environment::create_env_from_vars;

    let env = create_env_from_vars(
        [
            ("PATH".to_string(), "/usr/bin".to_string()),
            (DO_NOT_TRACK_ENV_VAR.to_string(), "0".to_string()),
        ],
        &ShellEnvironmentPolicy::default(),
        /*thread_id*/ None,
    );

    assert_eq!(env.get(DO_NOT_TRACK_ENV_VAR), Some(&"1".to_string()));
    assert_eq!(
        env.get("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        Some(&"1".to_string())
    );
}

#[test]
fn install_privacy_env_forces_the_process_environment() {
    // The process-level install is what covers spawn paths that inherit the
    // environment instead of building an explicit map (exec helpers, sandbox
    // wrappers). It must overwrite an inherited opt-out.
    let previous = std::env::var_os(DO_NOT_TRACK_ENV_VAR);
    // SAFETY: only this test mutates these variables.
    unsafe { std::env::set_var(DO_NOT_TRACK_ENV_VAR, "0") };

    install_privacy_env();

    assert_eq!(
        std::env::var(DO_NOT_TRACK_ENV_VAR).as_deref(),
        Ok("1"),
        "install_privacy_env must overwrite an inherited opt-out"
    );
    assert_eq!(
        std::env::var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").as_deref(),
        Ok("1")
    );

    match previous {
        Some(value) => unsafe { std::env::set_var(DO_NOT_TRACK_ENV_VAR, value) },
        None => unsafe { std::env::remove_var(DO_NOT_TRACK_ENV_VAR) },
    }
}
