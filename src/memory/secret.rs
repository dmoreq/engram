//! Suspected-secret detection for memory note fields.
//!
//! Best-effort, deterministic, dependency-free: provider prefixes with length
//! and token-boundary rules. It blocks ordinary words such as `task-1` that a
//! bare `sk-` substring check rejected, and it catches common token shapes.
//! It is not an entropy detector; unknown formats can pass.

/// One matched pattern label. The label is internal; callers emit the shared
/// `SUSPECTED_SECRET_MSG` and never echo the matched text.
pub(crate) fn suspected_secret(text: &str) -> Option<&'static str> {
    if has_openai_key(text) {
        return Some("openai-key");
    }
    if has_github_token(text) {
        return Some("github-token");
    }
    if has_aws_access_key(text) {
        return Some("aws-access-key");
    }
    if has_jwt(text) {
        return Some("jwt");
    }
    if has_private_key(text) {
        return Some("private-key");
    }
    if has_slack_token(text) {
        return Some("slack-token");
    }
    if has_bearer_token(text) {
        return Some("bearer-token");
    }
    if has_database_url(text) {
        return Some("database-url");
    }
    None
}

/// Thin wrapper used by every validation call site.
pub(crate) fn contains_suspected_secret(text: &str) -> bool {
    suspected_secret(text).is_some()
}

fn boundary(text: &str, byte_idx: usize) -> bool {
    byte_idx == 0
        || !text[..byte_idx]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric())
}

fn run_len(text: &str, start: usize, allowed: impl Fn(char) -> bool, min: usize) -> bool {
    text[start..].chars().take_while(|c| allowed(*c)).count() >= min
}

fn has_openai_key(text: &str) -> bool {
    text.match_indices("sk-").any(|(idx, _)| {
        if !boundary(text, idx) {
            return false;
        }
        let run: String = text[idx + 3..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if run.len() < 16 {
            return false;
        }
        // Snake/kebab prose (`sk-learn-pipeline-config`) carries `-` or `_`;
        // require digit/uppercase entropy for those shapes while preserving
        // detection of pure-alphanumeric keys.
        if run.contains('-') || run.contains('_') {
            run.chars()
                .any(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        } else {
            true
        }
    })
}

fn has_github_token(text: &str) -> bool {
    const CLASSIC: [&str; 5] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
    for prefix in CLASSIC {
        if text.match_indices(prefix).any(|(idx, _)| {
            boundary(text, idx)
                && run_len(text, idx + prefix.len(), |c| c.is_ascii_alphanumeric(), 20)
        }) {
            return true;
        }
    }
    text.match_indices("github_pat_").any(|(idx, _)| {
        boundary(text, idx)
            && run_len(
                text,
                idx + "github_pat_".len(),
                |c| c.is_ascii_alphanumeric() || c == '_',
                22,
            )
    })
}

fn has_aws_access_key(text: &str) -> bool {
    for prefix in ["AKIA", "ASIA"] {
        for (idx, _) in text.match_indices(prefix) {
            if !boundary(text, idx) {
                continue;
            }
            let rest = &text[idx + prefix.len()..];
            let mut chars = rest.chars();
            let key: String = chars.by_ref().take(16).collect();
            if key.len() != 16
                || !key
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            {
                continue;
            }
            let next_is_key = chars
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
            if !next_is_key {
                return true;
            }
        }
    }
    false
}

fn has_jwt(text: &str) -> bool {
    // Scan every `eyJ` hit instead of whitespace-split words: JWTs embedded in
    // JSON strings (`"token": "eyJ..."`) or assignments (`JWT=eyJ...`) never
    // start a whitespace-delimited word. The boundary gate rejects mid-word
    // false hits, and the maximal base64url-and-dot run carries the three
    // base64url segments (each >= 10 chars) the detector requires.
    for (idx, _) in text.match_indices("eyJ") {
        if !boundary(text, idx) {
            continue;
        }
        let run_end = text[idx..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
            .unwrap_or(text.len() - idx);
        // A trailing sentence period must not shrink the final segment to an
        // empty part and bypass the three-segment shape check.
        let run = text[idx..idx + run_end].trim_end_matches('.');
        let parts: Vec<&str> = run.split('.').collect();
        if parts.len() >= 3
            && parts.iter().all(|p| {
                p.len() >= 10
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
        {
            return true;
        }
    }
    false
}

fn has_private_key(text: &str) -> bool {
    let Some(begin) = text.find("-----BEGIN ") else {
        return false;
    };
    text[begin..].contains("PRIVATE KEY-----") || text[begin..].contains("PRIVATE KEY BLOCK-----")
}

fn has_slack_token(text: &str) -> bool {
    for prefix in ["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"] {
        if text.match_indices(prefix).any(|(idx, _)| {
            boundary(text, idx)
                && run_len(
                    text,
                    idx + prefix.len(),
                    |c| c.is_ascii_alphanumeric() || c == '-',
                    10,
                )
        }) {
            return true;
        }
    }
    false
}

fn has_bearer_token(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for (idx, _) in lower.match_indices("bearer") {
        if !boundary(&lower, idx) {
            continue;
        }
        // Tolerate any run of spaces/tabs between the scheme and the token;
        // no separator at all ("bearerxyz") is not a scheme occurrence.
        let rest = &lower[idx + "bearer".len()..];
        let Some(offset) = rest.find(|c: char| c != ' ' && c != '\t') else {
            continue;
        };
        if offset == 0 {
            continue;
        }
        let start = idx + "bearer".len() + offset;
        let run: String = text[start..]
            .chars()
            .take_while(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '-')
            })
            .collect();
        if run.len() >= 20
            && run
                .chars()
                .any(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
        {
            return true;
        }
    }
    false
}

fn has_database_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for scheme in [
        "postgres://",
        "postgresql://",
        "mysql://",
        "mongodb://",
        "mongodb+srv://",
        "redis://",
        "rediss://",
        "mariadb://",
        "amqp://",
        "amqps://",
    ] {
        for (idx, _) in lower.match_indices(scheme) {
            if !boundary(&lower, idx) {
                continue;
            }
            let rest = &lower[idx + scheme.len()..];
            let authority = rest
                .split(|c: char| matches!(c, '/' | '?' | '#') || c.is_whitespace())
                .next()
                .unwrap_or("");
            let Some((userinfo, _host)) = authority.rsplit_once('@') else {
                continue;
            };
            if let Some((user, pass)) = userinfo.split_once(':') {
                // Username is optional (`redis://:pass@host`); an empty
                // password (`user:@host`) is not a credential.
                let _ = user;
                if !pass.is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_words() {
        for text in [
            "task-1 pending",
            "risk-averse reviewer",
            "desk-lamp on the shelf",
            "sk-learn pipeline",
            "application.configuration.properties",
            "https://deploy@example.com:8443",
        ] {
            assert_eq!(suspected_secret(text), None, "{text}");
        }
    }

    #[test]
    fn accepts_short_prefixes() {
        for text in ["sk-12345", "ghp_abcdef", "eyJhbGciOi"] {
            assert_eq!(suspected_secret(text), None, "{text}");
        }
    }

    #[test]
    fn rejects_each_pattern() {
        let openai = format!("sk-{}", "a".repeat(24));
        let github = format!("ghp_{}", "a".repeat(36));
        let github_pat = format!("github_pat_{}", "a".repeat(22));
        let aws = format!("AKIA{}", "A".repeat(16));
        let jwt = format!(
            "eyJ{}.eyJ{}.{}",
            "a".repeat(10),
            "b".repeat(10),
            "c".repeat(10)
        );
        let pem = format!("-----BEGIN {} PRIVATE KEY-----", "RSA");
        let slack = format!("xoxb-{}", "1".repeat(12));
        let bearer = format!("Bearer a1B2{}", "c".repeat(20));
        let database = "postgres://user:pass@localhost/db";

        for (text, label) in [
            (openai, "openai-key"),
            (github, "github-token"),
            (github_pat, "github-token"),
            (aws, "aws-access-key"),
            (jwt, "jwt"),
            (pem, "private-key"),
            (slack, "slack-token"),
            (bearer, "bearer-token"),
            (database.to_string(), "database-url"),
        ] {
            assert_eq!(suspected_secret(&text), Some(label), "{text}");
        }
    }

    #[test]
    fn jwt_detected_in_json_string_and_env_assignment() {
        let seg1 = format!("eyJ{}", "a".repeat(10));
        let seg2 = format!("eyJ{}", "b".repeat(10));
        let seg3 = "c".repeat(10);
        let bare = format!("{seg1}.{seg2}.{seg3}");
        // JSON string value and KEY=assignment shapes: the JWT never starts a
        // whitespace-delimited word, yet must still be detected.
        for text in [
            format!(r#"{{"token": "{bare}"}}"#),
            format!("JWT={bare}"),
            format!("auth = Bearer {bare};"),
        ] {
            assert_eq!(suspected_secret(&text), Some("jwt"), "{text}");
        }
    }

    #[test]
    fn jwt_with_trailing_sentence_period_still_detected() {
        let seg1 = format!("eyJ{}", "a".repeat(10));
        let seg2 = format!("eyJ{}", "b".repeat(10));
        let seg3 = "c".repeat(10);
        // A sentence-final dot used to shrink the last segment to an empty
        // part and bypass the shape check.
        assert_eq!(
            suspected_secret(&format!("token leaked {seg1}.{seg2}.{seg3}.")),
            Some("jwt")
        );
    }

    #[test]
    fn database_url_detects_redis_mariadb_and_password_only() {
        assert_eq!(
            suspected_secret("redis://:s3cret@host:6379/0"),
            Some("database-url")
        );
        assert_eq!(
            suspected_secret("rediss://:s3cret@host:6379/0"),
            Some("database-url")
        );
        assert_eq!(
            suspected_secret("mariadb://root:s3cret@host/db"),
            Some("database-url")
        );
        assert_eq!(
            suspected_secret("postgresql://:s3cret@host/db"),
            Some("database-url")
        );
        // Negative shapes: empty password and credential-less URLs stay clean.
        assert_eq!(suspected_secret("redis://host:6379/0"), None);
        assert_eq!(suspected_secret("mariadb://root@host/db"), None);
        assert_eq!(suspected_secret("redis://user:@host/0"), None);
    }

    #[test]
    fn jwt_negative_shapes_stay_undetected() {
        let seg = format!("eyJ{}", "a".repeat(10));
        let full = format!("{seg}.{seg}.{seg}");
        // eyJ prefix alone, two-segment and short-segment shapes, and a
        // mid-word false hit all stay undetected.
        for text in [
            "eyJhbGciOi".to_string(),
            format!("{seg}.{seg}"),
            format!("{}.{}.{}", "a".repeat(10), "b".repeat(10), "c".repeat(10)),
            format!("word{full}"),
        ] {
            assert_eq!(suspected_secret(&text), None, "{text}");
        }
    }

    #[test]
    fn boundary_rules() {
        let embedded = format!("task-{}", "a".repeat(24));
        assert_eq!(suspected_secret(&embedded), None);
        let at_boundary = format!("-- sk-{}", "a".repeat(24));
        assert_eq!(suspected_secret(&at_boundary), Some("openai-key"));
    }

    #[test]
    fn database_url_requires_a_password() {
        assert_eq!(suspected_secret("postgres://user:@host/db"), None);
        assert_eq!(
            suspected_secret("mysql://user:pass@host/db"),
            Some("database-url")
        );
    }

    #[test]
    fn bearer_is_case_insensitive() {
        let text = format!("bearer a1B2{}", "c".repeat(20));
        assert_eq!(suspected_secret(&text), Some("bearer-token"));
    }

    #[test]
    fn snake_kebab_prose_and_extra_db_schemes() {
        // Snake/kebab prose without digit/uppercase entropy must not trigger.
        assert_eq!(suspected_secret("sk-learn_pipeline_configuration"), None);
        assert_eq!(suspected_secret("sk-my-kebab-prose-identifier"), None);
        assert_eq!(suspected_secret("bearer token_with_snake_case_words"), None);
        assert_eq!(
            suspected_secret("Bearer some-kebab-prose-string-here"),
            None
        );

        // Extra schemes + whitespace-terminated authority.
        assert_eq!(
            suspected_secret("mongodb+srv://app:s3cret@cluster.example/db"),
            Some("database-url")
        );
        assert_eq!(
            suspected_secret("amqps://svc:s3cret@mq.example"),
            Some("database-url")
        );
        assert_eq!(
            suspected_secret("postgres://host/db then user:pass@later"),
            None
        );
    }
}
