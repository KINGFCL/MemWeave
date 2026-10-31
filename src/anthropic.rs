use std::io::{BufRead, BufReader, Read};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Error;
use crate::knowledge::NodeId;
use crate::model::{Model, Prompt};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_OUTPUT_TOKENS: u32 = 64_000;
const MAX_RETRIES: u32 = 2;

/// 通过 Claude API 调用的模型。
pub struct AnthropicModel {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
    model: String,
    context_limit: usize,
}

/// 一次请求失败的结果，附带是否值得重试。
struct Failure {
    message: String,
    retryable: bool,
}

impl AnthropicModel {
    /// 创建客户端，并向 Models API 查询该模型的上下文上限。
    pub fn connect(base_url: &str, api_key: &str, model: &str) -> Result<Self, Error> {
        // 状态码自行处理，才能读到错误响应里的说明。
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut this = Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            context_limit: 0,
        };
        this.context_limit = this.fetch_context_limit()?;
        Ok(this)
    }

    fn fetch_context_limit(&self) -> Result<usize, Error> {
        #[derive(Deserialize)]
        struct ModelInfo {
            max_input_tokens: usize,
        }

        let url = format!("{}/v1/models/{}", self.base_url, self.model);
        let text = with_retries(|| {
            let response = self
                .agent
                .get(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .call()
                .map_err(transport_failure)?;
            read_ok_body(response)
        })?;
        let info: ModelInfo = parse_json(&text)?;
        Ok(info.max_input_tokens)
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        self.agent
            .post(format!("{}{path}", self.base_url))
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
    }
}

impl Model for AnthropicModel {
    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error> {
        #[derive(Deserialize)]
        struct TokenCount {
            input_tokens: usize,
        }

        let body = json!({
            "model": self.model,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
        });
        let text = with_retries(|| {
            let response = self
                .post("/v1/messages/count_tokens")
                .send(body.to_string())
                .map_err(transport_failure)?;
            read_ok_body(response)
        })?;
        let count: TokenCount = parse_json(&text)?;
        Ok(count.input_tokens)
    }

    fn choose(&self, prompt: &Prompt) -> Result<Vec<NodeId>, Error> {
        #[derive(Deserialize)]
        struct Choice {
            ids: Vec<NodeId>,
        }

        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "stream": true,
            "fallbacks": "default",
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
            "output_config": {
                "effort": "high",
                "format": {
                    "type": "json_schema",
                    "schema": {
                        "type": "object",
                        "properties": { "ids": { "type": "array", "items": { "type": "string" } } },
                        "required": ["ids"],
                        "additionalProperties": false,
                    },
                },
            },
        });
        let text = with_retries(|| {
            let response = self
                .post("/v1/messages")
                .header("anthropic-beta", FALLBACK_BETA)
                .send(body.to_string())
                .map_err(transport_failure)?;
            if !response.status().is_success() {
                return Err(status_failure(response));
            }
            read_stream(response.into_body().into_reader())
        })?;
        let choice: Choice = parse_json(&text)?;
        Ok(choice.ids)
    }
}

fn with_retries<T>(mut attempt: impl FnMut() -> Result<T, Failure>) -> Result<T, Error> {
    let mut retries = 0;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(failure) if failure.retryable && retries < MAX_RETRIES => {
                retries += 1;
                thread::sleep(Duration::from_secs(1 << retries));
            }
            Err(failure) => return Err(Error::Model(failure.message)),
        }
    }
}

fn transport_failure(error: ureq::Error) -> Failure {
    Failure {
        message: error.to_string(),
        retryable: true,
    }
}

fn read_ok_body(mut response: ureq::http::Response<ureq::Body>) -> Result<String, Failure> {
    if !response.status().is_success() {
        return Err(status_failure(response));
    }
    response
        .body_mut()
        .read_to_string()
        .map_err(transport_failure)
}

fn status_failure(mut response: ureq::http::Response<ureq::Body>) -> Failure {
    let code = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    Failure {
        message: format!("HTTP {code}：{}", api_error_message(&text)),
        retryable: matches!(code, 408 | 409 | 429) || code >= 500,
    }
}

fn api_error_message(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text.to_owned())
}

/// 读取 SSE 流，拼出模型输出的文本。
fn read_stream(reader: impl Read) -> Result<String, Failure> {
    let mut text = String::new();
    let mut stop_reason = None;

    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| Failure {
            message: e.to_string(),
            retryable: true,
        })?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let event: Value = serde_json::from_str(data.trim()).map_err(|e| Failure {
            message: format!("无法解析流事件：{e}"),
            retryable: false,
        })?;
        match event["type"].as_str() {
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                text.push_str(event["delta"]["text"].as_str().unwrap_or_default());
            }
            Some("message_delta") => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(reason.to_owned());
                }
            }
            Some("error") => {
                let kind = event["error"]["type"].as_str().unwrap_or_default();
                return Err(Failure {
                    message: format!("{kind}：{}", event["error"]["message"]),
                    retryable: matches!(
                        kind,
                        "overloaded_error" | "api_error" | "rate_limit_error"
                    ),
                });
            }
            _ => {}
        }
    }

    // 拒绝和截断都不能当作完整的选择结果。
    match stop_reason.as_deref() {
        Some("end_turn") => Ok(text),
        Some("refusal") => Err(Failure {
            message: "模型拒绝了这次请求".into(),
            retryable: false,
        }),
        Some("max_tokens") => Err(Failure {
            message: format!("模型输出达到 {MAX_OUTPUT_TOKENS} token 上限，结果不完整"),
            retryable: false,
        }),
        Some(other) => Err(Failure {
            message: format!("意外的结束原因：{other}"),
            retryable: false,
        }),
        None => Err(Failure {
            message: "响应流在结束前中断".into(),
            retryable: true,
        }),
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Model(format!("无法解析模型响应：{e}")))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    use super::*;

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: x\ndata: {e}\n\n"))
            .collect()
    }

    fn text_delta(text: &str) -> Value {
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } })
    }

    fn stop(reason: &str) -> Value {
        json!({ "type": "message_delta", "delta": { "stop_reason": reason }, "usage": { "output_tokens": 1 } })
    }

    #[test]
    fn stream_concatenates_text_and_skips_thinking() {
        let stream = sse(&[
            json!({ "type": "message_start", "message": {} }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "想" } }),
            text_delta("{\"ids\":"),
            text_delta("[\"a\"]}"),
            stop("end_turn"),
            json!({ "type": "message_stop" }),
        ]);

        let text = read_stream(stream.as_bytes()).ok().unwrap();

        assert_eq!(text, r#"{"ids":["a"]}"#);
    }

    #[test]
    fn stream_rejects_incomplete_results() {
        for (reason, retryable) in [("refusal", false), ("max_tokens", false)] {
            let stream = sse(&[text_delta("{\"ids\":"), stop(reason)]);
            let failure = read_stream(stream.as_bytes()).err().unwrap();
            assert_eq!(failure.retryable, retryable, "{reason}");
        }

        let truncated = sse(&[text_delta("{\"ids\":")]);
        assert!(read_stream(truncated.as_bytes()).err().unwrap().retryable);
    }

    #[test]
    fn stream_error_event_is_retryable_when_overloaded() {
        let stream = sse(&[
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "忙" } }),
        ]);
        assert!(read_stream(stream.as_bytes()).err().unwrap().retryable);

        let stream = sse(&[
            json!({ "type": "error", "error": { "type": "invalid_request_error", "message": "错" } }),
        ]);
        assert!(!read_stream(stream.as_bytes()).err().unwrap().retryable);
    }

    struct Recorded {
        request_line: String,
        headers: String,
        body: String,
    }

    fn read_request(stream: &mut TcpStream) -> Recorded {
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            let lower = line.to_lowercase();
            if let Some(value) = lower.strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            headers.push_str(&lower);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        Recorded {
            request_line,
            headers,
            body: String::from_utf8(body).unwrap(),
        }
    }

    /// 依次用预设响应回答请求的本地服务器。
    fn serve(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<Recorded>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                sender.send(read_request(&mut stream)).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (url, receiver)
    }

    #[test]
    fn talks_to_the_api() {
        let stream = sse(&[text_delta(r#"{"ids":["a","b"]}"#), stop("end_turn")]);
        let (url, requests) = serve(vec![
            (200, json!({ "max_input_tokens": 1000 }).to_string()),
            (200, json!({ "input_tokens": 42 }).to_string()),
            (200, stream),
        ]);
        let prompt = Prompt {
            system: "系统",
            user: "用户".into(),
        };

        let model = AnthropicModel::connect(&url, "key", "m").unwrap();
        assert_eq!(model.context_limit(), 1000);
        assert_eq!(model.count_tokens(&prompt).unwrap(), 42);
        assert_eq!(model.choose(&prompt).unwrap(), ["a", "b"]);

        let info = requests.recv().unwrap();
        assert!(info.request_line.starts_with("GET /v1/models/m "));
        assert!(info.headers.contains("x-api-key: key"));
        assert!(info.headers.contains("anthropic-version: 2026-10-21"));

        let count = requests.recv().unwrap();
        assert!(
            count
                .request_line
                .starts_with("POST /v1/messages/count_tokens ")
        );
        let body: Value = serde_json::from_str(&count.body).unwrap();
        assert_eq!(body["system"], "系统");
        assert_eq!(body["messages"][0]["content"], "用户");

        let choose = requests.recv().unwrap();
        assert!(choose.request_line.starts_with("POST /v1/messages "));
        assert!(
            choose
                .headers
                .contains("anthropic-beta: server-side-fallback-2026-07-01")
        );
        let body: Value = serde_json::from_str(&choose.body).unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    }

    #[test]
    fn retries_overloaded_then_succeeds() {
        let (url, _requests) = serve(vec![
            (529, json!({ "error": { "message": "忙" } }).to_string()),
            (200, json!({ "max_input_tokens": 7 }).to_string()),
        ]);

        let model = AnthropicModel::connect(&url, "key", "m").unwrap();

        assert_eq!(model.context_limit(), 7);
    }

    #[test]
    fn client_errors_fail_without_retry_and_keep_the_message() {
        let (url, _requests) = serve(vec![(
            401,
            json!({ "error": { "message": "密钥无效" } }).to_string(),
        )]);

        let err = AnthropicModel::connect(&url, "key", "m").err().unwrap();

        let message = err.to_string();
        assert!(
            message.contains("401") && message.contains("密钥无效"),
            "{message}"
        );
    }
}
