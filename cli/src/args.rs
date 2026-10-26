//! Tiny argument parser.
//!
//! Rules, kept boring on purpose:
//! * the first non-flag word is the command, the rest are positionals;
//! * `--name value` and `--name=value` are equivalent;
//! * `--flag` with no value is a boolean switch (detected by the next word starting with
//!   `-` or being absent);
//! * `-h`, `-V` are short aliases for `--help`, `--version`.

use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub positional: Vec<String>,
    flags: BTreeMap<String, Option<String>>,
}

impl Parsed {
    pub fn flag(&self, name: &str) -> Option<Option<&str>> {
        self.flags.get(name).map(|v| v.as_deref())
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    /// A flag that must carry a value.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }

    /// A boolean switch.
    pub fn switch(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub fn command(&self) -> Option<&str> {
        self.positional.first().map(String::as_str)
    }

    /// Positional arguments after the command.
    pub fn rest(&self) -> &[String] {
        &self.positional[1.min(self.positional.len())..]
    }
}

/// `-1`, `-0.5`, `-2ms` are values, not flags.
fn is_negative_number(word: &str) -> bool {
    word.len() > 1
        && word.starts_with('-')
        && word[1..]
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
}

pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let mut i = 0;
    while i < argv.len() {
        let word = &argv[i];
        if word == "-h" {
            out.flags.insert("help".into(), None);
            i += 1;
            continue;
        }
        if word == "-V" {
            out.flags.insert("version".into(), None);
            i += 1;
            continue;
        }
        if let Some(rest) = word.strip_prefix("--") {
            if rest.is_empty() {
                return Err("`--` is not a flag".into());
            }
            if let Some((name, value)) = rest.split_once('=') {
                if name.is_empty() {
                    return Err(format!("`{word}` has no flag name"));
                }
                out.flags.insert(name.to_string(), Some(value.to_string()));
                i += 1;
                continue;
            }
            // A value is the next word, unless the next word is itself a flag. A negative
            // number is a value: `--limit -1` must reach the command so it can explain
            // itself rather than being rejected as an unknown flag.
            let next_is_value = argv
                .get(i + 1)
                .map(|v| !v.starts_with('-') || v == "-" || is_negative_number(v))
                .unwrap_or(false);
            if next_is_value {
                out.flags
                    .insert(rest.to_string(), Some(argv[i + 1].clone()));
                i += 2;
            } else {
                out.flags.insert(rest.to_string(), None);
                i += 1;
            }
            continue;
        }
        if word.starts_with('-') && word.len() > 1 {
            return Err(format!("unknown flag `{word}`"));
        }
        out.positional.push(word.clone());
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(words: &[&str]) -> Parsed {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn separates_command_positionals_and_flags() {
        let parsed = p(&[
            "memory",
            "recall",
            "token",
            "--focus",
            "file:a.rs",
            "--limit",
            "3",
        ]);
        assert_eq!(parsed.command(), Some("memory"));
        assert_eq!(parsed.rest(), &["recall".to_string(), "token".to_string()]);
        assert_eq!(parsed.value("focus"), Some("file:a.rs"));
        assert_eq!(parsed.value("limit"), Some("3"));
    }

    #[test]
    fn equals_form_is_equivalent() {
        let a = p(&["recall", "--limit=3"]);
        let b = p(&["recall", "--limit", "3"]);
        assert_eq!(a.value("limit"), b.value("limit"));
    }

    #[test]
    fn a_switch_does_not_swallow_the_next_flag() {
        let parsed = p(&["recall", "x", "--json", "--limit", "2"]);
        assert!(parsed.switch("json"));
        assert_eq!(parsed.value("limit"), Some("2"));
    }

    #[test]
    fn a_trailing_switch_is_a_switch() {
        let parsed = p(&["memory", "stats", "--json"]);
        assert!(parsed.switch("json"));
    }

    #[test]
    fn negative_numbers_are_values_not_flags() {
        // `--temperature -1` must parse as a value, not as an unknown flag.
        let parsed = p(&["providers", "show", "x", "--temperature", "-1"]);
        assert_eq!(parsed.value("temperature"), Some("-1"));
    }

    #[test]
    fn short_aliases_work() {
        assert!(p(&["-h"]).switch("help"));
        assert!(p(&["-V"]).switch("version"));
    }

    #[test]
    fn unknown_short_flags_are_rejected() {
        let err = parse(&["-x".to_string()]).unwrap_err();
        assert!(err.contains("unknown flag"), "{err}");
    }

    #[test]
    fn an_empty_flag_name_is_rejected() {
        assert!(parse(&["--=v".to_string()]).is_err());
        assert!(parse(&["--".to_string()]).is_err());
    }
}
