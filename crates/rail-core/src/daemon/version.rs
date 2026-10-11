//! Which of two Rail versions is newer, so that a client replaces only an older daemon and
//! an older client steps aside instead of stopping its successor.

use std::cmp::Ordering;

/// A parsed `MAJOR.MINOR.PATCH[-PRE][+BUILD]` version. Build metadata is ignored.
#[derive(Debug, PartialEq, Eq)]
struct Version<'a> {
    core: [u64; 3],
    pre: Option<&'a str>,
}

fn parse(text: &str) -> Option<Version<'_>> {
    let text = text.split_once('+').map_or(text, |(version, _)| version);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        Some(_) => return None,
        None => (text, None),
    };
    let mut numbers = core.split('.');
    let mut parsed = [0u64; 3];
    for slot in &mut parsed {
        let part = numbers.next()?;
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if numbers.next().is_some() || pre.is_some_and(|p| !valid_pre(p)) {
        return None;
    }
    Some(Version { core: parsed, pre })
}

/// Semver pre-release identifiers: non-empty, ASCII letters, digits and hyphens only. This
/// also keeps a parsed version safe to quote in Rail's own messages.
fn valid_pre(pre: &str) -> bool {
    pre.split('.')
        .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// Semver precedence of two pre-release strings: identifiers left to right, numeric ones
/// numerically and below alphanumeric ones, a shorter list first when all else is equal.
fn compare_pre(ours: &str, theirs: &str) -> Ordering {
    let mut left = ours.split('.');
    let mut right = theirs.split('.');
    loop {
        let order = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => a.cmp(b),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// How version `a` compares with version `b` in semver precedence; `None` when either does
/// not parse, which callers treat as "different, order unknown".
/// The `MAJOR.MINOR.PATCH` of `text`, or `None` when it does not parse. The only part of a
/// version string from another process that Rail repeats: digits and dots, nothing else.
pub(crate) fn core(text: &str) -> Option<String> {
    let [major, minor, patch] = parse(text)?.core;
    Some(format!("{major}.{minor}.{patch}"))
}

pub(crate) fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parse(a)?, parse(b)?);
    let order = a.core.cmp(&b.core).then_with(|| match (a.pre, b.pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => compare_pre(a, b),
    });
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_is_the_version_without_pre_release_or_build() {
        assert_eq!(core("1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(core("1.2.3-rc.1").as_deref(), Some("1.2.3"));
        assert_eq!(
            core("9.9.9+\u{1b}]52;c;ZXZpbA==\u{7}").as_deref(),
            Some("9.9.9")
        );
        assert_eq!(core("not a version\u{1b}[2J"), None);
    }

    #[test]
    fn numeric_parts_compare_as_numbers() {
        assert_eq!(compare("0.10.0", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(compare("1.0.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare("0.1.0", "0.1.1"), Some(Ordering::Less));
        assert_eq!(compare("2.0.0", "10.0.0"), Some(Ordering::Less));
    }

    #[test]
    fn a_pre_release_sorts_before_its_release() {
        assert_eq!(compare("0.0.0-old", "0.1.0"), Some(Ordering::Less));
        assert_eq!(compare("1.0.0-rc.1", "1.0.0"), Some(Ordering::Less));
        assert_eq!(compare("1.0.0-rc.2", "1.0.0-rc.10"), Some(Ordering::Less));
        assert_eq!(
            compare("1.0.0-alpha", "1.0.0-alpha.1"),
            Some(Ordering::Less)
        );
        assert_eq!(compare("1.0.0-1", "1.0.0-alpha"), Some(Ordering::Less));
        assert_eq!(compare("1.0.0+build.5", "1.0.0"), Some(Ordering::Equal));
    }

    #[test]
    fn anything_else_is_unparsable() {
        for bad in [
            "",
            "1",
            "1.0",
            "1.0.0.0",
            "a.b.c",
            "1.0.0-",
            "1.0.0-a..b",
            "1.-1.0",
            "1.0.0-a\nb",
        ] {
            assert_eq!(compare(bad, "1.0.0"), None, "{bad}");
            assert_eq!(compare("1.0.0", bad), None, "{bad}");
        }
    }
}
