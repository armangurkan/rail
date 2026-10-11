//! The systemd user unit.

use color_eyre::eyre::bail;

use super::{Setup, Unit, cmd, environment, text_of};

pub const UNIT_NAME: &str = "rail.service";

/// Variables a user manager often holds (imported from the graphical session or set by
/// `environment.d`) that the daemon must not inherit: a display and browser (each login a
/// client begins sends its own), a log filter, and 1Password CLI settings that would point
/// `op` at another account or a service account. `UnsetEnvironment=` takes names only, so
/// `OP_SESSION_<account>` (named after each account) cannot be listed.
pub const UNSET: [&str; 11] = [
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "BROWSER",
    "RUST_LOG",
    "OP_ACCOUNT",
    "OP_SERVICE_ACCOUNT_TOKEN",
    "OP_CONNECT_HOST",
    "OP_CONNECT_TOKEN",
    "OP_CONFIG_DIR",
    "OP_BIOMETRIC_UNLOCK_ENABLED",
];

fn refuse_control(value: &str) -> color_eyre::Result<()> {
    if value.chars().any(char::is_control) {
        bail!("a value for the login unit contains a control character");
    }
    Ok(())
}

/// `\`, `"` escaped and `%` doubled (no specifier expansion), in double quotes.
fn quoted(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// `value` in double quotes for `ExecStart=`: `\` and `"` escaped, `%` and `$` doubled so
/// systemd expands neither. Control characters are refused.
pub fn quote(value: &str) -> color_eyre::Result<String> {
    refuse_control(value)?;
    Ok(quoted(&value.replace('$', "$$")))
}

/// `value` in double quotes for `Environment=`, which expands specifiers but not variables:
/// as `quote`, with `$` left alone.
pub fn quote_env(value: &str) -> color_eyre::Result<String> {
    refuse_control(value)?;
    Ok(quoted(value))
}

pub fn unit(setup: &Setup) -> color_eyre::Result<Unit> {
    let dir = setup.config_home.join("systemd").join("user");
    let mut text = String::from(
        "# Written by `rail daemon install`; `rail daemon uninstall` removes it.\n\
         [Unit]\nDescription=Rail mail daemon: keeps the local mail store in sync\n\
         StartLimitIntervalSec=300\nStartLimitBurst=5\n\n[Service]\nType=simple\n",
    );
    text.push_str(&format!(
        "ExecStart={} daemon run\n",
        quote(&text_of(&setup.exe)?)?
    ));
    text.push_str("Restart=on-failure\nRestartSec=5\n");
    for (key, value) in environment(setup)? {
        text.push_str(&format!(
            "Environment={}\n",
            quote_env(&format!("{key}={value}"))?
        ));
    }
    text.push_str(&format!("UnsetEnvironment={}\n", UNSET.join(" ")));
    text.push_str("\n[Install]\nWantedBy=default.target\n");
    Ok(Unit {
        name: UNIT_NAME.to_string(),
        path: dir.join(UNIT_NAME),
        text,
        enabled_marker: Some(dir.join("default.target.wants").join(UNIT_NAME)),
        probe: Some(cmd(&["systemctl", "--user", "show", "--property=Version"])),
        before_install: Vec::new(),
        after_install: vec![
            cmd(&["systemctl", "--user", "daemon-reload"]),
            cmd(&["systemctl", "--user", "enable", "--now", UNIT_NAME]),
        ],
        before_remove: vec![cmd(&["systemctl", "--user", "disable", "--now", UNIT_NAME])],
        after_remove: vec![cmd(&["systemctl", "--user", "daemon-reload"])],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::service::Setup;
    use rail_core::paths::Paths;

    fn setup(exe: &str, op_dir: Option<&str>) -> Setup {
        setup_in("/home/me/rail dirs", exe, op_dir)
    }

    fn setup_in(root: &str, exe: &str, op_dir: Option<&str>) -> Setup {
        Setup {
            exe: exe.into(),
            paths: Paths::all_in(std::path::Path::new(root)),
            op_dir: op_dir.map(Into::into),
            home: "/home/me".into(),
            config_home: "/home/me/.config".into(),
            uid: 1000,
        }
    }

    #[test]
    fn the_unit_runs_this_binary_with_rails_directories_and_a_plain_path() {
        let unit = unit(&setup("/opt/rail/bin/rail", Some("/opt/1password"))).unwrap();
        assert_eq!(
            unit.path,
            std::path::Path::new("/home/me/.config/systemd/user/rail.service")
        );
        let text = &unit.text;
        assert!(
            text.contains("ExecStart=\"/opt/rail/bin/rail\" daemon run\n"),
            "{text}"
        );
        assert!(
            text.contains("Restart=on-failure\n") && text.contains("StartLimitBurst=5\n"),
            "{text}"
        );
        assert!(
            text.contains("Environment=\"RAIL_CONFIG_DIR=/home/me/rail dirs/config\"\n"),
            "{text}"
        );
        assert!(
            text.contains("Environment=\"RAIL_RUNTIME_DIR=/home/me/rail dirs/run\"\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "Environment=\"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/1password\"\n"
            ),
            "{text}"
        );
        assert!(text.contains("WantedBy=default.target"), "{text}");
        let commands: Vec<String> = unit.after_install.iter().map(|c| c.join(" ")).collect();
        assert_eq!(
            commands,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now rail.service"
            ]
        );
        assert_eq!(
            unit.before_remove[0].join(" "),
            "systemctl --user disable --now rail.service"
        );
        assert_eq!(
            unit.probe.as_ref().map(|c| c.join(" ")).as_deref(),
            Some("systemctl --user show --property=Version")
        );
    }

    #[test]
    fn values_are_quoted_for_systemd_and_control_characters_refused() {
        assert_eq!(
            quote("/a b/100%/$HOME/\"x\"\\").unwrap(),
            "\"/a b/100%%/$$HOME/\\\"x\\\"\\\\\""
        );
        assert!(quote("/bad\npath").is_err());
        assert!(unit(&setup("/opt/ra\nil", None)).is_err());
        assert!(unit(&setup("/opt/rail", Some("/opt/\u{1b}[31m"))).is_err());
        assert!(unit(&setup_in("/home/me/a\tb", "/opt/rail", None)).is_err());
    }

    /// `Environment=` expands specifiers but not variables, so `$` stays single there while
    /// `ExecStart=` (which expands both) doubles it.
    #[test]
    fn a_directory_with_a_space_a_percent_a_quote_and_a_dollar_is_escaped_for_each_setting() {
        let root = "/home/me/my \"mail\" 100% $HOME";
        let unit = unit(&setup_in(root, "/opt/my rail/100%/$x/rail", None)).unwrap();
        let text = &unit.text;
        assert!(
            text.contains("ExecStart=\"/opt/my rail/100%%/$$x/rail\" daemon run\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "Environment=\"RAIL_DATA_DIR=/home/me/my \\\"mail\\\" 100%% $HOME/data\"\n"
            ),
            "{text}"
        );
        assert_eq!(quote_env("a\\b").unwrap(), "\"a\\\\b\"");
        assert!(quote_env("a\nb").is_err());
    }

    #[test]
    fn the_unit_unsets_the_session_variables_the_user_manager_may_hold() {
        let unit = unit(&setup("/opt/rail", None)).unwrap();
        let line = unit
            .text
            .lines()
            .find(|l| l.starts_with("UnsetEnvironment="))
            .unwrap_or_default()
            .to_string();
        for name in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "BROWSER",
            "OP_SERVICE_ACCOUNT_TOKEN",
        ] {
            assert!(line.split_whitespace().any(|w| w.ends_with(name)), "{line}");
        }
        assert!(!line.contains("PATH") && !line.contains("RAIL_"), "{line}");
        assert!(
            unit.text
                .contains("Environment=\"PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\"\n"),
            "{}",
            unit.text
        );
    }

    #[test]
    fn an_op_directory_with_a_colon_cannot_go_on_path() {
        let error = unit(&setup("/opt/rail", Some("/opt/a:b"))).err().unwrap();
        assert!(error.to_string().contains("':'"), "{error}");
    }
}
