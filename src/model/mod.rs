pub mod anthropic;
pub mod deepseek;
mod messages;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Stage};

/// 模型输出无法解析时，最多尝试的次数。
const MAX_PARSE_ATTEMPTS: usize = 3;

/// 发给模型的一次请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: &'static str,
    pub user: String,
}

/// 调用模型需要的能力。每家厂商在各自的子模块中实现。
pub trait Model {
    /// 单次请求可容纳的输入 token 数。
    fn context_limit(&self) -> usize;

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error>;

    /// 让模型生成文本。`schema` 是期望输出的 JSON Schema，厂商支持时应该作为约束，
    /// 不支持时可以忽略，所以提示词里也要写明输出格式。
    ///
    /// 模型拒绝或输出被截断时必须返回错误，不能返回不完整的文本。
    fn generate(&self, prompt: &Prompt, schema: &Value) -> Result<String, Error>;
}

/// 核对上下文上限后调用模型，并把输出解析成 `T`。
///
/// 超限时直接报错，不做截断。不是每家厂商都支持输出格式约束，所以输出无法解析时会重试。
pub fn generate_json<T: DeserializeOwned>(
    model: &dyn Model,
    stage: Stage,
    prompt: &Prompt,
    schema: &Value,
) -> Result<T, Error> {
    let tokens = model.count_tokens(prompt)?;
    let limit = model.context_limit();
    if tokens > limit {
        return Err(Error::ContextOverflow {
            stage,
            tokens,
            limit,
        });
    }

    let mut last_error = None;
    for _ in 0..MAX_PARSE_ATTEMPTS {
        let text = model.generate(prompt, schema)?;
        match serde_json::from_str(strip_code_fence(&text)) {
            Ok(value) => return Ok(value),
            Err(e) => last_error = Some(e),
        }
    }
    Err(Error::Model(format!(
        "{stage}：无法解析模型的输出：{}",
        last_error.expect("至少尝试一次")
    )))
}

fn strip_code_fence(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|rest| rest.strip_suffix("```"))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Output {
        ids: Vec<String>,
    }

    struct FakeModel {
        outputs: RefCell<VecDeque<&'static str>>,
        calls: Cell<usize>,
        limit: usize,
    }

    impl FakeModel {
        fn new(outputs: &[&'static str]) -> Self {
            Self {
                outputs: RefCell::new(outputs.iter().copied().collect()),
                calls: Cell::new(0),
                limit: usize::MAX,
            }
        }

        fn run(&self) -> Result<Output, Error> {
            let prompt = Prompt {
                system: "系统",
                user: "用户".into(),
            };
            generate_json(self, Stage::Catalog, &prompt, &json!({}))
        }
    }

    impl Model for FakeModel {
        fn context_limit(&self) -> usize {
            self.limit
        }

        fn count_tokens(&self, _prompt: &Prompt) -> Result<usize, Error> {
            Ok(10)
        }

        fn generate(&self, _prompt: &Prompt, _schema: &Value) -> Result<String, Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.outputs.borrow_mut().pop_front().unwrap().to_owned())
        }
    }

    #[test]
    fn accepts_plain_and_fenced_json() {
        for text in [
            r#"{"ids":["a"]}"#,
            "  {\"ids\":[\"a\"]}\n",
            "```json\n{\"ids\":[\"a\"]}\n```",
            "```\n{\"ids\":[\"a\"]}\n```",
        ] {
            let output = FakeModel::new(&[text]).run().unwrap();
            assert_eq!(output.ids, ["a"], "{text}");
        }
    }

    #[test]
    fn retries_when_the_output_cannot_be_parsed() {
        // 不是 JSON，和 JSON 结构不对，都要重试。
        let model = FakeModel::new(&["a", r#"{"other":1}"#, r#"{"ids":["a"]}"#]);

        assert_eq!(model.run().unwrap().ids, ["a"]);
        assert_eq!(model.calls.get(), 3);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let model = FakeModel::new(&["a", "b", "c"]);

        let message = model.run().unwrap_err().to_string();

        assert!(message.contains("无法解析模型的输出"), "{message}");
        assert_eq!(model.calls.get(), MAX_PARSE_ATTEMPTS);
    }

    #[test]
    fn overflow_does_not_call_the_model() {
        let mut model = FakeModel::new(&[]);
        model.limit = 9;

        let err = model.run().unwrap_err();

        assert!(matches!(
            err,
            Error::ContextOverflow {
                tokens: 10,
                limit: 9,
                ..
            }
        ));
        assert_eq!(model.calls.get(), 0);
    }
}
