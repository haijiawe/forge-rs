//! `web_fetch` and `web_search`: the two ways a turn reaches the public web.
//!
//! Both sit on `forge_net::Client`, which owns TLS, redirects, the deadline and
//! the byte budget. Nothing here parses HTML beyond turning it into text, and
//! nothing here decides that a non-2xx response is a failure: the status is data
//! the model reads, the same way a shell exit code is.

use std::sync::Arc;

use forge_llm::ToolSpec;
use serde_json::{json, Value};

use crate::contract::{decode_args, Call, Result, CODE_IO_ERROR, CODE_TIMEOUT, ERR_BAD_ARGS};
use crate::truncate::{truncate, Limits};
use crate::Tool;

/// The search endpoint answered with something that is not a result list.
pub const WEB_CODE_BAD_CONTENT: &str = "bad_content";

/// Where `web_search` sends its queries and what it authenticates with.
#[derive(Clone, Debug)]
pub struct SearchConfig {
    /// Full URL of the endpoint. The request is a JSON POST.
    pub endpoint: String,
    /// Sent as `Authorization: Bearer <key>` when non-empty.
    pub key: String,
}

/// Map a transport error to the code the model sees.
fn transport_code(message: &str) -> &'static str {
    if message.contains("deadline") || message.contains("timed out") {
        CODE_TIMEOUT
    } else {
        CODE_IO_ERROR
    }
}

/// Is this response body HTML?
///
/// Decided by the declared content type first, because that is what the server
/// meant; the sniff is the fallback for servers that send `text/plain` for a
/// page, which is common enough to be worth one line.
fn looks_like_html(content_type: Option<&str>, body: &str) -> bool {
    match content_type {
        Some(value) => {
            let value = value.to_ascii_lowercase();
            value.contains("text/html") || value.contains("application/xhtml")
        }
        None => {
            let head = body.trim_start();
            head.starts_with("<!doctype")
                || head.starts_with("<!DOCTYPE")
                || head.starts_with("<html")
        }
    }
}

// ---------------------------------------------------------------- web_fetch

#[derive(serde::Deserialize)]
struct FetchArgs {
    url: String,
    #[serde(default)]
    raw: bool,
}

/// Fetch one URL and return it as text.
pub struct WebFetchTool {
    limits: Limits,
    client: Arc<forge_net::Client>,
}

impl WebFetchTool {
    pub fn new(limits: Limits, client: Arc<forge_net::Client>) -> Self {
        Self { limits, client }
    }
}

impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".to_string(),
            description: "Fetch one URL over HTTP(S) and return its body as text.\n\n\
When to use: read a page or an API response you already have the address of. When you \
need to *find* an address, use web_search first: guessing a URL wastes a round trip.\n\n\
HTML is reduced to text (tags removed, script and style bodies dropped, entities \
decoded, blank lines collapsed). Anything else is returned as bytes decoded as UTF-8. \
Pass raw=true to keep the HTML source.\n\n\
Redirects are followed, up to five. A non-2xx status is not an error: the status line \
is the first line of the output and the call succeeds, so read it. Only a transport \
failure — DNS, TLS, connection, deadline — is an error.\n\n\
The body is capped (2 MiB by default) and the output is capped by the limits, so a \
huge page is truncated with the reason stated rather than silently cut."
                .to_string(),
            schema: json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string","description":"Absolute http:// or https:// URL."},
                    "raw":{"type":"boolean","description":"Return the body unmodified instead of extracting text from HTML. Defaults to false."}
                },
                "required":["url"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: FetchArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if !(args.url.starts_with("http://") || args.url.starts_with("https://")) {
            return Result::error(
                ERR_BAD_ARGS,
                "web_fetch: url must start with http:// or https://",
            );
        }

        let response = match self.client.get(&args.url) {
            Ok(response) => response,
            Err(error) => return Result::error(transport_code(error.message()), error.message()),
        };

        let body = response.text();
        let content_type = response.header("content-type");
        let rendered = if args.raw || !looks_like_html(content_type, &body) {
            body
        } else {
            forge_net::to_text(&body)
        };

        let mut output = format!("{} {}\n\n{}", response.status, response.url, rendered);
        if response.truncated {
            output.push_str("\n\n[truncated: the body exceeded the byte budget]");
        }
        let (output, truncated) = truncate(&output, self.limits);

        let mut result = Result::ok(output).with_meta("status", json!(response.status));
        result = result.with_meta("url", json!(response.url));
        if response.truncated || truncated {
            result = result.with_meta("truncated", json!(true));
        }
        result
    }
}

// --------------------------------------------------------------- web_search

#[derive(serde::Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    count: Option<u32>,
}

/// One search hit, in whatever field names the endpoint used.
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// The `web_search` tool.
pub struct WebSearchTool {
    limits: Limits,
    client: Arc<forge_net::Client>,
    config: SearchConfig,
}

impl WebSearchTool {
    pub fn new(limits: Limits, client: Arc<forge_net::Client>, config: SearchConfig) -> Self {
        Self {
            limits,
            client,
            config,
        }
    }
}

/// Pick the first string field among `names` from a JSON object.
fn field(object: &serde_json::Map<String, Value>, names: &[&str]) -> String {
    for name in names {
        if let Some(value) = object.get(*name) {
            if let Some(text) = value.as_str() {
                return text.to_string();
            }
        }
    }
    String::new()
}

/// Turn the endpoint's answer into hits.
///
/// Endpoints disagree about where the list lives (`results`, `data`, `items`,
/// `web.results`) and about field names (`url`/`link`, `snippet`/`description`/
/// `content`). Reading a list of shapes generously is cheaper than making the
/// operator configure a schema, and an unrecognised shape is reported instead of
/// being rendered as an empty answer that looks like "no results".
fn parse_hits(value: &Value) -> Option<Vec<Hit>> {
    let mut list: Option<&Vec<Value>> = None;
    if let Some(array) = value.as_array() {
        list = Some(array);
    } else if let Some(object) = value.as_object() {
        let mut candidates: Vec<&str> = vec!["results", "data", "items", "hits", "web"];
        for name in candidates.drain(..) {
            let found = object.get(name);
            if let Some(array) = found.and_then(Value::as_array) {
                list = Some(array);
                break;
            }
            if let Some(results) = found
                .and_then(Value::as_object)
                .and_then(|inner| inner.get("results"))
                .and_then(Value::as_array)
            {
                list = Some(results);
                break;
            }
        }
    }
    let list = list?;
    let hits = list
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let url = field(object, &["url", "link", "href"]);
            if url.is_empty() {
                return None;
            }
            let title = field(object, &["title", "name", "heading"]);
            let snippet = field(object, &["snippet", "description", "content", "text"]);
            Some(Hit {
                title,
                url,
                snippet,
            })
        })
        .collect::<Vec<_>>();
    Some(hits)
}

impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".to_string(),
            description: "Search the web and return the hits as a list of title, URL and \
snippet.\n\n\
When to use: you do not know which page holds the answer. Follow up with web_fetch on \
the hits you want to read: the snippet is a summary, not evidence.\n\n\
The results are whatever the configured endpoint returns, reordered only to put a URL \
after its title. If the endpoint reports failure, that is an error; if it answers with \
a shape this tool does not recognise, the call reports bad_content and includes the \
first part of the body so you can see what it actually said."
                .to_string(),
            schema: json!({
                "type":"object",
                "properties":{
                    "query":{"type":"string","description":"What to search for."},
                    "count":{"type":"integer","description":"How many hits to ask for. Defaults to the endpoint's own default; values above 20 are clamped."}
                },
                "required":["query"],
                "additionalProperties":false
            }),
        }
    }

    fn call(&self, call: Call) -> Result {
        let args: SearchArgs = match decode_args(&call) {
            Ok(args) => args,
            Err(result) => return result,
        };
        if args.query.trim().is_empty() {
            return Result::error(ERR_BAD_ARGS, "web_search: query must not be empty");
        }

        let count = args.count.unwrap_or(0).min(20);
        let mut payload = json!({ "query": args.query });
        if count > 0 {
            payload["count"] = json!(count);
        }
        let body = payload.to_string();

        let mut headers: Vec<(String, String)> = Vec::new();
        if !self.config.key.is_empty() {
            headers.push((
                "Authorization".to_string(),
                format!("Bearer {}", self.config.key),
            ));
        }

        let response = match self.client.post_json_with_headers(
            &self.config.endpoint,
            &headers,
            body.as_bytes(),
        ) {
            Ok(response) => response,
            Err(error) => return Result::error(transport_code(error.message()), error.message()),
        };

        if !response.is_success() {
            return Result::error(
                forge_net::status_slug(response.status),
                response.error_summary(),
            )
            .with_meta("status", json!(response.status));
        }

        let text = response.text();
        let value: Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                return Result::error(
                    WEB_CODE_BAD_CONTENT,
                    format!("web_search: the endpoint did not return JSON ({error})"),
                )
                .with_meta("status", json!(response.status));
            }
        };

        let hits = match parse_hits(&value) {
            Some(hits) => hits,
            None => {
                let head: String = text.chars().take(400).collect();
                return Result::error(
                    WEB_CODE_BAD_CONTENT,
                    format!(
                        "web_search: no result list in the answer; first 400 characters:\n{head}"
                    ),
                )
                .with_meta("status", json!(response.status));
            }
        };

        if hits.is_empty() {
            return Result::ok("no results").with_meta("count", json!(0));
        }

        let mut output = String::new();
        for (index, hit) in hits.iter().enumerate() {
            output.push_str(&format!("{}. {}\n   {}\n", index + 1, hit.title, hit.url));
            if !hit.snippet.is_empty() {
                output.push_str(&format!("   {}\n", hit.snippet));
            }
        }
        let (output, truncated) = truncate(&output, self.limits);
        let mut result = Result::ok(output).with_meta("count", json!(hits.len()));
        if truncated {
            result = result.with_meta("truncated", json!(true));
        }
        result
    }
}
