use serde_json::json;

use super::messages::MessagesClient;
use super::{Model, Prompt};
use crate::error::Error;
use crate::knowledge::NodeId;

pub const DEFAULT_MODEL: &str = "deepseek-flash";
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com/anthropic";

const MAX_OUTPUT_TOKENS: u32 = 64_000;

/// 通过 DeepSeek 的 Anthropic 格式接口调用的模型。选这个接口是因为它提供 token 计数。
///
/// 这个接口没有 Models API，所以上下文上限必须由调用方提供；
/// 实测它会忽略 `output_config.format`，也没有确认支持 `fallbacks`，
/// 所以请求里都不发送，输出格式只靠提示词约束。
pub struct DeepSeekModel {
    client: MessagesClient,
    model: String,
    context_limit: usize,
}

impl DeepSeekModel {
    pub fn new(base_url: &str, api_key: &str, model: &str, context_limit: usize) -> Self {
        Self {
            client: MessagesClient::new(base_url, api_key),
            model: model.to_owned(),
            context_limit,
        }
    }
}

impl Model for DeepSeekModel {
    fn context_limit(&self) -> usize {
        self.context_limit
    }

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error> {
        self.client.count_tokens(&self.model, prompt)
    }

    fn choose(&self, prompt: &Prompt) -> Result<Vec<NodeId>, Error> {
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": prompt.user }],
        });
        self.client.choose(body, None)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::messages::test_support::{serve, sse, stop, text_delta};
    use super::*;

    #[test]
    fn sends_only_what_the_service_supports() {
        let stream = sse(&[text_delta(r#"{"ids":["a"]}"#), stop("end_turn")]);
        let (url, requests) = serve(vec![
            (200, json!({ "input_tokens": 42 }).to_string()),
            (200, stream),
        ]);
        let prompt = Prompt {
            system: "系统",
            user: "用户".into(),
        };

        let model = DeepSeekModel::new(&url, "key", "m", 1000);
        assert_eq!(model.context_limit(), 1000);
        assert_eq!(model.count_tokens(&prompt).unwrap(), 42);
        assert_eq!(model.choose(&prompt).unwrap(), ["a"]);

        // 第一个请求就是计数：创建模型时不查询 Models API。
        let count = requests.recv().unwrap();
        assert!(
            count
                .request_line
                .starts_with("POST /v1/messages/count_tokens ")
        );

        let choose = requests.recv().unwrap();
        assert!(choose.request_line.starts_with("POST /v1/messages "));
        assert!(choose.headers.contains("x-api-key: key"));
        assert!(!choose.headers.contains("anthropic-beta"));
        let body: Value = serde_json::from_str(&choose.body).unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "系统");
        assert!(body.get("fallbacks").is_none());
        assert!(body.get("output_config").is_none());
    }
}
