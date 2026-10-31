use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use memweave::lang::journal::Journal;
use memweave::lang::{self, Mode, Session};
use memweave::memory::Memory;
use memweave::model::Model;
use memweave::model::anthropic::{self, AnthropicModel};
use memweave::model::deepseek::{self, DeepSeekModel};
use memweave::parse::json::{self, JsonParser};
use memweave::retrieve::retrieve;
use memweave::source::file::FileSources;
use memweave::weave::Weaver;
use memweave::weave::text::TextWeaver;

const USAGE: &str = "\
用法：
  memweave weave <文本文件> <知识库目录>   调用模型把文本组织成新的知识库
  memweave query <知识库目录> <问题>       从知识库检索，返回知识树
  memweave evidence <知识库目录> <节点>    读取节点的证据原文，不调用模型
  memweave run [--write] <知识库目录> <脚本|->
                                          执行记忆指令，- 表示从标准输入读。默认只读，不调用模型
  memweave spec                           输出记忆指令的说明，用来放进模型的提示词

环境变量：
  MEMWEAVE_PROVIDER       可选，模型厂商：deepseek（默认）或 anthropic
  MEMWEAVE_MODEL          可选，默认使用所选厂商的默认模型
  MEMWEAVE_CONTEXT_LIMIT  模型的输入 token 上限：
                          anthropic 可选，默认从 Models API 查询；deepseek 必填

  deepseek： DEEPSEEK_API_KEY 必填，DEEPSEEK_BASE_URL 可选
  anthropic：ANTHROPIC_API_KEY 必填，ANTHROPIC_BASE_URL 可选";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let [command, rest @ ..] = args.as_slice()
        && command == "run"
    {
        return run_command(rest);
    }
    let result = match args.as_slice() {
        [command, input, kb] if command == "weave" => run_weave(input, kb),
        [command, kb, query] if command == "query" => run_query(kb, query),
        [command, kb, node] if command == "evidence" => run_evidence(kb, node),
        [command] if command == "spec" => Ok(lang::SPEC.to_owned()),
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
    let memory = load_memory(kb)?;
    let model = connect_model()?;
    let tree = retrieve(&memory, model.as_ref(), query).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())
}

fn run_weave(input: &str, kb: &str) -> Result<String, String> {
    let target = Path::new(kb);
    // 先确认能写入：目标不可用时不必调用模型。
    json::check_target(target).map_err(|e| e.to_string())?;
    let model = connect_model()?;
    let woven = TextWeaver
        .weave(Path::new(input), model.as_ref())
        .map_err(|e| e.to_string())?;
    let sections = woven.knowledge.catalog.sections.len();
    let memory = Memory::from_parsed(woven.knowledge, &woven.sources).map_err(|e| e.to_string())?;
    json::save(&memory, &woven.sources, target).map_err(|e| e.to_string())?;
    Ok(format!(
        "已生成 {} 个节点、{sections} 个板块，写入 {kb}",
        memory.node_count()
    ))
}

fn run_evidence(kb: &str, node: &str) -> Result<String, String> {
    let memory = load_memory(kb)?;
    let evidence = memory
        .evidence(node, &FileSources::new(Path::new(kb)))
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&evidence).map_err(|e| e.to_string())
}

fn run_command(args: &[String]) -> ExitCode {
    let (mode, kb, script) = match args {
        [flag, kb, script] if flag == "--write" => (Mode::ReadWrite, kb, script),
        [kb, script] => (Mode::ReadOnly, kb, script),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run_script(mode, kb, script) {
        // 报错也是给模型看的，和正常结果一样输出到标准输出。
        Ok(output) => {
            println!("{}", output.text);
            if output.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("错误：{message}");
            ExitCode::FAILURE
        }
    }
}

fn run_script(mode: Mode, kb: &str, script: &str) -> Result<lang::Output, String> {
    let script = if script == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("无法读取标准输入：{e}"))?;
        text
    } else {
        script.to_owned()
    };
    let path = Path::new(kb);
    // 写脚本是基于加载时的状态检查的，加载到写完日志之间不能有别的进程写入，否则日志重放会失败。
    let _lock = (mode == Mode::ReadWrite)
        .then(|| lock_for_writing(path))
        .transpose()?;
    let mut memory = load_memory(kb)?;
    let sources = FileSources::new(path);
    let journal = Journal::new(path);
    let output = Session::new(&mut memory, &sources, mode)
        .with_journal(&journal)
        .run(&script);
    Ok(output)
}

/// 独占知识库的写入权，返回的文件关闭时自动释放。
fn lock_for_writing(path: &Path) -> Result<File, String> {
    let file =
        File::create(path.join("journal.lock")).map_err(|e| format!("无法创建写入锁：{e}"))?;
    file.lock().map_err(|e| format!("无法获取写入锁：{e}"))?;
    Ok(file)
}

/// 加载知识库，并重放写入日志，恢复上次的修改。
fn load_memory(kb: &str) -> Result<Memory, String> {
    let path = Path::new(kb);
    let sources = FileSources::new(path);
    let mut memory = Memory::load(&JsonParser, &sources, path).map_err(|e| e.to_string())?;
    Journal::new(path)
        .replay(&mut memory, &sources)
        .map_err(|e| e.to_string())?;
    Ok(memory)
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
