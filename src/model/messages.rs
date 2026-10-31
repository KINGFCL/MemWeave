//! Anthropic Messages 协议的客户端，供使用这一协议的厂商共用。
//! 请求内容由各厂商自己决定，这里只负责传输、流式解析和重试。

use std::io::{BufRead, BufReader, Read};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Prompt;
use crate::error::Error;
use crate::knowledge::NodeId;

const API_VERSION: &str = "2026-10-21";
const MAX_RETRIES: u32 = 2;

pub(super) struct MessagesClient {
    agent: ureq::Agent,
    base_url: String,
    api_key: String,
}

/// 一次请求失败的结果，附带是否值得重试。
struct Failure {
    message: String,
    retryable: bool,
}

impl MessagesClient {
    pub(super) fn new(base_url: &str, api_key: &str) -> Self {
        // 状态码自行处理，才能读到错误响应里的说明。
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    pub(super) fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let url = format!("{}{path}", self.base_url);
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
        parse_json(&text)
    }

    pub(super) fn count_tokens(&self, model: &str, prompt: &Prompt) -> Result<usize, Error> {
        #[derive(Deserialize)]
        struct TokenCount {
            input_tokens: usize,
        }

        let body = json!({
            "model": model,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
        })
        .to_string();
        let text = with_retries(|| {
            let response = self
                .post("/v1/messages/count_tokens")
                .send(&body)
                .map_err(transport_failure)?;
            read_ok_body(response)
        })?;
        let count: TokenCount = parse_json(&text)?;
        Ok(count.input_tokens)
    }

    /// 发送一次流式请求并解析选择结果。`body` 不需要包含 `stream` 字段。
    pub(super) fn choose(&self, mut body: Value, beta: Option<&str>) -> Result<Vec<NodeId>, Error> {
        body["stream"] = json!(true);
        let body = body.to_string();
        with_retries(|| {
            let mut request = self.post("/v1/messages");
            if let Some(beta) = beta {
                request = request.header("anthropic-beta", beta);
            }
            let response = request.send(&body).map_err(transport_failure)?;
            if !response.status().is_success() {
                return Err(status_failure(response));
            }
            parse_choice(&read_stream(response.into_body().into_reader())?)
        })
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        self.agent
            .post(format!("{}{path}", self.base_url))
            .header("content-type", "application/json")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
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
            message: "模型输出达到 max_tokens 上限，结果不完整".into(),
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

/// 解析模型的选择结果。不是每家厂商都支持输出格式约束，所以容忍代码块包裹，
/// 其他格式问题交给重试。
fn parse_choice(text: &str) -> Result<Vec<NodeId>, Failure> {
    #[derive(Deserialize)]
    struct Choice {
        ids: Vec<NodeId>,
    }

    let text = text.trim();
    let body = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .unwrap_or(text);
    serde_json::from_str::<Choice>(body)
        .map(|choice| choice.ids)
        .map_err(|e| Failure {
            message: format!("无法解析模型的选择结果：{e}"),
            retryable: true,
        })
}

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Model(format!("无法解析模型响应：{e}")))
}

/// 各厂商测试共用的本地模拟服务器。
#[cfg(test)]
pub(super) mod test_support {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use serde_json::{Value, json};

    pub(crate) fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: x\ndata: {e}\n\n"))
            .collect()
    }

    pub(crate) fn text_delta(text: &str) -> Value {
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } })
    }

    pub(crate) fn stop(reason: &str) -> Value {
        json!({ "type": "message_delta", "delta": { "stop_reason": reason }, "usage": { "output_tokens": 1 } })
    }

    pub(crate) struct Recorded {
        pub(crate) request_line: String,
        pub(crate) headers: String,
        pub(crate) body: String,
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
    pub(crate) fn serve(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<Recorded>) {
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
}

#[cfg(test)]
mod tests {
    use super::test_support::{serve, sse, stop, text_delta};
    use super::*;

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

    #[test]
    fn parse_choice_accepts_plain_and_fenced_json() {
        for text in [
            r#"{"ids":["a"]}"#,
            "  {\"ids\":[\"a\"]}\n",
            "```json\n{\"ids\":[\"a\"]}\n```",
            "```\n{\"ids\":[\"a\"]}\n```",
        ] {
            assert_eq!(parse_choice(text).ok().unwrap(), ["a"], "{text}");
        }
        assert!(parse_choice("a").err().unwrap().retryable);
    }

    #[test]
    fn retries_when_the_model_ignores_the_output_format() {
        let (url, _requests) = serve(vec![
            (200, sse(&[text_delta("a"), stop("end_turn")])),
            (
                200,
                sse(&[text_delta(r#"{"ids":["a"]}"#), stop("end_turn")]),
            ),
        ]);
        let client = MessagesClient::new(&url, "key");

        assert_eq!(client.choose(json!({}), None).unwrap(), ["a"]);
    }

    #[test]
    fn retries_overloaded_then_succeeds() {
        let (url, _requests) = serve(vec![
            (529, json!({ "error": { "message": "忙" } }).to_string()),
            (200, json!({ "input_tokens": 7 }).to_string()),
        ]);
        let client = MessagesClient::new(&url, "key");
        let prompt = Prompt {
            system: "系统",
            user: "用户".into(),
        };

        assert_eq!(client.count_tokens("m", &prompt).unwrap(), 7);
    }

    #[test]
    fn client_errors_fail_without_retry_and_keep_the_message() {
        let (url, _requests) = serve(vec![(
            401,
            json!({ "error": { "message": "密钥无效" } }).to_string(),
        )]);
        let client = MessagesClient::new(&url, "key");

        let err = client.get_json::<Value>("/v1/models/m").unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("401") && message.contains("密钥无效"),
            "{message}"
        );
    }
}
