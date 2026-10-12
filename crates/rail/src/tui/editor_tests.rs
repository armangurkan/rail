use super::editor::{command_line, run};

fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        vars.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_string())
    }
}

#[test]
fn the_editor_comes_from_config_then_visual_then_editor_then_vi() {
    assert_eq!(
        command_line(Some("code --wait"), env(&[("VISUAL", "nvim")])),
        ["code", "--wait"]
    );
    assert_eq!(
        command_line(None, env(&[("VISUAL", "nvim"), ("EDITOR", "vi")])),
        ["nvim"]
    );
    assert_eq!(
        command_line(None, env(&[("EDITOR", "emacs -nw")])),
        ["emacs", "-nw"]
    );
    assert_eq!(command_line(None, env(&[])), ["vi"]);
    assert_eq!(
        command_line(Some("  "), env(&[("EDITOR", "")])),
        ["vi"],
        "blank values are ignored"
    );
    assert_eq!(
        command_line(Some("'/opt/My Editor/bin/ed' -f"), env(&[])),
        ["/opt/My Editor/bin/ed", "-f"],
        "shell quoting, no shell"
    );
    assert_eq!(
        command_line(Some("bad 'quote"), env(&[("EDITOR", "vi")])),
        ["vi"],
        "an unparseable command falls back"
    );
}

#[cfg(unix)]
#[test]
fn run_editor_returns_the_status_and_leaves_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("draft.eml");
    std::fs::write(&file, "To: a@b.c\n\nbody\n").unwrap();
    let command: Vec<String> = ["/bin/sh", "-c", "printf 'more\\n' >> \"$0\"; exit 3"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(run(&command, &file).unwrap(), 3);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "To: a@b.c\n\nbody\nmore\n"
    );
    let missing: Vec<String> = vec!["/no/such/editor".into()];
    let error = run(&missing, &file).unwrap_err();
    assert!(error.contains("cannot run"), "{error}");
    assert!(
        !error.contains("draft.eml"),
        "the path is not echoed: {error}"
    );
}

#[tokio::test]
async fn settle_runs_the_fake_editor_on_the_path_and_reports_back() {
    use crate::tui::app::Effect;
    use crate::tui::test_util::{Editor, Hello, settle_capturing};
    use std::sync::{Arc, Mutex};

    let editor = Editor::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    editor.set(move |path| {
        log.lock().unwrap().push(path.to_path_buf());
        Err("no editor today".to_string())
    });
    let mut screen = Hello::default();
    let effects = vec![Effect::RunEditor {
        command: vec!["ignored".to_string()],
        path: std::path::PathBuf::from("/tmp/draft.eml"),
    }];
    settle_capturing(&mut screen, effects, &Arc::new(Mutex::new(None)), &editor).await;
    assert_eq!(
        *seen.lock().unwrap(),
        [std::path::PathBuf::from("/tmp/draft.eml")]
    );
    assert_eq!(screen.edited, vec![Err("no editor today".to_string())]);
    let effects = vec![Effect::RunEditor {
        command: vec![],
        path: std::path::PathBuf::from("/x"),
    }];
    let quiet = Editor::default();
    settle_capturing(&mut screen, effects, &Arc::new(Mutex::new(None)), &quiet).await;
    assert_eq!(screen.edited.last(), Some(&Ok(0)), "the default exits 0");
}
