use super::redaction::redact_secrets;
use super::truncate_chars_with_notice;

/// Applies the shared outbound-content pipeline in its security-sensitive order:
/// redact first, normalize controls/whitespace second, and truncate last.
pub(crate) fn prepare_content(text: &str, max_chars: usize, should_redact: bool) -> Option<String> {
    let redacted = if should_redact {
        redact_secrets(text)
    } else {
        text.to_owned()
    };
    let normalized = normalize_whitespace(&redacted);
    if normalized.is_empty() {
        None
    } else {
        Some(truncate_chars_with_notice(&normalized, max_chars))
    }
}

pub(crate) fn normalize_whitespace(text: &str) -> String {
    let mut scalar_normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                scalar_normalized.push('\n');
            }
            '\n' => scalar_normalized.push('\n'),
            '\t' => scalar_normalized.push_str("    "),
            character if character.is_control() => scalar_normalized.push(' '),
            character => scalar_normalized.push(character),
        }
    }

    let mut lines = Vec::new();
    let mut consecutive_empty = 0;
    for line in scalar_normalized.split('\n') {
        let line = line.trim_end();
        if line.trim().is_empty() {
            consecutive_empty += 1;
            if consecutive_empty <= 3 {
                lines.push(String::new());
            }
        } else {
            consecutive_empty = 0;
            lines.push(line.to_owned());
        }
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pipeline_redacts_normalizes_then_truncates() {
        let prepared = prepare_content(
            "\r\nAuthorization: Bearer secret-value\t\n\n\n\nvisible",
            80,
            true,
        )
        .unwrap();
        assert!(prepared.contains("Bearer [REDACTED]"));
        assert!(!prepared.contains("secret-value"));
        assert!(!prepared.contains('\r'));
        assert!(!prepared.contains("\n\n\n\n\n"));
        assert!(prepared.chars().count() <= 80);
    }
}
