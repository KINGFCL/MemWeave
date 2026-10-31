use std::env;
use std::path::Path;
use std::process::ExitCode;

use memweave::memory::Memory;
use memweave::model::Model;
use memweave::model::anthropic::{self, AnthropicModel};
use memweave::model::deepseek::{self, DeepSeekModel};
use memweave::parse::json::{self, JsonParser};
use memweave::retrieve::retrieve;
use memweave::weave::Weaver;
use memweave::weave::text::TextWeaver;

const USAGE: &str = "\
用法：
  memweave weave <文本文件> <知识库目录>   调用模型把文本组织成新的知识库
  memweave query <知识库目录> <问题>       从知识库检索，返回知识树

环境变量：
  MEMWEAVE_PROVIDER       可选，模型厂商：deepseek（默认）或 anthropic
  MEMWEAVE_MODEL          可选，默认使用所选厂商的默认模型
  MEMWEAVE_CONTEXT_LIMIT  模型的输入 token 上限：
                          anthropic 可选，默认从 Models API 查询；deepseek 必填

  deepseek： DEEPSEEK_API_KEY 必填，DEEPSEEK_BASE_URL 可选
  anthropic：ANTHROPIC_API_KEY 必填，ANTHROPIC_BASE_URL 可选";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.as_slice() {
        [command, input, kb] if command == "weave" => run_weave(input, kb),
        [command, kb, query] if command == "query" => run_query(kb, query),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
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

fn run_query(kb: &str, query: &str) -> Result<String, String> {
    // 先解析知识库：有问题就不必连接模型。
    let memory = Memory::load(&JsonParser, Path::new(kb)).map_err(|e| e.to_string())?;
    let model = connect_model()?;
    let tree = retrieve(&memory, model.as_ref(), query).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())
}

fn run_weave(input: &str, kb: &str) -> Result<String, String> {
    let target = Path::new(kb);
    // 先确认能写入：目标不可用时不必调用模型。
    json::check_target(target).map_err(|e| e.to_string())?;
    let model = connect_model()?;
    let knowledge = TextWeaver
        .weave(Path::new(input), model.as_ref())
        .map_err(|e| e.to_string())?;
    let sections = knowledge.catalog.sections.len();
    let memory = Memory::from_parsed(knowledge).map_err(|e| e.to_string())?;
    json::save(&memory, target).map_err(|e| e.to_string())?;
    Ok(format!(
        "已生成 {} 个节点、{sections} 个板块，写入 {kb}",
        memory.node_count()
    ))
}

/// 根据环境变量创建模型。接入新厂商时，在这里加一个分支。
fn connect_model() -> Result<Box<dyn Model>, String> {
    let provider = env_or("MEMWEAVE_PROVIDER", "deepseek");
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
