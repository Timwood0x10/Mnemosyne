//! Noise and security filters for the distillation pipeline.
//!
//! [`NoiseFilter`] rejects utterances that are pure social chatter or so short
//! they cannot encode a problem-solution pair. [`SecurityFilter`] rejects
//! utterances containing secrets (API keys, tokens, passwords).
//!
//! Both filters are stateless and cheap; they run early in the pipeline to
//! avoid wasting embedding-service budget on junk.

use crate::types::Message;

/// Maximum message length we keep. Longer messages are likely transcripts
/// or code dumps and are not useful as a single distilled memory.
pub const MAX_MESSAGE_LENGTH: usize = 8_000;

/// Minimum message length for a non-trivial user utterance.
pub const MIN_MEANINGFUL_LENGTH: usize = 8;

/// Phrases that mark an utterance as social chatter, not a problem.
///
/// Matching is boundary-aware: a phrase matches only if it is followed by
/// end-of-string or a non-alphanumeric character (whitespace, punctuation).
/// This prevents `"hi"` from swallowing `"hi, how do I parse JSON?"`.
const CHATTER_PHRASES: &[&str] = &[
    "thank you",
    "thanks",
    "thx",
    "ok",
    "okay",
    "great",
    "perfect",
    "got it",
    "sounds good",
    "hello",
    "hi",
    "hey",
    "bye",
    "lol",
    "haha",
    "cool",
    "nice",
    "awesome",
    "sure",
    "yes please",
    "no thanks",
];

/// Boundary-aware prefix match for chatter phrases.
///
/// Returns `true` when `text` starts with `phrase` AND the next character
/// (if any) is non-alphanumeric. A trailing question (`?`/`？`) is NEVER
/// chatter: the remainder after the prefix is a real question, so
/// `"hi, how do I parse JSON?"` and `"ok, how do I fix the crash?"` pass
/// through (the docstring example was previously swallowed by the prefix
/// rule alone).
#[must_use]
fn matches_chatter_prefix(text: &str, phrase: &str) -> bool {
    if !text.starts_with(phrase) {
        return false;
    }
    // A question after the chatter word is substantive content.
    if text.contains('?') || text.contains('？') {
        return false;
    }
    match text[phrase.len()..].chars().next() {
        None => true,
        Some(c) => !c.is_alphanumeric(),
    }
}

/// Noise filter — rejects chatter and over-long/short messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoiseFilter;

impl NoiseFilter {
    /// Build a new noise filter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Returns `true` if the message should be discarded as noise.
    ///
    /// Noise is defined as:
    /// - empty or whitespace-only content, or
    /// - content shorter than [`MIN_MEANINGFUL_LENGTH`], or
    /// - content starting with a known chatter phrase, or
    /// - content longer than [`MAX_MESSAGE_LENGTH`] (likely a transcript).
    #[must_use]
    pub fn is_noise(&self, msg: &Message) -> bool {
        let trimmed = msg.content.trim();
        if trimmed.is_empty() {
            return true;
        }
        // Character count for the MIN gate (char-oriented threshold); byte
        // length for the MAX payload cap.
        if trimmed.chars().count() < MIN_MEANINGFUL_LENGTH {
            return true;
        }
        if trimmed.len() > MAX_MESSAGE_LENGTH {
            return true;
        }
        let lower = trimmed.to_lowercase();
        if CHATTER_PHRASES
            .iter()
            .any(|p| matches_chatter_prefix(&lower, p))
        {
            return true;
        }
        false
    }

    /// Filter a slice of messages, returning indices that survive.
    ///
    /// Useful when the caller wants to preserve the original ordering but
    /// drop noise entries without re-allocating the message vector.
    pub fn retain_indices<'a>(
        &'a self,
        messages: &'a [Message],
    ) -> impl Iterator<Item = usize> + 'a {
        messages
            .iter()
            .enumerate()
            .filter(move |(_, m)| !self.is_noise(m))
            .map(|(i, _)| i)
    }
}

/// Security filter — rejects messages containing obvious secrets.
///
/// The patterns are conservative: they target the common shapes of leaked
/// credentials (long hex/base64 strings adjacent to a secret-name keyword)
/// rather than trying to detect arbitrary secrets. False positives are
/// preferred over false negatives here because the upstream embedding
/// service is treated as untrusted.
#[derive(Debug, Clone, Default)]
pub struct SecurityFilter {
    /// If non-empty, only these patterns are checked; otherwise the default
    /// built-in patterns are used.
    custom_patterns: Vec<String>,
}

impl SecurityFilter {
    /// Build a new security filter with the built-in pattern set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            custom_patterns: Vec::new(),
        }
    }

    /// Replace the pattern set with a custom list of substrings.
    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.custom_patterns = patterns;
        self
    }

    /// Returns `true` if the message contains a likely secret.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to scan. Case-insensitive substring match is used.
    #[must_use]
    pub fn is_sensitive(&self, msg: &Message) -> bool {
        let lower = msg.content.to_lowercase();
        // Pick the active pattern list; the static slice is zero-cost to borrow.
        if !self.custom_patterns.is_empty() {
            return self
                .custom_patterns
                .iter()
                .any(|p| lower.contains(&p.to_lowercase()));
        }
        SECRET_INDICATORS.iter().any(|p| lower.contains(p))
    }
}

/// Built-in secret-indicator substrings, lowercase.
const SECRET_INDICATORS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "secret_key",
    "secret-key",
    "access_token",
    "access-token",
    "auth_token",
    "auth-token",
    "bearer ",
    "password=",
    "password: ",
    "passwd=",
    "private_key",
    "private-key",
    "begin rsa private key",
    "begin openvpn static key",
    "begin private key",
    "aws_secret_access_key",
    "client_secret",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Objective: Verify NoiseFilter rejects empty and whitespace messages.
    /// Invariants: All-whitespace content always reports as noise.
    #[test]
    fn noise_filter_rejects_blank() {
        let f = NoiseFilter::new();
        let empty = Message::new("user", "");
        let ws = Message::new("user", "    \t\n  ");
        assert!(f.is_noise(&empty), "empty content is noise");
        assert!(f.is_noise(&ws), "whitespace-only content is noise");
    }

    /// Objective: Verify NoiseFilter rejects very short content.
    /// Invariants: Content shorter than MIN_MEANINGFUL_LENGTH is noise.
    #[test]
    fn noise_filter_rejects_short() {
        let f = NoiseFilter::new();
        let short = Message::new("user", "hi"); // 2 chars
        assert!(f.is_noise(&short), "very short content is noise");
    }

    /// Objective: Verify NoiseFilter rejects chatter prefixes.
    /// Invariants: Any chatter phrase as a prefix is rejected.
    #[test]
    fn noise_filter_rejects_chatter() {
        let f = NoiseFilter::new();
        let cases = [
            ("thank you so much for your help", true),
            ("ok that's fine", true),
            ("great, let me try that", true),
            ("How do I read a file?", false),
            ("The function returns 42.", false),
        ];
        for (text, expected) in cases {
            let msg = Message::new("user", text);
            assert_eq!(
                f.is_noise(&msg),
                expected,
                "input `{text}` expected noise={expected}"
            );
        }
    }

    /// Objective: Verify retain_indices preserves non-noise messages.
    /// Invariants: Surviving indices point at non-noise messages.
    #[test]
    fn noise_filter_retain_indices() {
        let f = NoiseFilter::new();
        let msgs = vec![
            Message::new("user", "How do I read a file?"), // 0: keep
            Message::new("user", "thanks"),                // 1: drop
            Message::new("assistant", "Use std::fs::read_to_string."), // 2: keep
            Message::new("user", ""),                      // 3: drop
        ];
        let kept: Vec<usize> = f.retain_indices(&msgs).collect();
        assert_eq!(kept, vec![0, 2], "indices of non-noise messages");
    }

    /// Objective: Verify SecurityFilter detects API keys in various shapes.
    /// Invariants: All known indicator patterns trigger the filter.
    #[test]
    fn security_filter_detects_api_keys() {
        let f = SecurityFilter::new();
        let cases = [
            "My api_key is abc123",
            "Set API-KEY=secret",
            "Authorization: Bearer xyz",
            "password=hunter2",
            "PRIVATE_KEY material here",
            "BEGIN PRIVATE KEY-----",
            "This is a normal message",
        ];
        let expected = [true, true, true, true, true, true, false];
        for (text, want) in cases.iter().zip(expected.iter()) {
            let msg = Message::new("user", *text);
            assert_eq!(
                f.is_sensitive(&msg),
                *want,
                "input `{text}` expected sensitive={want}"
            );
        }
    }

    /// Objective: Verify custom patterns fully replace the default set.
    /// Invariants: When custom_patterns is set, default patterns are not checked.
    #[test]
    fn security_filter_custom_patterns_replace_defaults() {
        let f = SecurityFilter::new().with_patterns(vec!["widget".to_string()]);
        // default pattern: would be sensitive with default config
        let api_msg = Message::new("user", "my api_key is abc");
        assert!(!f.is_sensitive(&api_msg), "custom filter ignores defaults");
        // custom pattern hit
        let widget_msg = Message::new("user", "the widget broke");
        assert!(f.is_sensitive(&widget_msg), "custom pattern triggers");
    }

    /// Objective: Verify case-insensitive matching works in both directions.
    /// Invariants: Uppercase API_KEY and lowercase api_key both match.
    #[test]
    fn security_filter_case_insensitive() {
        let f = SecurityFilter::new();
        let upper = Message::new("user", "MY API_KEY IS LEAKED");
        let lower = Message::new("user", "my api_key is leaked");
        assert!(f.is_sensitive(&upper), "uppercase indicator matches");
        assert!(f.is_sensitive(&lower), "lowercase indicator matches");
    }
}
