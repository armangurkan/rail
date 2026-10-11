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

/// Characters systemd refuses in a program path ("Executable path contains special
/// characters"), even quoted: quotes, backslash and glob characters.
pub const EXEC_REFUSED: [char; 6] = ['"', '\'', '\\', '*', '?', '['];

/// `value` in double quotes for a unit file: `\` and `"` escaped, `%` doubled so systemd
/// expands no specifier. `$` is left alone: `Environment=` never expands variables, and
/// `ExecStart=` does not expand them in the program path (the only place Rail puts a path).
/// Control characters are refused.
pub fn quote(value: &str) -> color_eyre::Result<String> {
    if value.chars().any(char::is_control) {
        bail!("a value for the login unit contains a control character");
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    Ok(format!("\"{escaped}\""))
}

/// The program path for `ExecStart=`, refused when systemd would refuse it.
fn exec_path(setup: &Setup) -> color_eyre::Result<String> {
    let exe = text_of(&setup.exe)?;
    if exe.contains(EXEC_REFUSED) {
        bail!(
            "systemd cannot run a program whose path contains any of {}: {exe}\nhint: move the rail binary to a directory without them and run `rail daemon install` again",
            EXEC_REFUSED
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    quote(&exe)
}

pub fn unit(setup: &Setup) -> color_eyre::Result<Unit> {
    let dir = setup.config_home.join("systemd").join("user");
    let mut text = String::from(
        "# Written by `rail daemon install`; `rail daemon uninstall` removes it.\n\
         [Unit]\nDescription=Rail mail daemon: keeps the local mail store in sync\n\
         StartLimitIntervalSec=300\nStartLimitBurst=5\n\n[Service]\nType=simple\n",
    );
    text.push_str(&format!("ExecStart={} daemon run\n", exec_path(setup)?));
    text.push_str("Restart=on-failure\nRestartSec=5\n");
    for (key, value) in environment(setup)? {
        text.push_str(&format!(
            "Environment={}\n",
            quote(&format!("{key}={value}"))?
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
        status: Some(cmd(&[
            "systemctl",
            "--user",
            "--no-pager",
            "status",
            UNIT_NAME,
        ])),
        main_pid: Some(cmd(&[
            "systemctl",
            "--user",
            "show",
            "--property=MainPID",
            "--value",
            UNIT_NAME,
        ])),
        start_hint: format!(
            "`journalctl --user -u {UNIT_NAME}` and `rail doctor` say why; `systemctl --user restart {UNIT_NAME}` tries again"
        ),
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
            "\"/a b/100%%/$HOME/\\\"x\\\"\\\\\""
        );
        assert!(quote("/bad\npath").is_err());
        assert!(unit(&setup("/opt/ra\nil", None)).is_err());
        assert!(unit(&setup("/opt/rail", Some("/opt/\u{1b}[31m"))).is_err());
        assert!(unit(&setup_in("/home/me/a\tb", "/opt/rail", None)).is_err());
    }

    /// Both settings expand specifiers (`%`) but neither expands variables where Rail puts a
    /// path, so `$` stays single.
    #[test]
    fn a_directory_with_a_space_a_percent_a_quote_and_a_dollar_is_escaped_for_each_setting() {
        let root = "/home/me/my \"mail\" 100% $HOME";
        let unit = unit(&setup_in(root, "/opt/my rail/100%/$x/rail", None)).unwrap();
        let text = &unit.text;
        assert!(
            text.contains("ExecStart=\"/opt/my rail/100%%/$x/rail\" daemon run\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "Environment=\"RAIL_DATA_DIR=/home/me/my \\\"mail\\\" 100%% $HOME/data\"\n"
            ),
            "{text}"
        );
        assert_eq!(quote("a\\b").unwrap(), "\"a\\\\b\"");
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

    #[test]
    fn a_program_path_systemd_would_refuse_is_refused_with_a_hint() {
        for exe in [
            "/opt/a\"b/rail",
            "/opt/a'b/rail",
            "/opt/a\\b/rail",
            "/opt/a*/rail",
            "/opt/a?/rail",
            "/opt/a[1/rail",
        ] {
            let error = unit(&setup(exe, None)).err().unwrap().to_string();
            assert!(error.contains("systemd cannot run"), "{exe}: {error}");
            assert!(error.contains("hint: "), "{error}");
        }
    }

    /// systemd's own parser, offline (`systemd-analyze verify` loads the file and checks the
    /// program exists; it starts nothing). Skipped where `systemd-analyze` is missing.
    #[test]
    fn systemd_accepts_the_unit_for_a_binary_under_a_dollar_a_space_and_a_percent() {
        if std::process::Command::new("systemd-analyze")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("a$b 100% x");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("rail");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let root = dir.path().join("my \"mail\" 50% $HOME");
        let mut setup = setup_in(root.to_str().unwrap(), exe.to_str().unwrap(), None);
        setup.config_home = dir.path().join("xdg");
        let unit = unit(&setup).unwrap();
        assert!(unit.text.contains("a$b 100%% x/rail"), "{}", unit.text);
        crate::cmd::service::flow::write_unit_file(&unit.path, &unit.text).unwrap();
        let out = std::process::Command::new("systemd-analyze")
            .args(["verify", "--man=no"])
            .arg(&unit.path)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}\n{}", unit.text);
        assert!(!stderr.contains("rail.service:"), "{stderr}");
    }
}
