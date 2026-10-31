use std::env;
use std::process::ExitCode;

use memweave::model::Model;
use memweave::model::anthropic::{self, AnthropicModel};
use memweave::model::deepseek::{self, DeepSeekModel};
use memweave::retrieve::retrieve;
use memweave::store::FsStore;

const USAGE: &str = "\
用法：memweave query <知识库目录> <问题>

环境变量：
  MEMWEAVE_PROVIDER       可选，模型厂商：anthropic（默认）或 deepseek
  MEMWEAVE_MODEL          可选，默认使用所选厂商的默认模型
  MEMWEAVE_CONTEXT_LIMIT  模型的输入 token 上限：
                          anthropic 可选，默认从 Models API 查询；deepseek 必填

  anthropic：ANTHROPIC_API_KEY 必填，ANTHROPIC_BASE_URL 可选
  deepseek： DEEPSEEK_API_KEY 必填，DEEPSEEK_BASE_URL 可选";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [command, kb, query] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if command != "query" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }

    match run(kb, query) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("错误：{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(kb: &str, query: &str) -> Result<String, String> {
    let model = connect_model()?;
    let tree = retrieve(&FsStore::new(kb), model.as_ref(), query).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())
}

/// 根据环境变量创建模型。接入新厂商时，在这里加一个分支。
fn connect_model() -> Result<Box<dyn Model>, String> {
    let provider = env_or("MEMWEAVE_PROVIDER", "anthropic");
    let context_limit = context_limit()?;

    match provider.as_str() {
        "anthropic" => {
            let api_key = required_env("ANTHROPIC_API_KEY")?;
            let base_url = env_or("ANTHROPIC_BASE_URL", anthropic::DEFAULT_BASE_URL);
            let model_name = env_or("MEMWEAVE_MODEL", anthropic::DEFAULT_MODEL);
            let model = AnthropicModel::connect(&base_url, &api_key, &model_name, context_limit)
                .map_err(|e| match context_limit {
                    Some(_) => e.to_string(),
                    None => {
                        format!("无法查询模型的上下文上限，可设置 MEMWEAVE_CONTEXT_LIMIT 指定：{e}")
                    }
                })?;
            Ok(Box::new(model))
        }
        "deepseek" => {
            let api_key = required_env("DEEPSEEK_API_KEY")?;
            let base_url = env_or("DEEPSEEK_BASE_URL", deepseek::DEFAULT_BASE_URL);
            let model_name = env_or("MEMWEAVE_MODEL", deepseek::DEFAULT_MODEL);
            let context_limit =
                context_limit.ok_or("使用 deepseek 时必须设置 MEMWEAVE_CONTEXT_LIMIT")?;
            Ok(Box::new(DeepSeekModel::new(
                &base_url,
                &api_key,
                &model_name,
                context_limit,
            )))
        }
        other => Err(format!(
            "不支持的模型厂商：{other}，可选 anthropic、deepseek"
        )),
    }
}

fn context_limit() -> Result<Option<usize>, String> {
    match env::var("MEMWEAVE_CONTEXT_LIMIT") {
        Ok(value) => value
            .parse()
            .map(Some)
            .map_err(|_| format!("MEMWEAVE_CONTEXT_LIMIT 不是有效的数字：{value}")),
        Err(_) => Ok(None),
    }
}

fn required_env(name: &str) -> Result<String, String> {
    env::var(name).map_err(|_| format!("未设置 {name}"))
}

fn env_or(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}
