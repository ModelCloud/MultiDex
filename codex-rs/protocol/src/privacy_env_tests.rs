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
