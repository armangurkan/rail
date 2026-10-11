//! The launchd agent: `RunAtLoad`, restarted when it exits on a failure, with only Rail's
//! environment.

use super::{Setup, Unit, cmd, environment, text_of};

pub const LABEL: &str = "io.github.vitosans.rail";

/// `text` escaped for XML character data and attributes.
pub fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn unit(setup: &Setup) -> color_eyre::Result<Unit> {
    let path = setup
        .home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"));
    let mut env = String::new();
    for (key, value) in environment(setup)? {
        env.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            xml(&key),
            xml(&value)
        ));
    }
    let exe = xml(&text_of(&setup.exe)?);
    let text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by `rail daemon install`; `rail daemon uninstall` removes it. -->\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n\
         \x20 <key>ProgramArguments</key>\n  <array>\n    <string>{exe}</string>\n    <string>daemon</string>\n    <string>run</string>\n  </array>\n\
         \x20 <key>EnvironmentVariables</key>\n  <dict>\n{env}  </dict>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n\
         \x20 <key>ThrottleInterval</key>\n  <integer>10</integer>\n</dict>\n</plist>\n"
    );
    let domain = format!("gui/{}", setup.uid);
    let shown = text_of(&path)?;
    Ok(Unit {
        name: format!("{LABEL}.plist"),
        enabled_marker: None,
        probe: None,
        before_install: vec![cmd(&["launchctl", "bootout", &format!("{domain}/{LABEL}")])],
        after_install: vec![cmd(&["launchctl", "bootstrap", &domain, &shown])],
        before_remove: vec![cmd(&["launchctl", "bootout", &format!("{domain}/{LABEL}")])],
        after_remove: Vec::new(),
        status: Some(cmd(&["launchctl", "print", &format!("{domain}/{LABEL}")])),
        main_pid: None,
        start_hint: format!(
            "`rail doctor` and Rail's log (rail.log) say why; `launchctl kickstart -k {domain}/{LABEL}` tries again"
        ),
        path,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::service::Setup;
    use rail_core::paths::Paths;

    fn setup(exe: &str, root: &str) -> Setup {
        Setup {
            exe: exe.into(),
            paths: Paths::all_in(std::path::Path::new(root)),
            op_dir: None,
            home: "/Users/me".into(),
            config_home: "/Users/me/.config".into(),
            uid: 501,
        }
    }

    #[test]
    fn the_agent_runs_at_load_keeps_alive_on_failure_and_sets_only_rails_environment() {
        let unit = unit(&setup(
            "/Applications/R&D/rail",
            "/Users/me/Library/Application Support/rail",
        ))
        .unwrap();
        assert_eq!(
            unit.path,
            std::path::Path::new("/Users/me/Library/LaunchAgents/io.github.vitosans.rail.plist")
        );
        let text = &unit.text;
        assert!(
            text.contains("<string>/Applications/R&amp;D/rail</string>"),
            "{text}"
        );
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"), "{text}");
        assert!(
            text.contains("<key>SuccessfulExit</key>\n    <false/>"),
            "{text}"
        );
        assert!(text.contains("<key>RAIL_DATA_DIR</key>"), "{text}");
        assert_eq!(text.matches("<key>RAIL_").count(), 5);
        assert!(!text.contains("DISPLAY"), "{text}");
        assert_eq!(
            unit.after_install[0].join(" "),
            format!("launchctl bootstrap gui/501 {}", unit.path.display())
        );
        assert_eq!(
            unit.before_install[0].join(" "),
            "launchctl bootout gui/501/io.github.vitosans.rail"
        );
        assert!(unit.probe.is_none());
        assert_eq!(
            xml("<a & 'b' \"c\">"),
            "&lt;a &amp; &apos;b&apos; &quot;c&quot;&gt;"
        );
    }

    #[test]
    fn a_directory_with_a_space_a_percent_and_a_quote_is_xml_escaped() {
        let agent = unit(&setup("/opt/rail", "/Users/me/my \"mail\" 100% <x>")).unwrap();
        assert!(
            agent
                .text
                .contains("<string>/Users/me/my &quot;mail&quot; 100% &lt;x&gt;/config</string>"),
            "{}",
            agent.text
        );
        assert!(unit(&setup("/opt/ra\nil", "/Users/me")).is_err());
        assert!(unit(&setup("/opt/rail", "/Users/me/\u{7}")).is_err());
    }
}
