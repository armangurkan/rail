use super::scheme::{COUNT, Rgb, Scheme, nearest, parse_table, scheme, schemes};
use ratatui::style::Color;

#[test]
fn the_vendored_table_parses_whole() {
    let all = schemes();
    assert_eq!(all.len(), COUNT);
    let mut names: Vec<String> = all.iter().map(|s| s.name.to_ascii_lowercase()).collect();
    names.dedup();
    assert_eq!(names.len(), COUNT, "names are unique ignoring case");
    let sorted = names.windows(2).all(|w| w[0] <= w[1]);
    assert!(sorted, "sorted case-insensitively");
    assert!(all.iter().all(|s| !s.name.is_empty() && s.name.is_ascii()));
}

#[test]
fn dracula_has_its_known_colours() {
    let d = scheme("dracula").expect("Dracula is in the table");
    assert_eq!(d.name, "Dracula");
    assert_eq!(d.background, Rgb(0x28, 0x2a, 0x36));
    assert_eq!(d.foreground, Rgb(0xf8, 0xf8, 0xf2));
    assert_eq!(d.selection_bg, Rgb(0x44, 0x47, 0x5a));
    assert_eq!(d.palette[6], Rgb(0x8b, 0xe9, 0xfd));
    assert_eq!(d.palette[6].color(), Color::Rgb(0x8b, 0xe9, 0xfd));
}

#[test]
fn nearest_suggests_a_close_name_and_nothing_for_nonsense() {
    assert_eq!(nearest("Drakula"), Some("Dracula"));
    assert_eq!(nearest("dracula"), Some("Dracula"));
    assert_eq!(nearest("qqqqqqqqqqqq"), None);
    assert_eq!(scheme("no such scheme"), None);
}

#[test]
fn rgb_parses_six_hex_digits_only() {
    assert_eq!(Rgb::parse("#00ff7F"), Some(Rgb(0, 255, 127)));
    assert_eq!(Rgb::parse("00ff7f"), None);
    assert_eq!(Rgb::parse("#00ff7"), None);
    assert_eq!(Rgb::parse("#00ff7fff"), None);
    assert_eq!(Rgb::parse("#gg0000"), None);
}

#[test]
fn a_malformed_table_names_its_line() {
    let good = format!("Good\t{}", vec!["#000000"; 20].join("\t"));
    let text: &'static str = Box::leak(format!("# c\n{good}\nBad\t#000000\n").into_boxed_str());
    let err = parse_table(text).unwrap_err();
    assert!(err.contains("line 3"), "{err}");
    let one: Vec<Scheme> = parse_table(&text[..text.rfind("Bad").unwrap()]).unwrap();
    // `parse_table` takes `&'static str`: the leaked box above makes the slice 'static too.
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].name, "Good");
}
