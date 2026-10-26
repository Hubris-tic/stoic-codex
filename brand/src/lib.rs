//! # stoic-codex-brand
//!
//! Every user-visible identity string in Stoic Codex comes from here.
//!
//! Why: upstream hardcodes "Codex" in **185 shipped strings** plus a handful of URLs
//! that point at OpenAI's infrastructure. Scattering replacements across the tree means
//! the next rebrand is another 185 edits. With this crate it is one file, and the
//! `rebrand` CI check catches anything that slips back in.
//!
//! Rule for the fork: user-visible text uses these constants. Internal identifiers
//! (`codex_home`, `codex-rs`, crate names) stay as upstream so rebases stay cheap —
//! see `NAMING.md`.
//!
//! It also owns the cast of a mission — [`phalanx`]: **Strategos** and the short-hand
//! system codes (`x0-core`, `x1-write`, …) that make up **The Phalanx**. Those names appear
//! in prompts, log lines and the TUI, so they live here for the same reason as the rest.

pub mod phalanx;

/// Display name, used mid-sentence and in headings: "Stoic Codex".
pub const NAME: &str = "Stoic Codex";

/// Short slug for the binary, config dir and status-line app name: "stoic-codex".
pub const APP_NAME: &str = "stoic-codex";

/// CLI binary name.
pub const BIN: &str = "stoic-codex";

/// Config directory under `$HOME`: `.stoic-codex`.
pub const CONFIG_DIR: &str = ".stoic-codex";

/// Env var prefix for the fork's own variables: `STOIC_CODEX_HOME`, `STOIC_CODEX_LOG`, ...
pub const ENV_PREFIX: &str = "STOIC_CODEX_";

/// Previous product values accepted during the one-time config/environment migration.
pub const LEGACY_CONFIG_DIR: &str = ".zenithal";
pub const LEGACY_ENV_PREFIX: &str = "ZENITHAL_";

/// One-line description for `--help`, package metadata and the about screen.
pub const TAGLINE: &str =
    "Provider-agnostic coding agent: BYOK everywhere, your keys, your machine.";

/// Project home. Everything user-facing links here, never to OpenAI's properties.
pub const HOMEPAGE: &str = "https://github.com/Hubris-tic/stoic-codex";

/// Where bug reports go. Upstream points at `openai/codex` issues, which would send
/// our users to a tracker we cannot triage.
pub const ISSUES_URL: &str = "https://github.com/Hubris-tic/stoic-codex/issues/new";

/// Docs. Upstream uses `developers.openai.com/codex/*`.
pub const DOCS_URL: &str = "https://github.com/Hubris-tic/stoic-codex/tree/main/docs";

/// Announcement tips feed. Upstream fetches `raw.githubusercontent.com/openai/codex/...`,
/// which would render *OpenAI's* announcements inside our TUI. Point it at us, or set to
/// `None` to disable the feature entirely.
pub const ANNOUNCEMENT_TIP_URL: Option<&str> =
    Some("https://raw.githubusercontent.com/Hubris-tic/stoic-codex/main/announcement_tip.toml");

/// Attribution shown in the session header and `--version` output. Required by
/// Apache-2.0 §4(b): state that this is a modified fork.
pub const ATTRIBUTION: &str = "Stoic Codex (fork of openai/codex)";

/// The origin alone, for places that already printed [`NAME`] and should not repeat it.
pub const ORIGIN: &str = "fork of openai/codex";

/// SPDX identifier of the licence the fork is distributed under.
pub const LICENSE: &str = "Apache-2.0";

/// "Codex can make mistakes" style disclaimer, rebranded.
pub const DISCLAIMER: &str = "Stoic Codex can make mistakes";

/// Composer placeholder.
pub const PLACEHOLDER: &str = "Ask Stoic Codex to do anything";

/// Session header line, e.g. "Stoic Codex (v0.1.0)".
pub fn version_header(version: &str) -> String {
    format!("{NAME} (v{version})")
}

/// Resolve the config directory, preferring ours and falling back to the previous
/// product's location so a rebrand never orphans a user's history. Returns the *chosen*
/// path plus whether the legacy location was used.
pub fn resolve_config_dir(
    home: &std::path::Path,
    have_stoic_codex: bool,
    have_legacy: bool,
) -> (std::path::PathBuf, bool) {
    if have_stoic_codex || !have_legacy {
        (home.join(CONFIG_DIR), true)
    } else {
        (home.join(LEGACY_CONFIG_DIR), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_self_consistent() {
        assert_eq!(APP_NAME, BIN, "status line and binary must agree");
        assert!(CONFIG_DIR.starts_with('.'));
        assert!(CONFIG_DIR.contains(APP_NAME));
        assert!(
            ENV_PREFIX.ends_with('_'),
            "env prefix must be joinable: STOIC_CODEX_HOME"
        );
        assert!(NAME.contains("Stoic Codex"));
    }

    #[test]
    fn nothing_points_at_upstream_openai_properties() {
        // The whole point of the rebrand: no user-facing link leaves our project.
        for url in [
            HOMEPAGE,
            ISSUES_URL,
            DOCS_URL,
            ANNOUNCEMENT_TIP_URL.unwrap_or(""),
        ] {
            assert!(!url.is_empty() || url.is_empty());
            assert!(!url.contains("openai.com"), "{url} points at OpenAI");
            assert!(!url.contains("chatgpt.com"), "{url} points at ChatGPT");
            assert!(
                !url.contains("openai/codex"),
                "{url} points at the upstream repo"
            );
            assert!(
                !url.contains("oaistatic.com"),
                "{url} points at OpenAI static assets"
            );
        }
    }

    #[test]
    fn attribution_is_composed_from_the_parts() {
        assert_eq!(ATTRIBUTION, format!("{NAME} ({ORIGIN})"));
        assert_eq!(LICENSE, "Apache-2.0");
    }

    #[test]
    fn version_header_is_branded() {
        assert_eq!(version_header("0.1.0"), "Stoic Codex (v0.1.0)");
    }

    #[test]
    fn config_dir_falls_back_to_legacy_rather_than_orphaning_data() {
        use std::path::Path;
        let home = Path::new("/home/u");
        // Both present -> prefer ours.
        assert_eq!(
            resolve_config_dir(home, true, true),
            (home.join(".stoic-codex"), true)
        );
        // Only ours -> use ours.
        assert_eq!(
            resolve_config_dir(home, true, false),
            (home.join(".stoic-codex"), true)
        );
        // Fresh install -> create ours.
        assert_eq!(
            resolve_config_dir(home, false, false),
            (home.join(".stoic-codex"), true)
        );
        // Only the previous product directory -> keep using it until migration completes.
        assert_eq!(
            resolve_config_dir(home, false, true),
            (home.join(".zenithal"), false)
        );
    }

    #[test]
    fn attribution_obeys_apache_notice_requirement() {
        assert!(ATTRIBUTION.contains("fork of"));
        assert!(ATTRIBUTION.contains("openai/codex"));
    }
}
