use super::*;

fn style_of(line: &Line<'_>, prefix: &str) -> Style {
    line.spans
        .iter()
        .find(|span| span.content.starts_with(prefix))
        .unwrap()
        .style
}

#[test]
fn dim_read_changes_only_the_requested_parts_of_read_rows() {
    for theme in [
        Theme::default(),
        Theme {
            text: Style::new().fg(Color::White),
            muted: Style::new().fg(Color::Gray),
            ..Theme::default()
        },
    ] {
        for mode in [DimRead::Dot, DimRead::Subject, DimRead::All] {
            let ctx = RowContext {
                dim_read: mode,
                ..context(Some("W"), "11:54")
            };
            let line = thread_line(
                &thread("Read subject", 2, 0),
                &ctx,
                90,
                &theme,
                &Glyphs::of(Icons::Unicode),
            );
            let subject = style_of(&line, "Read subject");
            assert_eq!(
                subject.fg,
                if mode == DimRead::Dot {
                    theme.text.fg
                } else {
                    theme.muted.fg
                },
                "{mode:?}"
            );
            let people = style_of(&line, DANA);
            let expected = if mode == DimRead::All {
                theme.muted.fg
            } else {
                theme.text.fg
            };
            assert_eq!(people.fg, expected, "{mode:?}");
            assert_eq!(
                line.spans.last().unwrap().style.fg,
                expected,
                "count and time: {mode:?}"
            );
            assert_eq!(line.spans[1].style.bg, Some(Color::Cyan), "account badge");
        }
    }
}

#[test]
fn dim_read_preserves_unread_cursor_marks_and_error_styles() {
    let theme = Theme::default();
    let glyphs = Glyphs::of(Icons::Ascii);
    for mode in [DimRead::Dot, DimRead::Subject, DimRead::All] {
        let ctx = RowContext {
            dim_read: mode,
            ..context(None, "11:54")
        };
        let unread = thread_line(&thread("Unread", 1, 1), &ctx, 90, &theme, &glyphs);
        assert!(
            unread
                .spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert_eq!(unread.spans[2].style.fg, theme.text.fg);
        let selected = RowContext {
            selected: true,
            ..ctx
        };
        let line = thread_line(&thread("Read", 1, 0), &selected, 90, &theme, &glyphs);
        assert!(line.spans.iter().all(|span| span.style == theme.selected));
        let failed = RowContext {
            selected: false,
            marked: true,
            failed: Some("Could not archive"),
            ..selected
        };
        let line = thread_line(&thread("Read", 1, 0), &failed, 90, &theme, &glyphs);
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan), "marked read row");
        assert_eq!(line.spans[2].style, theme.error, "errors take precedence");
    }
}
