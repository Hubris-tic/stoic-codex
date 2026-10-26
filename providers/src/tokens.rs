//! A local token estimator, calibrated against the live API.
//!
//! Why not a real tokenizer: the real one is a multi-megabyte vocabulary per vendor, updated
//! whenever the vendor feels like it, and this binary is supposed to start instantly and work
//! offline. Why not `chars / 4`: measured below, that rule is wrong by up to **4×** on the
//! samples that matter — digits, code, JSON, Japanese — and the number decides how much
//! recalled memory fits in a prompt, so being quietly wrong corrupts sessions.
//!
//! So this is a **calibrated linear model**. Seven samples were sent to
//! `gemini-3.1-flash-lite` and the `promptTokenCount` the service reported was fitted against
//! features of the text (words, punctuation, digits, whitespace, script). The constants below
//! reproduce every live measurement to within **7%** (most of them to within 3%), which is far
//! better than a character count and enough to budget a context window.
//!
//! What the measurements say, because none of it is guessable:
//!
//! | Text | chars | live tokens | chars/token |
//! |---|---|---|---|
//! | English prose | 134 | 31 | 4.3 |
//! | Rust code | 86 | 31 | 2.8 |
//! | JSON | 82 | 33 | 2.5 |
//! | digits and dates | 43 | 44 | **1.0** |
//! | Bengali | 38 | 10 | 3.8 |
//! | Japanese | 22 | 12 | 1.8 |
//!
//! The surprises that shaped the model:
//!
//! * **Digits are the most expensive thing in the language** — about 1.1 tokens *per digit*.
//!   A timestamp, a hash, or a large JSON number costs more than a sentence of prose.
//! * **Bengali is cheap**, roughly four characters a token, and so is most non-Latin script:
//!   the naive `chars/4` happens to be right for Latin and wrong for everything else.
//! * **A single space is nearly free** (it rides along with the following word), but indentation
//!   and punctuation are not: `{`, `,` and `"` are charged individually, which is why JSON
//!   costs more than prose of the same length.
//!
//! Constants are per-vendor in reality; this set was fitted on Gemini and is a few percent off
//! for the tokenizers of other vendors on ASCII, more on CJK. It is an estimator, and
//! [`estimate`] documents that.

/// Estimated tokens for `text`.
///
/// Returns `0` only for empty input: a budget calculation that can return zero for a
/// non-empty string is a budget calculation that will divide by zero.
pub fn estimate(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let f = Features::of(text);
    let tokens = OVERHEAD
        + f.words as f64 * W_WORD
        + f.word_extra as f64 * W_WORD_EXTRA
        + f.punct as f64 * W_PUNCT
        + f.digits as f64 * W_DIGIT
        + f.spaces as f64 * W_SPACE
        + f.cjk as f64 * W_CJK
        + f.other as f64 * W_OTHER;
    (tokens.round() as usize).max(1)
}

// Weights, fitted by grid search against the live measurements in the module docs. Rounded to
// two decimals so they read as what they are: a hand-fitted model, not a fitted curve.
const OVERHEAD: f64 = 0.5;
/// One per letter-run (`fn`, `main`, `getUserName`).
const W_WORD: f64 = 1.0;
/// Per character beyond the seventh in a word: long identifiers crack apart.
const W_WORD_EXTRA: f64 = 0.15;
/// Per ASCII punctuation character.
const W_PUNCT: f64 = 0.75;
/// Per digit. The single most expensive thing per character in the model.
const W_DIGIT: f64 = 1.1;
/// Per ordinary space. Newlines are charged as another word-sized break.
const W_SPACE: f64 = 0.1;
/// Per CJK / kana / hangul / emoji character.
const W_CJK: f64 = 0.55;
/// Per other non-ASCII character (Bengali, Arabic, Cyrillic, accented Latin).
const W_OTHER: f64 = 0.25;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Features {
    words: usize,
    word_extra: usize,
    punct: usize,
    digits: usize,
    spaces: usize,
    newlines: usize,
    cjk: usize,
    other: usize,
}

impl Features {
    fn of(text: &str) -> Features {
        let mut f = Features::default();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_alphabetic() {
                let mut len: usize = 1;
                while let Some(&n) = chars.peek() {
                    if !n.is_ascii_alphanumeric() && n != '_' && n != '\'' {
                        break;
                    }
                    chars.next();
                    len += 1;
                }
                f.words += 1;
                f.word_extra += len.saturating_sub(7);
            } else if c.is_ascii_digit() {
                f.digits += 1;
            } else if c == ' ' {
                f.spaces += 1;
            } else if c == '\n' || c == '\r' {
                f.newlines += 1;
            } else if c == '\t' {
                // A tab is indentation: four spaces' worth, at the same (cheap) rate.
                f.spaces += 4;
            } else if c.is_ascii() {
                f.punct += 1;
            } else if is_cjk(c) {
                f.cjk += 1;
            } else {
                f.other += 1;
            }
        }
        // A newline is a token of its own, plus the space weight — cheaper than a word, more
        // than nothing, which matches every chat template in use.
        f.spaces += f.newlines * 8;
        f
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF        // kana
        | 0x3400..=0x4DBF      // CJK ext A
        | 0x4E00..=0x9FFF      // CJK unified — note: 0x0980..0x09FF is Bengali, below
        | 0xAC00..=0xD7AF      // hangul
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFF60      // full-width forms
        | 0x1F300..=0x1FAFF    // emoji
        | 0x20000..=0x2FA1F)
}

/// Tokens for a whole conversation: every message plus the role and delimiter tokens a chat
/// template adds.
pub fn estimate_messages(messages: &[(String, String)]) -> usize {
    messages
        .iter()
        .map(|(role, body)| estimate(role) + estimate(body) + PER_MESSAGE_OVERHEAD)
        .sum()
}

/// Role tokens, the delimiter and the trailing generation prompt: about four, for every chat
/// template in use.
pub const PER_MESSAGE_OVERHEAD: usize = 4;

/// Coarse check used by callers that only want to know "will this fit".
pub fn fits(text: &str, budget: usize) -> bool {
    estimate(text) <= budget
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live measurements this module was fitted on: `(sample, tokens the service
    /// reported)`. They are the reason the constants exist — without them this would be
    /// folklore. Re-measure before changing anything (`LIVE.md`).
    const LIVE: &[(&str, usize)] = &[
        (
            "The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the \
             lazy dog. The quick brown fox jumps over the lazy dog.",
            31,
        ),
        (
            "fn main() { let mut total = 0u64; for item in items.iter() { total += item.cost(); } }",
            31,
        ),
        (
            r#"{"model":"gemini-3.1-flash-lite","temperature":0.2,"max_tokens":512,"stream":true}"#,
            33,
        ),
        ("123456789 12345 678 9012 3.14159 2026-10-26", 44),
        ("আমার সোনার বাংলা, আমি তোমায় ভালোবাসি।", 10),
        ("日本語のテキストは文字あたり一トークンです。", 12),
        ("hello", 2),
    ];

    #[test]
    fn the_estimate_lands_within_eight_percent_of_every_live_measurement() {
        for (sample, actual) in LIVE {
            let estimated = estimate(sample);
            let error = (estimated as f64 - *actual as f64).abs() / *actual as f64;
            assert!(
                error <= 0.08,
                "off by {:.1}%: estimated {estimated}, live {actual} for {sample:?}",
                error * 100.0
            );
        }
    }

    #[test]
    fn the_naive_rule_is_much_worse_which_is_why_this_exists() {
        // `chars / 4` is right for Latin prose and badly wrong elsewhere. If this test ever
        // fails because the naive rule improved, the estimator is no longer earning its keep.
        let naive_error = |sample: &str, actual: usize| {
            let naive = (sample.chars().count() / 4).max(1);
            (naive as f64 - actual as f64).abs() / actual as f64
        };
        let worst = LIVE
            .iter()
            .map(|(s, n)| naive_error(s, *n))
            .fold(0.0f64, f64::max);
        assert!(worst > 0.5, "the naive rule was never this bad: {worst:.2}");

        // Digits are the case a character count cannot express at all.
        let digits = LIVE.iter().find(|(s, _)| s.starts_with("1234")).unwrap();
        assert_eq!(estimate(digits.0), 42, "live: 44");
        assert!(
            digits.0.chars().count() / 4 < 14,
            "and the naive rule says ~11"
        );
    }

    #[test]
    fn digits_cost_more_per_character_than_prose() {
        let digits = "31415926535897932384626433832795";
        let prose = "the same number of characters of ordinary english prose here";
        assert!(
            estimate(digits) > estimate(prose),
            "digits {} vs prose {}",
            estimate(digits),
            estimate(prose)
        );
    }

    #[test]
    fn code_costs_more_than_prose_of_the_same_length() {
        let prose = "the model reads the file and then explains what the function does";
        let code = "fn f(x:&str)->Result<Vec<u8>,E>{Ok(x.as_bytes().to_vec())}";
        assert!(
            estimate(code) > estimate(prose),
            "{} vs {}",
            estimate(code),
            estimate(prose)
        );
    }

    #[test]
    fn indentation_is_not_free() {
        let flat = "if x { return y; }";
        let indented = "    if x {\n        return y;\n    }";
        assert!(estimate(indented) > estimate(flat));
    }

    #[test]
    fn the_edge_cases_are_sane() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("hello"), 2, "one word, plus the request overhead");
        assert!(estimate(" ") >= 1, "whitespace is not free");
        assert!(
            estimate("東") >= 1,
            "a CJK character is not a quarter token"
        );
        assert!(estimate("আ") >= 1, "nor is a Bengali character");
        assert!(
            estimate(&"word ".repeat(200)) >= 200,
            "200 words cannot be fewer tokens"
        );
    }

    #[test]
    fn a_conversation_costs_more_than_its_text_alone() {
        let messages = vec![
            ("system".to_string(), "you are stoic-codex".to_string()),
            ("user".to_string(), "hello".to_string()),
        ];
        let text: usize = messages
            .iter()
            .map(|(r, b)| estimate(r) + estimate(b))
            .sum();
        assert_eq!(
            estimate_messages(&messages),
            text + 2 * PER_MESSAGE_OVERHEAD
        );
    }

    #[test]
    fn adversarial_input_does_not_hang_or_overflow() {
        let mut s = String::new();
        for _ in 0..500 {
            s.push_str("(((([[{{<>}}]]))))\n");
        }
        assert!(estimate(&s) > 800, "{}", estimate(&s));
        let long = "x".repeat(10_000);
        // One absurd identifier is not one token: it cracks apart.
        assert!(estimate(&long) >= 1200, "{}", estimate(&long));
        assert!(fits("hello", 10));
        assert!(!fits("hello", 0));
    }
}
