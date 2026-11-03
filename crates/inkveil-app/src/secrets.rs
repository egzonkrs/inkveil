//! Spotting text that looks like API keys, passwords or other secrets, so nothing secret is sent
//! to the AI by accident. It errs on the side of warning: a false alarm costs one click, a missed
//! key can cost a lot more.

/// One thing that looks secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What it looks like, e.g. "GitHub token".
    pub kind: &'static str,
    /// The start and end of the value with the middle hidden, e.g. "sk-or-…9f2a".
    pub hint: String,
}

/// Known key formats: prefix, minimum total length, description.
const PREFIXES: &[(&str, usize, &str)] = &[
    ("sk-or-", 20, "OpenRouter API key"),
    ("sk-ant-", 20, "Anthropic API key"),
    ("sk-proj-", 20, "OpenAI API key"),
    ("sk-", 20, "API key (OpenAI, OpenRouter or similar)"),
    ("sk_live_", 16, "Stripe secret key"),
    ("sk_test_", 16, "Stripe test key"),
    ("rk_live_", 16, "Stripe restricted key"),
    ("whsec_", 16, "webhook signing secret"),
    ("ghp_", 20, "GitHub token"),
    ("gho_", 20, "GitHub token"),
    ("ghu_", 20, "GitHub token"),
    ("ghs_", 20, "GitHub token"),
    ("ghr_", 20, "GitHub token"),
    ("github_pat_", 20, "GitHub token"),
    ("glpat-", 20, "GitLab token"),
    ("xoxb-", 15, "Slack token"),
    ("xoxp-", 15, "Slack token"),
    ("xoxa-", 15, "Slack token"),
    ("xapp-", 15, "Slack token"),
    ("AIza", 35, "Google API key"),
    ("ya29.", 20, "Google access token"),
    ("hf_", 20, "Hugging Face token"),
    ("npm_", 30, "npm token"),
    ("pypi-", 30, "PyPI token"),
    ("SG.", 30, "SendGrid key"),
    ("shpat_", 20, "Shopify token"),
    ("dop_v1_", 30, "DigitalOcean token"),
    ("sq0atp-", 20, "Square token"),
    ("AKIA", 20, "AWS access key"),
    ("ASIA", 20, "AWS access key"),
];

/// Words that, followed by `=` or `:` and a value, usually introduce a secret.
const SECRET_NAMES: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "access_key",
    "secret_key",
    "private_key",
    "client_secret",
    "auth_token",
    "access_token",
    "refresh_token",
    "token",
    "bearer",
    "accountkey",
    "connectionstring",
];

/// Characters that end a token.
fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
        )
}

fn hint(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 10 {
        return "…".into();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Whether `token` looks like a random secret: long, few repeats, and a mix of letters and
/// digits (or other symbols).
fn looks_random(token: &str) -> bool {
    let len = token.chars().count();
    if len < 32 || token.contains("://") || token.contains('/') && !token.contains('=') {
        return false;
    }
    let letters = token.chars().filter(char::is_ascii_alphabetic).count();
    let digits = token.chars().filter(char::is_ascii_digit).count();
    let upper = token.chars().filter(char::is_ascii_uppercase).count();
    let lower = token.chars().filter(char::is_ascii_lowercase).count();
    let distinct = {
        let mut seen: Vec<char> = token.chars().collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    };
    let mixed = digits >= 4 && letters >= 8 || upper >= 4 && lower >= 4 && digits >= 2;
    mixed && distinct >= 16 && token.chars().all(|c| c.is_ascii_graphic())
}

/// Everything in `text` that looks like a secret, without duplicates.
pub fn scan(text: &str) -> Vec<Finding> {
    let mut found: Vec<Finding> = Vec::new();
    let mut push = |kind: &'static str, value: &str| {
        let finding = Finding {
            kind,
            hint: hint(value),
        };
        if !found.contains(&finding) {
            found.push(finding);
        }
    };

    if text.contains("PRIVATE KEY-----") {
        push("private key", "-----BEGIN PRIVATE KEY-----");
    }

    let tokens: Vec<&str> = text.split(is_separator).filter(|t| !t.is_empty()).collect();
    for (i, raw) in tokens.iter().enumerate() {
        // `name=value` and `name: value`.
        let lower = raw.to_ascii_lowercase();
        for name in SECRET_NAMES {
            let value = lower
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(['=', ':']))
                .map(|_| &raw[name.len() + 1..])
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    // `password: hunter22` with the value in the next token.
                    (lower == format!("{name}:") || lower == format!("{name}="))
                        .then(|| tokens.get(i + 1).copied())
                        .flatten()
                });
            if let Some(value) = value
                && value.chars().count() >= 6
            {
                push("password or secret value", value);
            }
        }

        let token = raw.trim_end_matches(['.', ':', '!', '?']);
        if let Some((_, _, kind)) = PREFIXES
            .iter()
            .find(|(prefix, min, _)| token.starts_with(prefix) && token.len() >= *min)
        {
            push(kind, token);
            continue;
        }
        // JSON web tokens: three base64 parts, the first starting with `eyJ`.
        if token.starts_with("eyJ") && token.split('.').count() == 3 && token.len() >= 40 {
            push("access token (JWT)", token);
            continue;
        }
        // Credentials inside a URL: scheme://user:password@host.
        if let Some(rest) = token.split_once("://").map(|(_, r)| r)
            && let Some((credentials, _)) = rest.split_once('@')
            && credentials.contains(':')
        {
            push("password in a link", token);
            continue;
        }
        if looks_random(token) {
            push("long random-looking string (possible key)", token);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<&'static str> {
        scan(text).into_iter().map(|f| f.kind).collect()
    }

    #[test]
    fn finds_common_keys() {
        assert_eq!(
            kinds("my key is sk-or-v1-0123456789abcdef0123456789abcdef ok"),
            ["OpenRouter API key"]
        );
        assert_eq!(
            kinds("ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            ["GitHub token"]
        );
        assert_eq!(kinds("AKIAIOSFODNN7EXAMPLE"), ["AWS access key"]);
        assert_eq!(kinds("-----BEGIN RSA PRIVATE KEY-----"), ["private key"]);
        assert_eq!(
            kinds("postgres://admin:hunter2secret@db.local/app"),
            ["password in a link"]
        );
    }

    #[test]
    fn finds_password_assignments() {
        assert_eq!(kinds("password=hunter22"), ["password or secret value"]);
        assert_eq!(kinds("API_KEY: abcd1234efgh"), ["password or secret value"]);
    }

    #[test]
    fn finds_random_strings_but_not_normal_text() {
        assert_eq!(
            kinds("token 9fA3kL0qZx7Pm2Rt8Wv5Yb1Nc4Hd6Je0Gs"),
            ["long random-looking string (possible key)"]
        );
        assert_eq!(
            kinds("I was going to make a joke, but instead I wanted to make a toy."),
            [] as [&str; 0]
        );
        assert_eq!(
            kinds("See https://example.com/docs/getting-started/installation-guide"),
            [] as [&str; 0]
        );
        assert_eq!(kinds("Ask her about the sk- prefix later"), [] as [&str; 0]);
    }

    #[test]
    fn hints_hide_the_middle() {
        let found = scan("sk-or-v1-0123456789abcdef0123456789abcdef");
        assert_eq!(found[0].hint, "sk-or-…cdef");
    }
}
