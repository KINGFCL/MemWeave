use std::env;
use std::process::ExitCode;

use memweave::anthropic::{AnthropicModel, DEFAULT_BASE_URL, DEFAULT_MODEL};
use memweave::retrieve::retrieve;
use memweave::store::FsStore;

const USAGE: &str = "\
用法：memweave query <知识库目录> <问题>

环境变量：
  ANTHROPIC_API_KEY   必填
  ANTHROPIC_BASE_URL  可选，默认 https://api.anthropic.com
  MEMWEAVE_MODEL      可选，默认 claude-opus-5-5";

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
    let api_key =
        env::var("ANTHROPIC_API_KEY").map_err(|_| "未设置 ANTHROPIC_API_KEY".to_owned())?;
    let base_url = env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned());
    let model_name = env::var("MEMWEAVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());

    let model =
        AnthropicModel::connect(&base_url, &api_key, &model_name).map_err(|e| e.to_string())?;
    let tree = retrieve(&FsStore::new(kb), &model, query).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())
}
