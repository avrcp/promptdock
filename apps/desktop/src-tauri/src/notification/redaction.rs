const REDACTED: &str = "[REDACTED]";

// Longest/specific names come first so `set-cookie` is handled as one field and
// not as the `cookie` suffix. Matching is ASCII case-insensitive because these
// are protocol/configuration identifiers, while the surrounding content may be
// arbitrary Unicode.
const SECRET_FIELD_NAMES: &[&str] = &[
    "aws_secret_access_key",
    "aws_access_key_id",
    "connection string",
    "database_url",
    "private key",
    "set-cookie",
    "api_key",
    "api-key",
    "apikey",
    "password",
    "passwd",
    "secret",
    "token",
    "cookie",
];

/// Redacts common credentials without changing non-secret Markdown/source text.
///
/// This is deliberately conservative: when a recognized field has an unquoted
/// value, the rest of that line is removed. Losing a little notification context
/// is preferable to persisting a credential in the durable outbox.
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut private_key_block = false;

    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }

        if private_key_block {
            if contains_ascii_case_insensitive(line, "-----END ")
                && contains_ascii_case_insensitive(line, "PRIVATE KEY-----")
            {
                output.push_str(line);
                private_key_block = false;
            } else if !output.ends_with(REDACTED) {
                output.push_str(REDACTED);
            }
            continue;
        }

        if contains_ascii_case_insensitive(line, "-----BEGIN ")
            && contains_ascii_case_insensitive(line, "PRIVATE KEY-----")
        {
            output.push_str(line);
            private_key_block = true;
            continue;
        }

        let line = redact_bearer_tokens(line);
        let line = redact_uri_userinfo(&line);
        let line = redact_named_fields(&line);
        let line = redact_aws_access_key_ids(&line);
        let line = redact_common_api_tokens(&line);
        output.push_str(&line);
    }

    output
}

/// Redacts URI userinfo (`scheme://userinfo@host`) while preserving the rest of
/// the URI. The scan is intentionally structural: email addresses, ordinary
/// `@` text and URLs without userinfo are left untouched.
fn redact_uri_userinfo(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut copied_until = 0;
    let mut cursor = 0;

    while let Some((authority_start, userinfo_end)) = find_uri_userinfo(bytes, cursor) {
        output.push_str(&line[copied_until..authority_start]);
        output.push_str(REDACTED);
        copied_until = userinfo_end;
        cursor = userinfo_end;
    }
    output.push_str(&line[copied_until..]);
    output
}

fn find_uri_userinfo(bytes: &[u8], mut cursor: usize) -> Option<(usize, usize)> {
    while cursor < bytes.len() {
        if !bytes[cursor].is_ascii_alphabetic()
            || (cursor > 0 && is_uri_scheme_character(bytes[cursor - 1]))
        {
            cursor += 1;
            continue;
        }

        let scheme_start = cursor;
        cursor += 1;
        while cursor < bytes.len() && is_uri_scheme_character(bytes[cursor]) {
            cursor += 1;
        }
        if !bytes[cursor..].starts_with(b"://") {
            cursor = scheme_start + 1;
            continue;
        }

        let authority_start = cursor + 3;
        let mut authority_end = authority_start;
        while authority_end < bytes.len() && !is_uri_authority_delimiter(bytes[authority_end]) {
            authority_end += 1;
        }
        let Some(relative_at) = bytes[authority_start..authority_end]
            .iter()
            .rposition(|byte| *byte == b'@')
        else {
            cursor = authority_end.max(authority_start + 1);
            continue;
        };
        let userinfo_end = authority_start + relative_at;
        let host_start = userinfo_end + 1;
        if userinfo_end == authority_start || host_start >= authority_end {
            cursor = authority_end.max(authority_start + 1);
            continue;
        }
        return Some((authority_start, userinfo_end));
    }
    None
}

fn is_uri_scheme_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

fn is_uri_authority_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'/' | b'?' | b'#' | b'\\' | b'\'' | b'"' | b'`' | b'<' | b'>' | b'(' | b')'
        )
}

fn redact_named_fields(line: &str) -> String {
    let mut earliest: Option<(usize, usize)> = None;
    for name in SECRET_FIELD_NAMES {
        let mut offset = 0;
        while let Some(relative) = find_ascii_case_insensitive(&line[offset..], name) {
            let start = offset + relative;
            let end = start + name.len();
            if has_field_boundaries(line, start, end) && value_start(line, end).is_some() {
                if earliest.is_none_or(|(current, _)| start < current) {
                    earliest = Some((start, end));
                }
                break;
            }
            offset = end;
            if offset >= line.len() {
                break;
            }
        }
    }

    let Some((_start, field_end)) = earliest else {
        return line.to_owned();
    };
    let Some(value_start) = value_start(line, field_end) else {
        return line.to_owned();
    };
    replace_field_value(line, value_start)
}

fn value_start(line: &str, field_end: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut cursor = field_end;
    let mut had_space = false;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        had_space = true;
        cursor += 1;
    }

    // JSON/TOML keys are commonly quoted: `"api_key": "..."`.
    if cursor < bytes.len() && matches!(bytes[cursor], b'\'' | b'"' | b'`') {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
    }

    if cursor < bytes.len() && matches!(bytes[cursor], b'=' | b':') {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        return (cursor < bytes.len()).then_some(cursor);
    }

    // Covers command-line/config forms such as `--password hunter2`.
    had_space
        .then_some(cursor)
        .filter(|cursor| *cursor < bytes.len())
}

fn replace_field_value(line: &str, value_start: usize) -> String {
    let bytes = line.as_bytes();
    let mut result = String::with_capacity(line.len().min(value_start + REDACTED.len() + 2));
    result.push_str(&line[..value_start]);

    if let Some(&quote @ (b'\'' | b'"' | b'`')) = bytes.get(value_start) {
        result.push(char::from(quote));
        result.push_str(REDACTED);
        if let Some(end) = find_closing_quote(bytes, value_start + 1, quote) {
            result.push(char::from(quote));
            // A source/config line can contain several assignments. Continue
            // scanning only the untouched suffix so the field just replaced
            // cannot be matched again and recursion always makes progress.
            result.push_str(&redact_named_fields(&line[end + 1..]));
        }
    } else {
        result.push_str(REDACTED);
    }
    result
}

fn find_closing_quote(bytes: &[u8], mut cursor: usize, quote: u8) -> Option<usize> {
    let mut escaped = false;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if byte == quote && !escaped {
            return Some(cursor);
        }
        escaped = byte == b'\\' && !escaped;
        if byte != b'\\' {
            escaped = false;
        }
        cursor += 1;
    }
    None
}

fn redact_bearer_tokens(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut remaining = line;
    while let Some(start) = find_ascii_case_insensitive(remaining, "bearer") {
        let end = start + "bearer".len();
        let left_ok = start == 0 || !remaining.as_bytes()[start - 1].is_ascii_alphanumeric();
        let right_ok = remaining
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_whitespace);
        if !left_ok || !right_ok {
            result.push_str(&remaining[..end]);
            remaining = &remaining[end..];
            continue;
        }

        result.push_str(&remaining[..end]);
        let mut token_start = end;
        while remaining
            .as_bytes()
            .get(token_start)
            .is_some_and(u8::is_ascii_whitespace)
        {
            result.push(char::from(remaining.as_bytes()[token_start]));
            token_start += 1;
        }
        let token_end = remaining[token_start..]
            .char_indices()
            .find_map(|(offset, character)| {
                matches!(character, '"' | '\'' | '`' | ',' | ';' | ')' | ']' | '}')
                    .then_some(token_start + offset)
                    .or_else(|| character.is_whitespace().then_some(token_start + offset))
            })
            .unwrap_or(remaining.len());
        if token_end > token_start {
            result.push_str(REDACTED);
        }
        remaining = &remaining[token_end..];
    }
    result.push_str(remaining);
    result
}

fn redact_aws_access_key_ids(line: &str) -> String {
    redact_prefixed_token(line, &["AKIA", "ASIA"], 20)
}

fn redact_common_api_tokens(line: &str) -> String {
    let line = redact_prefixed_token(line, &["github_pat_"], 20);
    let line = redact_prefixed_token(&line, &["ghp_", "gho_", "ghu_", "ghs_"], 20);
    redact_prefixed_token(&line, &["sk-"], 20)
}

fn redact_prefixed_token(line: &str, prefixes: &[&str], minimum_len: usize) -> String {
    let bytes = line.as_bytes();
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let prefix = prefixes.iter().find(|prefix| {
            bytes[cursor..].starts_with(prefix.as_bytes())
                && (cursor == 0 || !bytes[cursor - 1].is_ascii_alphanumeric())
        });
        let Some(prefix) = prefix else {
            let character = line[cursor..].chars().next().expect("valid UTF-8 boundary");
            output.push(character);
            cursor += character.len_utf8();
            continue;
        };
        let mut end = cursor + prefix.len();
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'-'))
        {
            end += 1;
        }
        if end - cursor >= minimum_len {
            output.push_str(REDACTED);
        } else {
            output.push_str(&line[cursor..end]);
        }
        cursor = end;
    }
    output
}

fn has_field_boundaries(line: &str, start: usize, end: usize) -> bool {
    let bytes = line.as_bytes();
    let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
    let right_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
    left_ok && right_ok
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    find_ascii_case_insensitive(haystack, needle).is_some()
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|start| {
        haystack[*start..*start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_field_names_and_redacts_common_assignment_styles() {
        let input = concat!(
            "OPENAI_API_KEY=sk-this-must-not-survive\n",
            "apikey: abc123\n",
            "api-key = 'quoted-secret'\n",
            "const password = \"hunter2\";\n",
            "--passwd another-secret\n",
            "token: tok-value\n",
            "client_secret=secret-value\n",
            "private key: inline-key\n",
            "AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF\n",
            "AWS_SECRET_ACCESS_KEY=aws-secret\n",
            "DATABASE_URL=postgres://user:pass@localhost/db\n",
            "connection string: Server=db;Password=pw\n",
            "Cookie: session=abc; user=me\n",
            "Set-Cookie: session=def; HttpOnly",
        );
        let redacted = redact_secrets(input);

        for key in [
            "OPENAI_API_KEY",
            "apikey",
            "api-key",
            "password",
            "passwd",
            "token",
            "client_secret",
            "private key",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "DATABASE_URL",
            "connection string",
            "Cookie",
            "Set-Cookie",
        ] {
            assert!(redacted.contains(key), "missing preserved key: {key}");
        }
        for secret in [
            "sk-this-must-not-survive",
            "abc123",
            "quoted-secret",
            "hunter2",
            "another-secret",
            "tok-value",
            "secret-value",
            "inline-key",
            "AKIA1234567890ABCDEF",
            "aws-secret",
            "postgres://user:pass@localhost/db",
            "Server=db",
            "session=abc",
            "session=def",
        ] {
            assert!(!redacted.contains(secret), "leaked secret: {secret}");
        }
        assert!(redacted.matches(REDACTED).count() >= 14);
    }

    #[test]
    fn redacts_bearer_tokens_in_headers_markdown_and_source_code() {
        let input = concat!(
            "Authorization: Bearer eyJhbGciOi.secret.signature\n",
            "curl -H \"Authorization: bearer second-token\" https://example.test\n",
            "Markdown `Bearer third-token` remains readable",
        );
        let redacted = redact_secrets(input);
        assert_eq!(redacted.matches("[REDACTED]").count(), 3);
        assert!(redacted.contains("Authorization: Bearer [REDACTED]"));
        assert!(redacted.contains("https://example.test"));
        assert!(!redacted.contains("eyJhbGciOi"));
        assert!(!redacted.contains("second-token"));
        assert!(!redacted.contains("third-token"));
    }

    #[test]
    fn replaces_private_key_material_but_keeps_block_labels() {
        let input = concat!(
            "before\n",
            "-----BEGIN RSA PRIVATE KEY-----\n",
            "MIIEowIBAAKCAQEAhighly-secret\n",
            "another-secret-line\n",
            "-----END RSA PRIVATE KEY-----\n",
            "after",
        );
        let redacted = redact_secrets(input);
        assert!(redacted.contains("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(redacted.contains("-----END RSA PRIVATE KEY-----"));
        assert!(redacted.contains(REDACTED));
        assert!(!redacted.contains("MIIEow"));
        assert!(!redacted.contains("another-secret-line"));
        assert!(redacted.ends_with("after"));
    }

    #[test]
    fn redacts_every_named_secret_on_the_same_line() {
        let input = concat!(
            "token=\"first-secret\" password='second-secret' api_key=`third-secret` ",
            "Cookie: fourth-secret",
        );
        let redacted = redact_secrets(input);
        assert_eq!(redacted.matches(REDACTED).count(), 4);
        for secret in [
            "first-secret",
            "second-secret",
            "third-secret",
            "fourth-secret",
        ] {
            assert!(
                !redacted.contains(secret),
                "leaked same-line secret: {secret}"
            );
        }
        for key in ["token", "password", "api_key", "Cookie"] {
            assert!(redacted.contains(key), "missing same-line key: {key}");
        }
    }

    #[test]
    fn redacts_uri_userinfo_and_preserves_scheme_host_port_and_path() {
        let input = concat!(
            "https://alice:hunter2@example.test/path?q=1#result\n",
            "ssh://deploy@git.example/repository\n",
            "postgres://user:pass@db.internal:5432/app\n",
            "mongodb://alice%40corp:p%40ss@[2001:db8::1]:27017/records\n",
            "\"mysql://quoted-user:quoted-pass@mysql.example/app\"\n",
            "[数据库](postgresql://md-user:md-pass@db.example/app)\n",
            "`redis://code-user:code-pass@[::1]:6379/0`",
        );
        let redacted = redact_secrets(input);
        assert_eq!(
            redacted,
            concat!(
                "https://[REDACTED]@example.test/path?q=1#result\n",
                "ssh://[REDACTED]@git.example/repository\n",
                "postgres://[REDACTED]@db.internal:5432/app\n",
                "mongodb://[REDACTED]@[2001:db8::1]:27017/records\n",
                "\"mysql://[REDACTED]@mysql.example/app\"\n",
                "[数据库](postgresql://[REDACTED]@db.example/app)\n",
                "`redis://[REDACTED]@[::1]:6379/0`",
            )
        );
        for secret in [
            "alice:hunter2",
            "deploy@",
            "user:pass",
            "alice%40corp:p%40ss",
            "quoted-user:quoted-pass",
            "md-user:md-pass",
            "code-user:code-pass",
        ] {
            assert!(!redacted.contains(secret), "leaked URI userinfo: {secret}");
        }
    }

    #[test]
    fn uri_rule_does_not_modify_normal_urls_emails_or_plain_at_text() {
        let input = concat!(
            "https://example.test/alice@example/path\n",
            "postgres://db.internal:5432/app\n",
            "联系 alice@example.test 或输入 foo@bar\n",
            "Markdown [普通链接](https://example.test/path?q=user@example.test)\n",
            "代码 `value.contains(\"a@b\")`",
        );
        assert_eq!(redact_secrets(input), input);
    }

    #[test]
    fn leaves_normal_chinese_emoji_markdown_and_source_unchanged() {
        let input = "完成审查 ✅\n\n## 结果\n\n```rust\nfn main() { println!(\"你好\"); }\n```";
        assert_eq!(redact_secrets(input), input);
    }
}
