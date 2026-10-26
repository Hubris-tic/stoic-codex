//! Local conversation transcripts for `stoic-codex run --continue` / `--resume`.
//!
//! The transcript contains only canonical user/assistant messages: never a provider key,
//! system prompt, memory block, request headers, or raw HTTP payload. Files stay under the
//! local Stoic Codex config directory with owner-only permissions on Unix. Writes use a temp
//! file + rename so a crash cannot leave half a JSON session behind.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use stoic_codex_providers::{tokens, Content, Message, Role};

use crate::paths::Paths;

const FORMAT_VERSION: u8 = 1;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// One persistent, local chat session. The prompt/system context is rebuilt for each turn;
/// `messages` is the full alternating user/assistant transcript. Request copies may be trimmed
/// to fit the selected model's context window, but saved history stays complete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSession {
    version: u8,
    pub id: String,
    pub provider: String,
    pub model: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub messages: Vec<Message>,
}

impl ConversationSession {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        let now = now_ms();
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let id = format!("s-{now:013}-{sequence:020}-{}", std::process::id());
        ConversationSession {
            version: FORMAT_VERSION,
            id,
            provider: provider.into(),
            model: model.into(),
            created_at_ms: now,
            updated_at_ms: now,
            messages: Vec::new(),
        }
    }

    pub fn push_user(&mut self, text: impl Into<String>) {
        self.messages.push(Message::user(text));
        self.updated_at_ms = now_ms().max(self.updated_at_ms.saturating_add(1));
    }

    pub fn push_assistant(&mut self, text: impl Into<String>) {
        self.messages.push(Message::assistant(text));
        self.updated_at_ms = now_ms().max(self.updated_at_ms.saturating_add(1));
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != FORMAT_VERSION {
            return Err(format!(
                "unsupported transcript format version {} (expected {})",
                self.version, FORMAT_VERSION
            ));
        }
        if !valid_id(&self.id) {
            return Err("transcript has an invalid session id".into());
        }
        if self.provider.trim().is_empty() || self.model.trim().is_empty() {
            return Err("transcript is missing its provider or model".into());
        }
        if self.messages.len() % 2 != 0 {
            return Err("saved transcript must end after a complete assistant turn".into());
        }
        for (index, message) in self.messages.iter().enumerate() {
            let expected = if index % 2 == 0 { Role::User } else { Role::Assistant };
            if message.role != expected {
                return Err(format!(
                    "transcript message {} has role {:?}; expected {:?}",
                    index + 1,
                    message.role,
                    expected
                ));
            }
            if message.content.iter().any(|part| part.as_text().is_none()) {
                return Err(format!(
                    "transcript message {} contains unsupported non-text content",
                    index + 1
                ));
            }
        }
        Ok(())
    }
}

/// Load a transcript by ID. IDs are validated before they are used as file names.
pub fn load(paths: &Paths, id: &str) -> Result<ConversationSession, String> {
    if !valid_id(id) {
        return Err("invalid session id; expected letters, digits, and hyphens".into());
    }
    let path = paths.sessions_dir().join(format!("{id}.json"));
    let mut text = String::new();
    File::open(&path)
        .and_then(|mut f| f.read_to_string(&mut text))
        .map_err(|e| format!("cannot read session `{id}` at {}: {e}", path.display()))?;
    let session: ConversationSession = serde_json::from_str(&text)
        .map_err(|e| format!("session `{id}` is not valid JSON at {}: {e}", path.display()))?;
    session.validate().map_err(|e| format!("invalid session `{id}`: {e}"))?;
    if session.id != id {
        return Err(format!("session file {} contains a different id", path.display()));
    }
    Ok(session)
}

/// Find the most recently updated saved session. Malformed JSON is reported rather than
/// skipped, so a broken file cannot silently change which conversation `--continue` means.
pub fn latest(paths: &Paths) -> Result<Option<ConversationSession>, String> {
    let dir = paths.sessions_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read sessions directory {}: {e}", dir.display())),
    };

    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read a session entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).ok_or_else(|| {
            format!("session file has a non-UTF-8 name: {}", path.display())
        })?;
        sessions.push(load(paths, name)?);
    }
    sessions.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(sessions.into_iter().next())
}

/// Atomically persist a complete transcript with owner-only permissions on Unix.
pub fn save(paths: &Paths, session: &ConversationSession) -> Result<(), String> {
    session.validate().map_err(|e| format!("refusing to save session: {e}"))?;
    let dir = paths.sessions_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create sessions directory {}: {e}", dir.display()))?;
    set_private_dir(&dir).map_err(|e| format!("cannot secure sessions directory {}: {e}", dir.display()))?;

    let path = session_path(&dir, &session.id)?;
    let temp_sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(
        ".{}.{}.{}.{}.tmp",
        session.id,
        std::process::id(),
        now_ms(),
        temp_sequence
    ));
    let json = serde_json::to_vec_pretty(session)
        .map_err(|e| format!("cannot serialize session `{}`: {e}", session.id))?;
    let result = (|| {
        let mut file = open_private_new(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &path)?;
        set_private_file(&path)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(format!("cannot save session `{}`: {e}", session.id));
    }
    Ok(())
}

/// Keep the newest user turn and remove the oldest complete user/assistant exchanges until
/// the estimated input plus the reserved output fits the model's context window.
///
/// `messages` must already include the current user message as its final item. The routine
/// never trims the system prompt or the new task; if those alone do not fit, it returns an
/// error before any network request is sent.
pub fn fit_context(
    messages: &mut Vec<Message>,
    system: &str,
    max_context: usize,
    reserve_output: usize,
) -> Result<usize, String> {
    if messages.is_empty() || messages.last().map(|m| m.role) != Some(Role::User) {
        return Err("the current transcript must end with a user message".into());
    }
    let current_index = messages.len() - 1;
    if current_index % 2 != 0 {
        return Err("saved transcript history is not made of complete user/assistant turns".into());
    }
    for (index, message) in messages[..current_index].iter().enumerate() {
        let expected = if index % 2 == 0 { Role::User } else { Role::Assistant };
        if message.role != expected {
            return Err(format!(
                "transcript history message {} has role {:?}; expected {:?}",
                index + 1,
                message.role,
                expected
            ));
        }
    }

    let system_tokens = if system.is_empty() {
        0
    } else {
        tokens::estimate_messages(&[("system".to_string(), system.to_string())])
    };
    let current_tokens = estimate_messages(&messages[current_index..]);
    let minimum = system_tokens.saturating_add(current_tokens).saturating_add(reserve_output);
    if minimum > max_context {
        return Err(format!(
            "current task, system prompt, and {} reserved output tokens need about {minimum} tokens, exceeding the context window ({max_context}); shorten the task or lower --max-tokens",
            reserve_output
        ));
    }

    let mut evicted = 0;
    while system_tokens
        .saturating_add(estimate_messages(messages))
        .saturating_add(reserve_output)
        > max_context
    {
        if messages.len() < 3 || messages[0].role != Role::User || messages[1].role != Role::Assistant
        {
            return Err("conversation cannot fit without removing the current user turn".into());
        }
        messages.drain(..2);
        evicted += 1;
    }
    Ok(evicted)
}

fn estimate_messages(messages: &[Message]) -> usize {
    let pairs: Vec<(String, String)> = messages
        .iter()
        .map(|message| (role_name(message.role).to_string(), message_text(message)))
        .collect();
    tokens::estimate_messages(&pairs)
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(Content::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn session_path(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("invalid session id; expected letters, digits, and hyphens".into());
    }
    Ok(dir.join(format!("{id}.json")))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn open_private_new(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn set_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let _ = path;
    Ok(())
}

fn set_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_paths(tag: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!(
            "stoic-codex-transcript-{tag}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        Paths::new(dir)
    }

    #[test]
    fn session_ids_are_safe_and_sessions_round_trip() {
        let paths = temp_paths("roundtrip");
        let mut session = ConversationSession::new("gemini", "gemini-3.1-flash-lite");
        session.push_user("What is in this project?");
        session.push_assistant("A Rust coding agent.");
        save(&paths, &session).unwrap();

        let loaded = load(&paths, &session.id).unwrap();
        assert_eq!(loaded, session);
        assert!(valid_id(&session.id));
        assert!(load(&paths, "../config.json").is_err());
        let _ = fs::remove_dir_all(paths.home());
    }

    #[test]
    fn incomplete_sessions_are_never_persisted() {
        let paths = temp_paths("incomplete");
        let mut session = ConversationSession::new("local", "tiny");
        session.push_user("waiting for an answer");
        let error = save(&paths, &session).unwrap_err();
        assert!(error.contains("complete assistant turn"), "{error}");
        assert!(!paths.sessions_dir().exists());
        let _ = fs::remove_dir_all(paths.home());
    }

    #[test]
    fn latest_is_selected_by_update_time() {
        let paths = temp_paths("latest");
        let mut older = ConversationSession::new("gemini", "a");
        older.push_user("old");
        older.push_assistant("answer");
        older.updated_at_ms = 1;
        let mut newer = ConversationSession::new("gemini", "b");
        newer.push_user("new");
        newer.push_assistant("answer");
        newer.updated_at_ms = 2;
        save(&paths, &older).unwrap();
        save(&paths, &newer).unwrap();
        assert_eq!(latest(&paths).unwrap().unwrap().id, newer.id);
        let _ = fs::remove_dir_all(paths.home());
    }

    #[test]
    fn context_eviction_drops_oldest_complete_turns_only() {
        let old_text = "old context token ".repeat(80);
        let middle_text = "middle context token ".repeat(80);
        let current_text = "current task";
        let mut messages = vec![
            Message::user(old_text.clone()),
            Message::assistant(old_text),
            Message::user(middle_text.clone()),
            Message::assistant(middle_text),
            Message::user(current_text),
        ];
        let full_transcript = messages.clone();
        let evicted = fit_context(&mut messages, "system", 700, 32).unwrap();
        assert_eq!(full_transcript.len(), 5, "stored history stays intact");
        assert_eq!(evicted, 1, "only the oldest complete turn should be removed");
        assert_eq!(messages.len(), 3, "the newer turn and current user prompt remain");
        assert_eq!(messages[0].role, Role::User);
        assert!(message_text(&messages[0]).contains("middle"));
        assert!(!message_text(&messages[0]).contains("old"));
        assert_eq!(messages.last().unwrap().role, Role::User);
        assert!(message_text(messages.last().unwrap()).contains("current task"));
        assert_eq!(messages.len() % 2, 1, "the current user turn stays as the final item");
    }

    #[test]
    fn the_current_prompt_is_refused_if_it_alone_exceeds_context() {
        let mut messages = vec![Message::user("x".repeat(5000))];
        let error = fit_context(&mut messages, "system", 64, 32).unwrap_err();
        assert!(error.contains("exceeding the context window"), "{error}");
    }

    #[test]
    fn transcripts_are_owner_only_on_unix() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let paths = temp_paths("permissions");
            let mut session = ConversationSession::new("local", "tiny");
            session.push_user("hello");
            session.push_assistant("hi");
            save(&paths, &session).unwrap();
            let dir_mode = fs::metadata(paths.sessions_dir()).unwrap().permissions().mode() & 0o777;
            let file_mode = fs::metadata(paths.sessions_dir().join(format!("{}.json", session.id)))
                .unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
            let _ = fs::remove_dir_all(paths.home());
        }
    }

    #[test]
    fn corrupt_session_files_are_not_silently_skipped() {
        let paths = temp_paths("corrupt");
        fs::create_dir_all(paths.sessions_dir()).unwrap();
        fs::write(paths.sessions_dir().join("s-broken.json"), "not json").unwrap();
        let error = latest(&paths).unwrap_err();
        assert!(error.contains("s-broken.json"), "{error}");
        let _ = fs::remove_dir_all(paths.home());
    }

    #[test]
    fn session_timestamp_is_not_in_the_future_by_a_large_margin() {
        let session = ConversationSession::new("local", "tiny");
        assert!(session.created_at_ms <= now_ms() + Duration::from_secs(1).as_millis() as u64);
    }

    #[test]
    fn path_validation_blocks_separators_and_dot_names() {
        for id in ["../bad", "a/b", "a\\b", ".", "", "id\n"] {
            assert!(!valid_id(id), "accepted {id:?}");
        }
        assert!(valid_id("s-123456-987-0"));
    }
}
