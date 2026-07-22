//! Subscription OAuth tokens for the oracle: Grok (SuperGrok / X Premium)
//! and ChatGPT (Plus/Pro via the Codex OAuth client).
//!
//! The interactive login happens OFF the tablet — reuse the riddle project's
//! `riddle-login grok` (or chatgpt) on a computer, then copy the resulting
//! auth JSON next to the scribe binary as `scribe-auth.json` (a copied
//! `riddle-auth.json` is also picked up, same format). From then on scribe
//! refreshes tokens itself with the stored refresh token.
//!
//! File format (single JSON object):
//!   {"provider":"grok"|"chatgpt","access_token":"…","refresh_token":"…",
//!    "expires_at":<unix seconds>,"account_id":"…" (chatgpt only)}

use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// Public OAuth clients (not secrets): the same IDs the vendors' own CLIs use.
pub const GROK_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const GROK_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CHATGPT_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Refresh this many seconds before the recorded expiry.
const EXPIRY_SKEW: u64 = 120;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct TokenStore {
    path: PathBuf,
    pub provider: String,
    access: String,
    refresh: String,
    expires_at: u64,
    pub account_id: Option<String>,
}

impl TokenStore {
    /// SCRIBE_AUTH_FILE, else scribe-auth.json next to the binary, else a
    /// riddle-auth.json next to the binary (drop-in reuse from the riddle
    /// project — same format, same providers).
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("SCRIBE_AUTH_FILE") {
            return Some(PathBuf::from(p));
        }
        let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        let own = dir.join("scribe-auth.json");
        if own.exists() {
            return Some(own);
        }
        let riddle = dir.join("riddle-auth.json");
        if riddle.exists() {
            return Some(riddle);
        }
        Some(own)
    }

    pub fn load() -> Option<Self> {
        let path = Self::default_path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        let provider = json_str(&text, "provider")?;
        let access = json_str(&text, "access_token")?;
        let refresh = json_str(&text, "refresh_token")?;
        let expires_at = json_num(&text, "expires_at").unwrap_or(0);
        let account_id = json_str(&text, "account_id");
        eprintln!(
            "scribe: auth file {} loaded ({provider}, expires in {}s)",
            path.display(),
            expires_at.saturating_sub(now_secs())
        );
        Some(Self { path, provider, access, refresh, expires_at, account_id })
    }

    /// A currently-valid access token, refreshing through the provider's
    /// token endpoint when the stored one is stale. May hit the network —
    /// call off the input loop.
    pub fn bearer(&mut self) -> io::Result<String> {
        if now_secs() + EXPIRY_SKEW >= self.expires_at {
            self.refresh_now()?;
        }
        Ok(self.access.clone())
    }

    fn refresh_now(&mut self) -> io::Result<()> {
        let (url, client_id) = match self.provider.as_str() {
            "grok" => (GROK_TOKEN_URL, GROK_CLIENT_ID),
            "chatgpt" => (CHATGPT_TOKEN_URL, CHATGPT_CLIENT_ID),
            other => {
                return Err(io::Error::other(format!("unknown auth provider {other}")));
            }
        };
        eprintln!("scribe: refreshing {} access token", self.provider);
        let mut body = format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            form_encode(client_id),
            form_encode(&self.refresh)
        );
        // xAI stamps the subscription tier into the token only when the
        // refresh echoes the login flow's scope/plan/referrer; without them
        // the new token silently drops to the API-spend tier (rate limit 0).
        if self.provider == "grok" {
            body.push_str(
                "&scope=openid%20profile%20email%20offline_access%20grok-cli%3Aaccess%20api%3Aaccess\
                 &plan=generic&referrer=hermes-agent",
            );
        }
        let resp = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .post(url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("Accept", "application/json")
            .send_string(&body);
        let text = match resp {
            Ok(r) => r.into_string().unwrap_or_default(),
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                return Err(io::Error::other(format!(
                    "token refresh failed (http {code}): {} — redo the login on a computer",
                    detail.trim()
                )));
            }
            Err(e) => return Err(io::Error::other(format!("token refresh failed: {e}"))),
        };
        let access = json_str(&text, "access_token")
            .ok_or_else(|| io::Error::other("token refresh reply had no access_token"))?;
        // Some providers rotate the refresh token; keep the old one if not.
        if let Some(r) = json_str(&text, "refresh_token") {
            self.refresh = r;
        }
        let expires_in = json_num(&text, "expires_in").unwrap_or(3600);
        self.access = access;
        self.expires_at = now_secs() + expires_in;
        self.save();
        Ok(())
    }

    fn save(&self) {
        let mut out = String::from("{");
        out.push_str(&format!("\"provider\":{},", quote(&self.provider)));
        out.push_str(&format!("\"access_token\":{},", quote(&self.access)));
        out.push_str(&format!("\"refresh_token\":{},", quote(&self.refresh)));
        out.push_str(&format!("\"expires_at\":{}", self.expires_at));
        if let Some(a) = &self.account_id {
            out.push_str(&format!(",\"account_id\":{}", quote(a)));
        }
        out.push('}');
        if let Err(e) = std::fs::write(&self.path, out) {
            eprintln!("scribe: warning: could not save {}: {e}", self.path.display());
        }
    }
}

/// Extract a string field's (unescaped) value from a small JSON object.
pub fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let mut at = s.find(&pat)? + pat.len();
    // Tolerate whitespace after the colon.
    at += s[at..].len() - s[at..].trim_start().len();
    let rest = s.get(at..)?;
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = (0..4).filter_map(|_| chars.next()).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                other => out.push(other),
            },
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None
}

/// Extract a non-negative integer field from a small JSON object.
pub fn json_num(s: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\":");
    let at = s.find(&pat)? + pat.len();
    let digits: String = s[at..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Percent-encode a form value (application/x-www-form-urlencoded).
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_field_extraction() {
        let s = r#"{"provider":"grok","access_token":"abc","expires_at": 1234,"expires_in":3600}"#;
        assert_eq!(json_str(s, "provider").as_deref(), Some("grok"));
        assert_eq!(json_str(s, "access_token").as_deref(), Some("abc"));
        assert_eq!(json_num(s, "expires_at"), Some(1234));
        assert_eq!(json_num(s, "expires_in"), Some(3600));
        assert_eq!(json_str(s, "missing"), None);
    }

    #[test]
    fn form_encoding_is_conservative() {
        assert_eq!(form_encode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(form_encode("a+b/c=="), "a%2Bb%2Fc%3D%3D");
    }
}
