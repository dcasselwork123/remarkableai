//! The oracle: a vision LLM that reads the circled handwriting and returns
//! the text to write back. Non-streaming — scribe can't start writing until
//! the old ink is erased anyway, so one blocking round-trip keeps it simple.
//!
//! Backends (chosen at startup):
//!  - `SCRIBE_OPENAI_KEY` set → any OpenAI-compatible `/chat/completions`
//!  - auth file present (grok) → xAI's API with the auto-refreshing
//!    subscription OAuth token — the same login riddle uses.
//!  - auth file present (chatgpt) → the Codex OAuth client's Responses-API
//!    dialect at `chatgpt.com/backend-api/codex/responses` (streaming SSE,
//!    accumulated into one blocking reply).

use std::io::{self, BufRead, BufReader};
use std::sync::{Arc, Mutex};

use crate::auth::{json_str, TokenStore};

const SYSTEM_PROMPT: &str = "You are Scribe, an AI assistant living inside a reMarkable e-ink \
notebook. You receive an image of the user's handwritten page. A straight horizontal rule near \
the bottom divides it: ABOVE the rule is page content; BELOW the rule is the COMMAND AREA, \
whose handwriting is always an instruction addressed to you. In the content, a hand-drawn loop \
(circle) marks the TARGET ink. Reply with EXACTLY the text that should replace the TARGET ink \
on the page — no preamble, no commentary, no markdown, no surrounding quotes.\n\
\n\
Rules:\n\
- If the command area contains an instruction (e.g. \"rewrite this politely\", \"translate to \
German\", \"solve this\", \"turn this into a list\", \"continue this thought\", \"draw a cat\"), \
carry it out on the TARGET ink and output only the result. Never repeat or include the \
instruction itself.\n\
- If the command area is empty or illegible, output a faithful, cleaned-up transcription of \
the TARGET ink only: the user's own words, made legible. Correct only obvious spelling slips; \
never rephrase.\n\
- If the TARGET ink itself is clearly an instruction addressed at you, execute it instead of \
transcribing it.\n\
- If the loop encloses blank paper, the user wants the instruction's output placed there: \
output the text (or DRAW drawing) that should fill that space.\n\
- If no loop is drawn, the target is all content above the rule.\n\
- Keep output compact: it will be re-written in handwriting by a slow pen. Prefer short lines; \
use \\n for line breaks and simple dashes for lists.\n\
- Plain characters only (letters, digits, common punctuation). No emoji, no markdown syntax.\n\
- If the instruction asks you to DRAW or sketch something (a shape, a diagram, a simple \
figure), do NOT use ASCII art. Instead reply with the word DRAW alone on the first line, then \
one polyline per line: space-separated x,y pairs in a 0-1000 coordinate space (origin \
top-left), straight segments between consecutive points, e.g. a square is:\n\
DRAW\n\
100,100 900,100 900,900 100,900 100,100\n\
Use up to 40 polylines; more points make smoother curves.\n\
- If the ink is illegible, output your best-guess transcription rather than an apology.\n\
- Answer in the same language as the handwriting.";

/// Authorization: a fixed API key, or an OAuth token store that refreshes
/// itself (Grok subscription).
#[derive(Clone)]
enum Cred {
    Key(String),
    OAuth(Arc<Mutex<TokenStore>>),
}

impl Cred {
    fn bearer(&self) -> io::Result<String> {
        match self {
            Cred::Key(k) => Ok(k.clone()),
            Cred::OAuth(s) => s.lock().unwrap().bearer(),
        }
    }
}

#[derive(Clone)]
pub struct Oracle {
    base: String, // e.g. https://api.x.ai/v1 (no trailing slash)
    cred: Cred,
    model: String,
    max_tokens: u32,
    reasoning: Option<String>,
    /// ChatGPT subscription: speak the Codex Responses dialect instead of
    /// `/chat/completions` (`base` is unused there — the URL is fixed).
    codex: bool,
}

impl Oracle {
    /// Pick a backend. `SCRIBE_ORACLE=openai|grok` forces one; otherwise
    /// `SCRIBE_OPENAI_KEY` → OpenAI-compatible, else the auth file's provider.
    pub fn from_env() -> io::Result<Self> {
        let forced = std::env::var("SCRIBE_ORACLE").ok().map(|s| s.to_lowercase());
        match forced.as_deref() {
            Some("openai" | "http") => return Self::openai(),
            Some(p @ ("grok" | "chatgpt" | "codex")) => {
                let store = TokenStore::load().ok_or_else(|| {
                    io::Error::other(format!(
                        "SCRIBE_ORACLE={p} but no auth file — copy scribe-auth.json (or \
                         riddle-auth.json from a `riddle-login {p}` run) next to the binary",
                    ))
                })?;
                let want = if p == "codex" { "chatgpt" } else { p };
                if store.provider != want {
                    return Err(io::Error::other(format!(
                        "SCRIBE_ORACLE={p} but the auth file's provider is {} — redo the \
                         login with `riddle-login {want}`",
                        store.provider
                    )));
                }
                return if want == "grok" { Self::grok(store) } else { Self::chatgpt(store) };
            }
            Some(other) => {
                return Err(io::Error::other(format!(
                    "unknown SCRIBE_ORACLE value {other} (openai|grok|chatgpt)"
                )));
            }
            None => {}
        }
        if std::env::var("SCRIBE_OPENAI_KEY").is_ok() {
            return Self::openai();
        }
        if let Some(store) = TokenStore::load() {
            return match store.provider.as_str() {
                "grok" => Self::grok(store),
                "chatgpt" => Self::chatgpt(store),
                other => Err(io::Error::other(format!(
                    "auth file provider {other} is not supported by scribe (grok|chatgpt); \
                     set SCRIBE_OPENAI_KEY instead"
                ))),
            };
        }
        Err(io::Error::other(
            "no oracle configured — set SCRIBE_OPENAI_KEY in oracle.env, or copy a grok or \
             chatgpt scribe-auth.json/riddle-auth.json next to the binary",
        ))
    }

    fn openai() -> io::Result<Self> {
        let key = std::env::var("SCRIBE_OPENAI_KEY")
            .map_err(|_| io::Error::other("SCRIBE_OPENAI_KEY not set"))?;
        let base = std::env::var("SCRIBE_OPENAI_BASE")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string())
            .trim_end_matches('/')
            .to_string();
        let model =
            std::env::var("SCRIBE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        eprintln!("scribe: oracle = OpenAI-compatible HTTP ({base}, {model})");
        Ok(Self {
            base,
            cred: Cred::Key(key),
            model,
            max_tokens: max_tokens_env(),
            reasoning: std::env::var("SCRIBE_REASONING").ok(),
            codex: false,
        })
    }

    fn grok(store: TokenStore) -> io::Result<Self> {
        let base = std::env::var("SCRIBE_GROK_BASE")
            .unwrap_or_else(|_| "https://api.x.ai/v1".to_string())
            .trim_end_matches('/')
            .to_string();
        // Must be vision-capable — it reads the handwriting PNG.
        let model = std::env::var("SCRIBE_GROK_MODEL").unwrap_or_else(|_| "grok-4.3".to_string());
        eprintln!("scribe: oracle = Grok subscription OAuth ({model})");
        Ok(Self {
            base,
            cred: Cred::OAuth(Arc::new(Mutex::new(store))),
            model,
            max_tokens: max_tokens_env(),
            reasoning: std::env::var("SCRIBE_REASONING").ok(),
            codex: false,
        })
    }

    fn chatgpt(store: TokenStore) -> io::Result<Self> {
        // Must be a vision-capable Codex-endpoint model.
        let model =
            std::env::var("SCRIBE_CHATGPT_MODEL").unwrap_or_else(|_| "gpt-5.1".to_string());
        eprintln!("scribe: oracle = ChatGPT subscription OAuth ({model})");
        Ok(Self {
            base: String::new(),
            cred: Cred::OAuth(Arc::new(Mutex::new(store))),
            model,
            max_tokens: max_tokens_env(),
            // "low" keeps the pen moving; "off" omits the field entirely.
            reasoning: Some(std::env::var("SCRIBE_REASONING").unwrap_or_else(|_| "low".into())),
            codex: true,
        })
    }

    /// One blocking round-trip: PNG in, reply text out. Call from a worker
    /// thread — token refresh + inference can take many seconds.
    pub fn ask(&self, png: &[u8]) -> io::Result<String> {
        if self.codex {
            return self.ask_codex(png);
        }
        let key = self.cred.bearer()?;
        let img = base64(png);
        let reasoning_field = self
            .reasoning
            .as_deref()
            .filter(|r| !r.is_empty() && *r != "off")
            .map(|r| format!("\"reasoning_effort\":{},", json_quote(r)))
            .unwrap_or_default();

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build();

        // The token-cap field is provider-dependent (OpenAI's newest models
        // demand max_completion_tokens; most compatible servers only know
        // max_tokens). Send the common one first; retry once if corrected.
        let request = |cap_field: &str| {
            let body = format!(
                concat!(
                    "{{\"model\":{},\"stream\":false,\"{}\":{},{}",
                    "\"messages\":[",
                    "{{\"role\":\"system\",\"content\":{}}},",
                    "{{\"role\":\"user\",\"content\":[",
                    "{{\"type\":\"text\",\"text\":\"Here is the circled region.\"}},",
                    "{{\"type\":\"image_url\",\"image_url\":{{\"url\":\"data:image/png;base64,{}\"}}}}",
                    "]}}]}}"
                ),
                json_quote(&self.model),
                cap_field,
                self.max_tokens,
                reasoning_field,
                json_quote(SYSTEM_PROMPT),
                img,
            );
            agent
                .post(&format!("{}/chat/completions", self.base))
                .set("Authorization", &format!("Bearer {key}"))
                .set("Content-Type", "application/json")
                .send_string(&body)
        };

        let asked = std::time::Instant::now();
        let resp = match request("max_tokens") {
            Err(ureq::Error::Status(400, r)) => {
                let detail = r.into_string().unwrap_or_default();
                if detail.contains("max_completion_tokens") {
                    request("max_completion_tokens")
                } else {
                    return Err(io::Error::other(format!("http 400: {}", detail.trim())));
                }
            }
            other => other,
        };
        let text = match resp {
            Ok(r) => r.into_string().map_err(io::Error::other)?,
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                return Err(io::Error::other(format!("http {code}: {}", detail.trim())));
            }
            Err(e) => return Err(io::Error::other(format!("request failed: {e}"))),
        };
        let reply = extract_content(&text)
            .ok_or_else(|| io::Error::other(format!("no content in reply: {}", clip(&text, 300))))?;
        eprintln!("scribe: oracle replied in {}ms ({} chars)", asked.elapsed().as_millis(), reply.len());
        let reply = reply.trim();
        if reply.is_empty() {
            return Err(io::Error::other("empty reply"));
        }
        Ok(reply.to_string())
    }

    /// ChatGPT subscription round-trip (Codex Responses dialect). The backend
    /// only streams; the SSE deltas are accumulated into one reply. Ported
    /// from riddle's CodexOracle.
    fn ask_codex(&self, png: &[u8]) -> io::Result<String> {
        let (key, account) = match &self.cred {
            Cred::OAuth(s) => {
                let mut s = s.lock().unwrap();
                (s.bearer()?, s.account_id.clone())
            }
            Cred::Key(k) => (k.clone(), None),
        };
        let img = base64(png);
        let reasoning_field = self
            .reasoning
            .as_deref()
            .filter(|r| !r.is_empty() && *r != "off")
            .map(|r| format!("\"reasoning\":{{\"effort\":{}}},", json_quote(r)))
            .unwrap_or_default();
        // The ChatGPT backend requires store=false; the include keeps the
        // stateless turn self-contained.
        let body = format!(
            concat!(
                "{{\"model\":{},\"stream\":true,\"store\":false,{}",
                "\"include\":[\"reasoning.encrypted_content\"],",
                "\"instructions\":{},",
                "\"input\":[",
                "{{\"role\":\"user\",\"content\":[",
                "{{\"type\":\"input_text\",\"text\":\"Here is the circled region.\"}},",
                "{{\"type\":\"input_image\",\"image_url\":\"data:image/png;base64,{}\"}}",
                "]}}]}}"
            ),
            json_quote(&self.model),
            reasoning_field,
            json_quote(SYSTEM_PROMPT),
            img,
        );

        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build();
        // A per-process session id helps the backend route the stream.
        let session =
            format!("scribe-{}-{}", std::process::id(), crate::auth::now_secs());
        let mut req = agent
            .post("https://chatgpt.com/backend-api/codex/responses")
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .set("Accept", "text/event-stream")
            .set("OpenAI-Beta", "responses=experimental")
            .set("originator", "codex_cli_rs")
            .set("session_id", &session);
        if let Some(a) = &account {
            req = req.set("chatgpt-account-id", a);
        }

        let asked = std::time::Instant::now();
        let reader = match req.send_string(&body) {
            Ok(r) => r.into_reader(),
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                return Err(io::Error::other(format!("http {code}: {}", detail.trim())));
            }
            Err(e) => return Err(io::Error::other(format!("request failed: {e}"))),
        };

        let mut acc = String::new();
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            match json_str(data, "type").as_deref() {
                Some("response.output_text.delta") => {
                    if let Some(frag) = json_str(data, "delta") {
                        acc.push_str(&frag);
                    }
                }
                Some("response.failed") | Some("error") => {
                    let msg =
                        json_str(data, "message").unwrap_or_else(|| "the oracle failed".into());
                    return Err(io::Error::other(msg));
                }
                Some("response.completed") => break,
                _ => {}
            }
        }
        eprintln!(
            "scribe: oracle replied in {}ms ({} chars)",
            asked.elapsed().as_millis(),
            acc.len()
        );
        let reply = acc.trim();
        if reply.is_empty() {
            return Err(io::Error::other("empty reply"));
        }
        Ok(reply.to_string())
    }
}

fn max_tokens_env() -> u32 {
    std::env::var("SCRIBE_MAX_TOKENS").ok().and_then(|v| v.parse().ok()).unwrap_or(1200)
}

/// Pull choices[0].message.content out of a chat-completions response.
fn extract_content(body: &str) -> Option<String> {
    // Anchor on the "message" object so a "content" elsewhere (e.g. inside
    // usage/logprobs) can't be matched by accident.
    let at = body.find("\"message\"")?;
    json_str(&body[at..], "content")
}

fn clip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_reply_content() {
        let body = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":"Buy milk\n- eggs"},"finish_reason":"stop"}],"usage":{"content":"red-herring"}}"#;
        assert_eq!(extract_content(body).as_deref(), Some("Buy milk\n- eggs"));
    }

    #[test]
    fn missing_content_is_none() {
        assert_eq!(extract_content(r#"{"error":{"message":"nope"}}"#), None);
    }

    #[test]
    fn base64_matches_known_vector() {
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"f"), "Zg==");
    }

    #[test]
    fn json_quote_escapes() {
        assert_eq!(json_quote("a\"b\nc"), "\"a\\\"b\\nc\"");
    }
}
