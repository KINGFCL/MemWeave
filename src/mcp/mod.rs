//! MCP 服务：让模型通过工具调用操作自己的记忆。
//!
//! 调用方就是模型，它自己走图、判断和写记忆，所以服务不调用模型。图只属于一个线程：这里用一个
//! 专门的线程持有 [`Memory`]，tokio 只负责收发消息，脚本排队依次执行。

pub mod manual;

use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::error::Error;
use crate::export::json::JsonExporter;
use crate::export::{Exporter, target_problem};
use crate::knowledge::Catalog;
use crate::lang::journal::{Journal, WriteLock, load_current};
use crate::lang::{Mode, Output, Session};
use crate::memory::Memory;
use crate::parse::ParsedKnowledge;
use crate::parse::json::JsonParser;
use crate::source::file::FileSources;
use crate::source::in_memory::InMemorySources;

/// 工具输出的默认上限。客户端常见的上限是 25000 token，40 KiB 的中文大约是这个量。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 40 * 1024;

const READ_TOOL: &str = "memory_read";
const WRITE_TOOL: &str = "memory_write";
const MANUAL_TOOL: &str = "memory_manual";

/// 在标准输入输出上提供服务，标准输入关闭时返回。
///
/// 服务运行期间一直持有写入权，否则内存里的图会和磁盘不一致。
pub fn serve(knowledge_base: &Path, max_output_bytes: usize) -> Result<(), Error> {
    create_if_empty(knowledge_base)?;
    let lock = WriteLock::acquire(knowledge_base)?;
    let sources = FileSources::new(knowledge_base);
    let memory = load_current(&JsonParser, &sources, knowledge_base)?;
    let journal = Journal::new(knowledge_base);

    let (jobs, queue) = mpsc::channel();
    let worker =
        thread::spawn(move || run_scripts(memory, &sources, &journal, max_output_bytes, &queue));

    let server = Server { jobs };
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Mcp(e.to_string()))?
        .block_on(async move {
            let service = server
                .serve(rmcp::transport::stdio())
                .await
                .map_err(|e| Error::Mcp(e.to_string()))?;
            service
                .waiting()
                .await
                .map(|_| ())
                .map_err(|e| Error::Mcp(e.to_string()))
        });
    // server 已经释放，队列关闭，线程会把手上的脚本做完再退出。
    worker
        .join()
        .map_err(|_| Error::Mcp("执行脚本的线程崩溃了".to_owned()))?;
    drop(lock);
    result
}

/// 目录不存在或为空时新建空知识库。目录里已有别的内容时不动它，加载时会报错。
fn create_if_empty(knowledge_base: &Path) -> Result<(), Error> {
    if target_problem(knowledge_base).is_some() {
        return Ok(());
    }
    let parsed = ParsedKnowledge {
        catalog: Catalog {
            sections: Vec::new(),
        },
        nodes: Vec::new(),
    };
    let sources = InMemorySources::new();
    let memory = Memory::from_parsed(parsed, &sources)?;
    JsonExporter.export(&memory, &sources, knowledge_base)?;
    eprintln!("新建了空知识库：{}", knowledge_base.display());
    Ok(())
}

struct Job {
    script: String,
    mode: Mode,
    reply: oneshot::Sender<Output>,
}

fn run_scripts(
    mut memory: Memory,
    sources: &FileSources,
    journal: &Journal,
    max_output_bytes: usize,
    queue: &mpsc::Receiver<Job>,
) {
    for job in queue {
        let output = Session::new(&mut memory, sources, job.mode)
            .with_journal(journal)
            .with_output_limit(max_output_bytes)
            .run(&job.script);
        // 客户端已经取消时没有人接收结果，脚本的效果不受影响。
        let _ = job.reply.send(output);
    }
}

struct Server {
    jobs: mpsc::Sender<Job>,
}

impl Server {
    async fn run_script(
        &self,
        request: &CallToolRequestParams,
        mode: Mode,
    ) -> Result<CallToolResult, ErrorData> {
        let script = string_argument(request, "script")?
            .ok_or_else(|| ErrorData::invalid_params("缺少参数 script", None))?;
        let (reply, result) = oneshot::channel();
        let job = Job {
            script: script.to_owned(),
            mode,
            reply,
        };
        let lost = || ErrorData::internal_error("执行脚本的线程已经退出", None);
        self.jobs.send(job).map_err(|_| lost())?;
        let output = result.await.map_err(|_| lost())?;
        let content = vec![ContentBlock::text(output.text)];
        // 脚本写错是模型能自己改正的错误，不是协议错误。
        Ok(if output.ok {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
        })
    }
}

fn string_argument<'a>(
    request: &'a CallToolRequestParams,
    name: &str,
) -> Result<Option<&'a str>, ErrorData> {
    match request.arguments.as_ref().and_then(|args| args.get(name)) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(ErrorData::invalid_params(
            format!("参数 {name} 必须是字符串"),
            None,
        )),
    }
}

fn read_manual(request: &CallToolRequestParams) -> Result<CallToolResult, ErrorData> {
    let name = string_argument(request, "topic")?.unwrap_or("overview");
    Ok(match manual::topic(name) {
        Some(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        None => CallToolResult::error(vec![ContentBlock::text(format!(
            "没有主题 {name}。可用的主题：{}",
            manual::TOPICS.join("、")
        ))]),
    })
}

fn schema(value: Value) -> Arc<Map<String, Value>> {
    match value {
        Value::Object(object) => Arc::new(object),
        _ => unreachable!("工具的参数定义总是对象"),
    }
}

fn tools() -> Vec<Tool> {
    let script = || {
        schema(json!({
            "type": "object",
            "properties": {
                "script": { "type": "string", "description": "要执行的脚本，每条语句一行" }
            },
            "required": ["script"],
            "additionalProperties": false
        }))
    };
    vec![
        Tool::new(READ_TOOL, manual::READ_DESCRIPTION, script())
            .with_annotations(ToolAnnotations::new().read_only(true).open_world(false)),
        Tool::new(WRITE_TOOL, manual::WRITE_DESCRIPTION, script()).with_annotations(
            ToolAnnotations::new()
                .read_only(false)
                .destructive(true)
                .idempotent(false)
                .open_world(false),
        ),
        Tool::new(
            MANUAL_TOOL,
            manual::MANUAL_DESCRIPTION,
            schema(json!({
                "type": "object",
                "properties": {
                    "topic": { "type": "string", "enum": manual::TOPICS }
                },
                "additionalProperties": false
            })),
        )
        .with_annotations(ToolAnnotations::new().read_only(true).open_world(false)),
    ]
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("memweave", env!("CARGO_PKG_VERSION")))
            .with_instructions(manual::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = match request.name.as_ref() {
            READ_TOOL => self.run_script(&request, Mode::ReadOnly).await?,
            WRITE_TOOL => self.run_script(&request, Mode::ReadWrite).await?,
            MANUAL_TOOL => read_manual(&request)?,
            other => {
                return Err(ErrorData::invalid_params(format!("没有工具 {other}"), None));
            }
        };
        Ok(result.into())
    }
}
