//! MCP 服务：让模型通过工具调用操作自己的记忆。
//!
//! 调用方就是模型，它自己走图、判断和写记忆，所以服务不调用模型。图只属于一个线程：这里用一个
//! 专门的线程持有 [`Memory`]，tokio 只负责收发消息，脚本排队依次执行。

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
use crate::lang::exec::{render_overview, revision_note};
use crate::lang::journal::{Journal, WriteLock, load_current};
use crate::lang::{Mode, Output, Session};
use crate::maintain;
use crate::manual::{self, Manual};
use crate::memory::Memory;
use crate::parse::ParsedKnowledge;
use crate::parse::json::JsonParser;
use crate::source::file::FileSources;
use crate::source::in_memory::InMemorySources;

/// 工具输出的默认上限。客户端常见的上限是 25000 token，40 KiB 的中文大约是这个量。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 40 * 1024;

const READ_TOOL: &str = "memory_read";
const WRITE_TOOL: &str = "memory_write";
const ATTACHED_HEADER: &str = "--- 当前工作板和总表（第一次调用和它们变化时附带）---";

/// 在标准输入输出上提供服务，标准输入关闭时返回。
///
/// 服务运行期间一直持有写入权，否则内存里的图会和磁盘不一致。
pub fn serve(knowledge_base: &Path, max_output_bytes: usize) -> Result<(), Error> {
    maintain::recover_interrupted_swap(knowledge_base)?;
    create_if_empty(knowledge_base)?;
    let lock = WriteLock::acquire(knowledge_base)?;
    let sources = FileSources::new(knowledge_base);
    let mut memory = load_current(&JsonParser, &sources, knowledge_base).inspect_err(|_| {
        eprintln!(
            "运行 memweave doctor {0} 查看详情，doctor --fix 可以尽量恢复",
            knowledge_base.display()
        );
    })?;
    let journal = Journal::new(knowledge_base);
    journal.restore_draft(&mut memory, &sources, true);
    let manual = Manual::new()?;

    let (jobs, queue) = mpsc::channel();
    let worker = thread::spawn(move || {
        run_scripts(
            memory,
            &manual,
            &sources,
            &journal,
            max_output_bytes,
            &queue,
        );
    });

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
            goals: Vec::new(),
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

/// 脚本的结果和执行后的工作板、总表。
struct Reply {
    output: Output,
    /// 模型还没看到过的工作板和总表。
    overview: Option<String>,
}

struct Job {
    script: String,
    mode: Mode,
    reply: oneshot::Sender<Reply>,
}

fn run_scripts(
    mut memory: Memory,
    manual: &Manual,
    sources: &FileSources,
    journal: &Journal,
    max_output_bytes: usize,
    queue: &mpsc::Receiver<Job>,
) {
    // 模型最后一次看到的总表。一样就不再附带，所以一次查询走多少层，总表都不会重复出现。
    let mut shown: Option<Catalog> = None;
    for job in queue {
        let listed = listed(&job.script);
        let before = (listed == Listed::Before).then(|| memory.catalog().clone());
        let output = Session::new(&mut memory, sources, job.mode)
            .with_journal(journal)
            .with_manual(manual)
            .with_output_limit(max_output_bytes)
            .run(&job.script);
        let current = memory.catalog();
        // 脚本报错时什么都没执行，输出里没有总表。
        let in_output = output.ok
            && match listed {
                Listed::No => false,
                Listed::Last => true,
                // 后面的语句可能又改了总表，那样输出里的就过时了。
                Listed::Before => before.as_ref() == Some(current),
            };
        let overview = if in_output {
            shown = Some(current.clone());
            None
        } else if shown.as_ref() == Some(current) {
            None
        } else {
            shown = Some(current.clone());
            Some(attached_overview(&memory, manual, max_output_bytes))
        };
        // 客户端已经取消时没有人接收结果，脚本的效果不受影响。
        let _ = job.reply.send(Reply { output, overview });
    }
}

/// 脚本有没有写 `catalog show`，写在哪。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listed {
    No,
    Before,
    Last,
}

fn listed(script: &str) -> Listed {
    let is_listing = |line: &str| line.trim() == "revision catalog show";
    let mut lines = script.lines().filter(|line| !line.trim().is_empty());
    if lines.next_back().is_some_and(is_listing) {
        Listed::Last
    } else if lines.any(is_listing) {
        Listed::Before
    } else {
        Listed::No
    }
}

/// 附带的内容。最多占输出上限的一半，总能留出位置给脚本的结果。
fn attached_overview(memory: &Memory, manual: &Manual, max_output_bytes: usize) -> String {
    let mut text = render_overview(memory, Some(manual));
    // 重启后接上的草稿，模型靠这里才知道。
    if let Some(note) = revision_note(memory) {
        text = format!("{note}\n{text}");
    }
    let limit = max_output_bytes / 2;
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n（工作板和总表太长，被截断了，用 revision catalog show 看全）\n");
    }
    format!("{ATTACHED_HEADER}\n{text}")
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
        let reply = self.ask(script.to_owned(), mode).await?;
        let output = reply.output;
        let mut content = vec![ContentBlock::text(output.text)];
        content.extend(reply.overview.map(ContentBlock::text));
        // 脚本写错是模型能自己改正的错误，不是协议错误。
        Ok(if output.ok {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
        })
    }
}

impl Server {
    async fn ask(&self, script: String, mode: Mode) -> Result<Reply, ErrorData> {
        let (reply, result) = oneshot::channel();
        self.jobs
            .send(Job {
                script,
                mode,
                reply,
            })
            .map_err(|_| lost_worker())?;
        result.await.map_err(|_| lost_worker())
    }
}

fn lost_worker() -> ErrorData {
    ErrorData::internal_error("执行脚本的线程已经退出", None)
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
            other => {
                return Err(ErrorData::invalid_params(format!("没有工具 {other}"), None));
            }
        };
        Ok(result.into())
    }
}
