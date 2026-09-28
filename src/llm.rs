//! LLM client: one chat completion at a time against any OpenAI-compatible endpoint (OpenAI,
//! Anthropic's compatibility endpoint, OpenRouter, Ollama, llama.cpp, ...).

use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Longest `retry-after` worth waiting for once; a longer one ends the run's AI calls.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TOKENS: u32 = 400;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited{}", .retry_after.map(|d| format!(" (retry after {}s)", d.as_secs())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },
    #[error("the endpoint refused the key ({status}): {body}")]
    Auth { status: u16, body: String },
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("not a chat completion: {0}")]
    Malformed(String),
}

/// A model's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The message content.
    pub content: String,
    /// The endpoint said no requests are left (`x-ratelimit-remaining-requests: 0`).
    pub exhausted: bool,
}

/// An OpenAI-compatible chat completions endpoint.
pub struct Client {
    http: reqwest::Client,
    url: String,
    key: Option<String>,
    model: String,
    /// Cleared after the endpoint rejects a `json_schema` response format; later requests ask
    /// for a plain JSON object.
    json_schema: bool,
    /// Cleared after the endpoint rejects `temperature` or `max_tokens` (reasoning models do);
    /// later requests leave both out.
    sampling: bool,
}

impl Client {
    /// `base_url` is the API root, e.g. `https://api.openai.com/v1`; `key` is sent as a bearer
    /// token when set (local servers need none).
    pub fn new(base_url: &str, key: Option<String>, model: &str) -> Result<Self, LlmError> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            url: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            key,
            model: model.to_string(),
            json_schema: true,
            sampling: true,
        })
    }

    /// Ask for a JSON reply matching `schema` (named `name`). Waits out one `429` whose
    /// `retry-after` is at most a minute; any other `429` is [`LlmError::RateLimited`].
    pub async fn complete(
        &mut self,
        system: &str,
        user: &str,
        name: &str,
        schema: &Value,
    ) -> Result<Completion, LlmError> {
        let mut waited = false;
        loop {
            let format = match self.json_schema {
                true => json!({
                    "type": "json_schema",
                    "json_schema": { "name": name, "strict": true, "schema": schema },
                }),
                false => json!({ "type": "json_object" }),
            };
            let mut body = json!({
                "model": self.model,
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user },
                ],
                "response_format": format,
            });
            if self.sampling {
                body["temperature"] = json!(0);
                body["max_tokens"] = json!(MAX_TOKENS);
            }
            let mut request = self.http.post(&self.url).json(&body);
            if let Some(key) = &self.key {
                request = request.bearer_auth(key);
            }
            let response = request.send().await?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let text = response.text().await?;
            match status {
                200..=299 => return parse(&text, &headers),
                429 => {
                    let retry_after = headers
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse().ok())
                        .map(Duration::from_secs);
                    match retry_after {
                        Some(wait) if !waited && wait <= MAX_RETRY_WAIT => {
                            waited = true;
                            tokio::time::sleep(wait).await;
                        }
                        _ => return Err(LlmError::RateLimited { retry_after }),
                    }
                }
                401 | 403 => return Err(LlmError::Auth { status, body: text }),
                400 if self.json_schema && text.contains("response_format") => {
                    self.json_schema = false;
                }
                400 if self.sampling
                    && (text.contains("temperature") || text.contains("max_tokens")) =>
                {
                    self.sampling = false;
                }
                _ => return Err(LlmError::Http { status, body: text }),
            }
        }
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

fn parse(text: &str, headers: &reqwest::header::HeaderMap) -> Result<Completion, LlmError> {
    let response: ChatResponse =
        serde_json::from_str(text).map_err(|e| LlmError::Malformed(e.to_string()))?;
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| LlmError::Malformed("no message content".into()))?;
    let exhausted = headers
        .get("x-ratelimit-remaining-requests")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0");
    Ok(Completion { content, exhausted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn reply(content: &str) -> Value {
        json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
    }

    async fn ask(server: &MockServer) -> Result<Completion, LlmError> {
        let mut client = Client::new(&format!("{}/v1/", server.uri()), Some("k".into()), "m")?;
        client.complete("sys", "user", "answer", &json!({})).await
    }

    #[tokio::test]
    async fn returns_the_message_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer k"))
            .and(body_partial_json(json!({
                "model": "m",
                "temperature": 0,
                "response_format": { "type": "json_schema" },
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(reply("{}"))
                    .insert_header("x-ratelimit-remaining-requests", "0"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let got = ask(&server).await.unwrap();
        assert_eq!(
            got,
            Completion {
                content: "{}".into(),
                exhausted: true
            }
        );
    }

    #[tokio::test]
    async fn waits_out_a_short_retry_after_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply("ok")))
            .mount(&server)
            .await;
        assert_eq!(ask(&server).await.unwrap().content, "ok");
    }

    #[tokio::test]
    async fn gives_up_on_a_long_or_repeated_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
            .expect(1)
            .mount(&server)
            .await;
        let err = ask(&server).await.unwrap_err();
        assert!(matches!(
            err,
            LlmError::RateLimited {
                retry_after: Some(d)
            } if d.as_secs() == 3600
        ));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .expect(2)
            .mount(&server)
            .await;
        assert!(matches!(
            ask(&server).await.unwrap_err(),
            LlmError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn falls_back_to_a_json_object_when_json_schema_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({ "response_format": { "type": "json_schema" } }),
            ))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("unsupported response_format type"),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                json!({ "response_format": { "type": "json_object" } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply("ok")))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(ask(&server).await.unwrap().content, "ok");
    }

    #[tokio::test]
    async fn drops_temperature_and_max_tokens_when_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "max_tokens": 400 })))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "Unsupported parameter: 'max_tokens'. Use 'max_completion_tokens' instead.",
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply("ok")))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(ask(&server).await.unwrap().content, "ok");
        let requests = server.received_requests().await.unwrap();
        let last: Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(last.get("max_tokens").is_none() && last.get("temperature").is_none());
    }

    #[tokio::test]
    async fn reports_auth_and_malformed_responses() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;
        assert!(matches!(
            ask(&server).await.unwrap_err(),
            LlmError::Auth { status: 401, .. }
        ));

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("OK"))
            .mount(&server)
            .await;
        assert!(matches!(
            ask(&server).await.unwrap_err(),
            LlmError::Malformed(_)
        ));
    }
}
