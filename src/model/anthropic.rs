use serde::Deserialize;
use serde_json::{Value, json};

use super::messages::MessagesClient;
use super::{Model, Prompt};
use crate::error::Error;

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_OUTPUT_TOKENS: u32 = 64_000;

/// 通过 Claude API 调用的模型。
pub struct AnthropicModel {
    client: MessagesClient,
    model: String,
    context_limit: usize,
}

impl AnthropicModel {
    /// 创建客户端。未指定 `context_limit` 时向 Models API 查询。
    pub fn connect(
        base_url: &str,
        api_key: &str,
        model: &str,
        context_limit: Option<usize>,
    ) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct ModelInfo {
            max_input_tokens: usize,
        }

        let client = MessagesClient::new(base_url, api_key);
        let context_limit = match context_limit {
            Some(limit) => limit,
            None => {
                let info: ModelInfo = client.get_json(&format!("/v1/models/{model}"))?;
                info.max_input_tokens
            }
        };
        Ok(Self {
            client,
            model: model.to_owned(),
            context_limit,
        })
    }
}

impl Model for AnthropicModel {
    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error> {
        self.client.count_tokens(&self.model, prompt)
    }

    fn generate(&self, prompt: &Prompt, schema: &Value) -> Result<String, Error> {
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "fallbacks": "default",
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
            "output_config": {
                "effort": "high",
                "format": { "type": "json_schema", "schema": schema },
            },
        });
        self.client.generate(body, Some(FALLBACK_BETA))
    }
}

#[cfg(test)]
mod tests {
    use super::super::messages::test_support::{serve, sse, stop, text_delta};
    use super::*;

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

        let model = AnthropicModel::connect(&url, "key", "m", None).unwrap();
        assert_eq!(model.context_limit(), 1000);
        assert_eq!(model.count_tokens(&prompt).unwrap(), 42);
        let schema = json!({ "type": "object" });
        assert_eq!(
            model.generate(&prompt, &schema).unwrap(),
            r#"{"ids":["a","b"]}"#
        );

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
        assert!(!count.headers.contains("anthropic-beta"));
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
        assert_eq!(body["output_config"]["format"]["schema"], schema);
    }

    #[test]
    fn explicit_context_limit_skips_the_models_api() {
        // 没有可连接的服务器：只要发出请求就会失败。
        let model = AnthropicModel::connect("http://127.0.0.1:1", "key", "m", Some(9)).unwrap();

        assert_eq!(model.context_limit(), 9);
    }
}
