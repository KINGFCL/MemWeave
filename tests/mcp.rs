//! 启动真实的 `memweave mcp` 进程，按协议和它对话。
#![cfg(feature = "mcp")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const MODERN: &str = "2026-07-28";
const LEGACY: &str = "2025-11-25";

fn demo_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/demo-kb")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn demo_copy(dir: &TempDir) -> PathBuf {
    let kb = dir.path().join("kb");
    copy_dir(&demo_path(), &kb);
    kb
}

/// 一个真实的服务进程。`modern` 决定用哪一代协议。
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    modern: bool,
}

impl Client {
    fn start(kb: &Path, modern: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_memweave"))
            .arg("mcp")
            .arg(kb)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
            modern,
        };
        if !modern {
            client.initialize();
        }
        client
    }

    fn initialize(&mut self) -> Value {
        let result = self.raw_request(
            "initialize",
            json!({
                "protocolVersion": LEGACY,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1.0" }
            }),
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        result
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// stdout 上的每一行都必须是协议消息。
    fn raw_request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "服务提前退出了：{method}");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout 不是协议消息：{line:?}：{e}"));
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            if message["id"] == id {
                return message;
            }
        }
    }

    fn request(&mut self, method: &str, mut params: Value) -> Value {
        if self.modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": MODERN,
                "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            });
        }
        self.raw_request(method, params)
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }

    fn read(&mut self, script: &str) -> (bool, String) {
        split(&self.call("memory_read", json!({ "script": script })))
    }

    fn write(&mut self, script: &str) -> (bool, String) {
        split(&self.call("memory_write", json!({ "script": script })))
    }

    /// 关闭标准输入，服务必须自己退出。
    fn stop(mut self) {
        self.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(Instant::now() < deadline, "标准输入关闭后服务没有退出");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn split(result: &Value) -> (bool, String) {
    let text = result["content"][0]["text"].as_str().unwrap().to_owned();
    (result["isError"] != json!(true), text)
}

fn check_tools(client: &mut Client) {
    let response = client.request("tools/list", json!({}));
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["memory_read", "memory_write"]);
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], false);
}

#[test]
fn legacy_clients_get_the_instructions_from_initialize() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, false);

    let response = client.raw_request("ping", json!({}));
    assert!(response.get("error").is_none(), "{response}");
    check_tools(&mut client);
    let (ok, text) = client.read("revision catalog show");
    assert!(ok, "{text}");
    assert!(text.contains("Rust 内存管理"), "{text}");
    client.stop();

    let mut fresh = Client::start(&kb, true);
    fresh.modern = false;
    let response = fresh.initialize();
    let instructions = response["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("revision show manual-start"),
        "{instructions}"
    );
    assert_eq!(response["result"]["serverInfo"]["name"], "memweave");
    fresh.stop();
}

#[test]
fn modern_clients_get_the_instructions_from_discover() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let response = client.request("server/discover", json!({}));

    let result = &response["result"];
    assert!(
        result["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|version| version == MODERN),
        "{response}"
    );
    assert!(result["capabilities"]["tools"].is_object(), "{response}");
    assert!(
        result["instructions"]
            .as_str()
            .unwrap()
            .contains("revision show manual-start"),
        "{response}"
    );
    check_tools(&mut client);
    let (ok, text) = client.read("revision catalog show");
    assert!(ok, "{text}");
    client.stop();
}

#[test]
fn writes_survive_a_restart_and_read_tools_cannot_write() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.read("set ownership title \"新标题\"");
    assert!(!ok, "{text}");
    assert!(text.contains("error[E301]"), "{text}");

    let (ok, text) = client.write("set ownership title \"新标题\"");
    assert!(ok, "{text}");
    let (ok, text) = client.read("revision show ownership");
    assert!(ok && text.contains("新标题"), "{text}");
    client.stop();

    let mut restarted = Client::start(&kb, false);
    let (ok, text) = restarted.read("revision show ownership");
    assert!(ok && text.contains("新标题"), "{text}");
    restarted.stop();
}

#[test]
fn script_errors_are_tool_errors_the_model_can_read() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.write("remove borrowing");
    assert!(!ok);
    assert!(text.contains("error[E213]"), "{text}");
    assert!(text.contains("没有执行任何语句"), "{text}");

    // 工具不存在和参数不对是协议错误。
    let response = client.request("tools/call", json!({ "name": "没有", "arguments": {} }));
    assert!(response["error"].is_object(), "{response}");
    let response = client.request(
        "tools/call",
        json!({ "name": "memory_read", "arguments": {} }),
    );
    assert!(response["error"].is_object(), "{response}");
    let (ok, text) = client.read("revision catalog show");
    assert!(ok, "服务应该还活着：{text}");
    client.stop();
}

#[test]
fn the_manual_is_read_like_memory() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.read("revision show manual-start");
    assert!(ok && text.contains("这是你的长期记忆"), "{text}");
    assert!(text.contains("related: manual-recall"), "{text}");
    for (node, expected) in [
        ("manual-recall", "都由你决定"),
        ("manual-write", "什么该记"),
        ("manual-repair-conflict", "两条记忆互相矛盾"),
        ("manual-language", "catalog create"),
        ("manual-errors", "E217"),
    ] {
        let (ok, text) = client.read(&format!("revision show {node}"));
        assert!(ok && text.contains(expected), "{node}：{text}");
    }
    // 其他读语句也能用，所以模型学到的走法在说明里就能练。
    let (ok, text) = client.read("revision links manual-repair\nrevision evidence manual-repair\nrevision full_source manual-repair limit 100\nrevision related_by manual-repair");
    assert!(
        ok && text.contains("manual-repair -> manual-repair-delete"),
        "{text}"
    );
    // 写错了地址，提示里有内置节点。
    let (ok, text) = client.read("revision show manual-recal");
    assert!(!ok && text.contains("manual-recall"), "{text}");
    client.stop();
}

/// 全新的知识库里说明也在，那段话才总是有效。
#[test]
fn a_new_knowledge_base_already_has_the_manual() {
    let dir = TempDir::new().unwrap();
    let kb = dir.path().join("新记忆");
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.read("revision catalog show");
    assert!(ok, "{text}");
    assert!(text.contains("section \"记忆系统使用说明\""), "{text}");
    assert!(text.contains("catalog: 空"), "内置说明不算记忆：{text}");
    let (ok, text) = client.read("revision show manual-start\nrevision check\nrevision timeline");
    assert!(ok && text.contains("check: 没有问题"), "{text}");
    client.stop();
    assert!(
        !kb.join("nodes/manual-start.json").exists(),
        "说明不能进知识库"
    );
}

#[test]
fn the_manual_cannot_be_changed() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    for script in [
        "set manual-start desc \"\"\"改掉\"\"\"",
        "remove manual-start",
        "link drop -> manual-start",
        "link manual-start -> drop",
        "unlink manual-start -> manual-recall",
        "base drop on manual-start",
        "unbase manual-start on drop",
        "cite manual-start text \"\"\"改掉\"\"\"",
        "catalog add manual-start to \"Rust 并发\"",
        "catalog remove manual-start from \"记忆系统使用说明\"",
        "catalog add drop to \"记忆系统使用说明\"",
        "catalog create \"记忆系统使用说明\" desc \"\"\"同名\"\"\"",
        "catalog describe \"记忆系统使用说明\" \"\"\"改掉\"\"\"",
        "catalog rename \"Rust 并发\" to \"记忆系统使用说明\"",
        "catalog delete \"记忆系统使用说明\"",
        "source 新节点\n  title \"t\"\n  desc \"\"\"d\"\"\"\n  text \"\"\"原文\"\"\"\n  related manual-start\n  related_by drop\nend",
        "add 新节点\n  title \"t\"\n  desc \"\"\"d\"\"\"\n  based_on manual-start\n  related_by drop\nend",
        "source manual-extra\n  title \"t\"\n  desc \"\"\"冒充规则\"\"\"\n  text \"\"\"原文\"\"\"\n  related_by drop\nend",
        "add manual-extra\n  title \"t\"\n  desc \"\"\"冒充规则\"\"\"\n  based_on drop\n  related_by drop\nend",
    ] {
        let (ok, text) = client.write(script);
        assert!(!ok && text.contains("error[E223]"), "{script}：{text}");
    }
    let (ok, text) = client.read("revision check");
    assert!(ok && text.contains("check: 没有问题"), "{text}");
    client.stop();

    let journal = fs::read_to_string(kb.join("journal.jsonl")).unwrap_or_default();
    assert!(journal.is_empty(), "被拒绝的脚本不进日志：{journal}");
}

/// 说明不进知识库，所以导出、时间线和原文目录里都没有它。
#[test]
fn the_manual_stays_out_of_the_knowledge_base() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let target = dir.path().join("导出");
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.write(&format!(
        "revision show manual-start\nlog \"\"\"读完了说明\"\"\"\nrevision export \"{}\"",
        target.display()
    ));
    assert!(ok, "{text}");
    let (ok, text) = client.read("revision timeline");
    assert!(ok && !text.contains("manual-"), "{text}");
    client.stop();

    assert!(!target.join("nodes/manual-start.json").exists());
    let catalog = fs::read_to_string(target.join("catalog.json")).unwrap();
    assert!(!catalog.contains("记忆系统使用说明"), "{catalog}");
    let sources = fs::read_dir(kb.join("sources")).unwrap().count();
    assert_eq!(sources, 1, "demo 只有一份原文");

    // 重启后重放日志，读说明的语句不会妨碍加载。
    let mut client = Client::start(&kb, true);
    let (ok, text) = client.read("revision show manual-start");
    assert!(ok, "{text}");
    client.stop();
}

/// 旧版本可能建过这种节点。知识库要能打开，模型也要能把它们迁走。
#[test]
fn old_nodes_with_the_reserved_prefix_still_load_and_can_be_moved() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let script = "source manual-old\n  title \"旧节点\"\n  desc \"\"\"旧版本建的。\"\"\"\n  text \"\"\"旧原文\"\"\"\n  related_by drop\nend\n";
    let entry = json!({ "script": script, "at": 1_700_000_000u64 });
    fs::write(kb.join("journal.jsonl"), format!("{entry}\n")).unwrap();

    let mut client = Client::start(&kb, true);
    let (ok, text) = client.read("revision show manual-old\nrevision check");
    assert!(ok && text.contains("旧版本建的"), "{text}");
    assert!(
        text.contains("note: 节点 manual-old 用了内置说明的前缀"),
        "{text}"
    );
    assert!(text.contains("check: 没有问题"), "{text}");
    let (ok, text) = client.write("remove manual-old");
    assert!(ok, "{text}");
    let (ok, text) = client.read("revision check");
    assert!(ok && !text.contains("note:"), "{text}");
    client.stop();
}

#[test]
fn only_one_process_owns_a_knowledge_base() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    assert!(client.read("revision catalog show").0);

    let second = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .arg("mcp")
        .arg(&kb)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(second.stdout.is_empty());
    assert!(String::from_utf8_lossy(&second.stderr).contains("另一个进程正在写"));

    // 命令行写入也立即报错，不会一直等；读取不受影响。
    let write = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .args(["run", "--write"])
        .arg(&kb)
        .arg("set ownership title \"x\"")
        .output()
        .unwrap();
    assert!(!write.status.success());
    assert!(String::from_utf8_lossy(&write.stderr).contains("另一个进程正在写"));
    let read = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .arg("run")
        .arg(&kb)
        .arg("revision catalog show")
        .output()
        .unwrap();
    assert!(read.status.success());
    client.stop();

    // 服务退出后写入权释放了。
    let write = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .args(["run", "--write"])
        .arg(&kb)
        .arg("set ownership title \"x\"")
        .output()
        .unwrap();
    assert!(write.status.success());
}

#[test]
fn a_missing_or_empty_directory_becomes_an_empty_knowledge_base() {
    let dir = TempDir::new().unwrap();
    let empty = dir.path().join("空目录");
    fs::create_dir(&empty).unwrap();

    for kb in [dir.path().join("新目录/记忆"), empty] {
        let mut client = Client::start(&kb, true);
        let (ok, text) = client.read("revision catalog show\nrevision check");
        assert!(ok, "{text}");
        assert!(text.contains("catalog: 空"), "{text}");

        let script = "catalog create \"咖啡\" desc \"\"\"咖啡知识。\"\"\"\nsource a\n title \"t\"\n desc \"\"\"d\"\"\"\n text \"\"\"原文\"\"\"\nend\ncatalog add a to \"咖啡\"";
        let (ok, text) = client.write(script);
        assert!(ok, "{text}");
        client.stop();

        let mut restarted = Client::start(&kb, false);
        let (ok, text) = restarted.read("revision show a");
        assert!(ok && text.contains("原文"), "{text}");
        restarted.stop();
    }
}

#[test]
fn a_directory_that_is_not_a_knowledge_base_is_refused_and_left_alone() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("别的文件"), "内容").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .arg("mcp")
        .arg(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(dir.path().join("别的文件")).unwrap(),
        "内容"
    );
    assert!(!dir.path().join("catalog.json").exists());
}

#[test]
fn large_output_is_cut_at_the_limit_with_a_note() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut child = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .arg("mcp")
        .arg(&kb)
        .args(["--max-output-bytes", "300"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut client = Client {
        child,
        stdin,
        stdout,
        next_id: 0,
        modern: true,
    };

    let (ok, text) =
        client.read("revision show ownership\nrevision show borrowing\nrevision show drop");

    assert!(ok);
    assert!(text.contains("输出超过上限"), "{text}");
    assert!(
        text.contains("第 1 行的 revision show ownership 没有显示完"),
        "{text}"
    );
    assert!(text.contains("后面还有 2 条语句"), "{text}");
    assert!(!text.contains("revision show drop\n"), "{text}");
    client.stop();
}

fn all_text(result: &Value) -> Vec<String> {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| block["text"].as_str().unwrap().to_owned())
        .collect()
}

const ATTACHED: &str = "当前工作板和总表";

/// 脚本的结果和附带的内容。
fn call_blocks(client: &mut Client, tool: &str, script: &str) -> Vec<String> {
    all_text(&client.call(tool, json!({ "script": script })))
}

/// 这是这个行为存在的原因：一次查询走多少层，总表都只出现一次。
#[test]
fn walking_the_graph_does_not_repeat_the_catalog() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    // 第一次调用带上。
    let blocks = call_blocks(&mut client, "memory_read", "revision show ownership");
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(blocks[0].starts_with("node ownership"), "{blocks:?}");
    assert!(blocks[1].contains(ATTACHED), "{blocks:?}");
    assert!(
        blocks[1].contains("section \"Rust 内存管理\""),
        "{blocks:?}"
    );

    // 之后沿关联走多少层都不带。
    for script in [
        "revision show borrowing, drop",
        "revision show send-sync",
        "revision evidence send-sync",
        "revision check",
        "revision timeline",
    ] {
        let blocks = call_blocks(&mut client, "memory_read", script);
        assert_eq!(blocks.len(), 1, "{script}：{blocks:?}");
    }
    // 读说明也不带。
    let blocks = call_blocks(&mut client, "memory_read", "revision show manual-recall");
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    client.stop();
}

#[test]
fn the_first_call_brings_the_catalog_whatever_it_is() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);

    // 说明。
    let mut client = Client::start(&kb, true);
    let blocks = call_blocks(&mut client, "memory_read", "revision show manual-work");
    assert!(blocks[0].contains("工作板"), "{blocks:?}");
    assert!(blocks[1].contains(ATTACHED), "{blocks:?}");
    client.stop();

    // 报错的脚本。什么都没执行，但模型正好需要总表来改正。
    let mut client = Client::start(&kb, true);
    let result = client.call("memory_read", json!({ "script": "revision show 没有" }));
    assert_eq!(result["isError"], true);
    let blocks = all_text(&result);
    assert!(blocks[0].contains("error[E201]"), "{blocks:?}");
    assert!(blocks[1].contains(ATTACHED), "{blocks:?}");
    let result = client.call("memory_read", json!({ "script": "revision show 也没有" }));
    assert_eq!(all_text(&result).len(), 1);
    client.stop();

    // 写了 catalog show 但脚本报错时，输出里其实没有总表，要带上。
    let mut client = Client::start(&kb, true);
    let blocks = call_blocks(
        &mut client,
        "memory_read",
        "revision catalog show\nrevision show 没有",
    );
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    client.stop();
}

#[test]
fn listing_the_catalog_is_not_repeated() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    // 输出里已经有了，不再带，之后也不带。
    let blocks = call_blocks(&mut client, "memory_read", " revision catalog show\n");
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(
        blocks[0].contains("section \"Rust 内存管理\""),
        "{blocks:?}"
    );
    let blocks = call_blocks(&mut client, "memory_read", "revision show drop");
    assert_eq!(blocks.len(), 1, "{blocks:?}");

    // 上层每次查询开始时都可以再看一次。
    let blocks = call_blocks(
        &mut client,
        "memory_read",
        "revision catalog show\nrevision show drop",
    );
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(blocks[0].contains("section"), "{blocks:?}");
    client.stop();
}

#[test]
fn the_catalog_comes_back_only_when_it_changes() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    assert_eq!(
        call_blocks(&mut client, "memory_read", "revision check").len(),
        2
    );

    // 只写节点和 log，总表和工作板都没变。
    let note = "source a\n title \"t\"\n desc \"d\"\n text \"原文\"\n related_by drop\nend";
    for script in [note, "log \"做过\"", "set a desc \"新\"", "link a -> drop"] {
        let blocks = call_blocks(&mut client, "memory_write", script);
        assert_eq!(blocks.len(), 1, "{script}：{blocks:?}");
    }
    // 改了节点标题，总表里的标题变了。
    let blocks = call_blocks(
        &mut client,
        "memory_write",
        "set ownership title \"所有权规则\"",
    );
    assert!(blocks[1].contains("所有权规则"), "{blocks:?}");

    // 总表变了就带一次最新的，下一次就不带了。
    let blocks = call_blocks(
        &mut client,
        "memory_write",
        "catalog create \"新领域\" desc \"d\"",
    );
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(blocks[1].contains("section \"新领域\""), "{blocks:?}");
    assert_eq!(
        call_blocks(&mut client, "memory_read", "revision show drop").len(),
        1
    );

    // 工作板变了也一样。
    let goal = "goal create g \"目标\"\n desc \"d\"\n step \"一\"\n step \"二\"\nend";
    let blocks = call_blocks(&mut client, "memory_write", goal);
    assert!(blocks[1].contains("[>] 1 \"一\""), "{blocks:?}");
    let blocks = call_blocks(&mut client, "memory_write", "step done g 1");
    assert!(blocks[1].contains("[>] 2 \"二\""), "{blocks:?}");
    assert_eq!(
        call_blocks(&mut client, "memory_read", "revision show drop").len(),
        1
    );

    // 报错的脚本没有改变任何东西。
    let result = client.call(
        "memory_write",
        json!({ "script": "catalog create \"新领域\" desc \"d\"" }),
    );
    assert_eq!(result["isError"], true);
    assert_eq!(all_text(&result).len(), 1);

    // 输出里的总表已经最新，不重复；后面的语句又改了总表时，输出里的过时了，要带上。
    let blocks = call_blocks(
        &mut client,
        "memory_write",
        "catalog create \"甲\" desc \"d\"\nrevision catalog show",
    );
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let blocks = call_blocks(
        &mut client,
        "memory_write",
        "revision catalog show\ncatalog create \"乙\" desc \"d\"",
    );
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(blocks[1].contains("section \"乙\""), "{blocks:?}");
    client.stop();
}

#[test]
fn the_board_is_still_there_in_the_next_session() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    let (ok, text) = client.write(
        "goal create g \"目标\"\n desc \"d\"\n step \"一\"\n step \"二\"\nend\nstep done g 1",
    );
    assert!(ok, "{text}");
    client.stop();

    let mut next = Client::start(&kb, false);
    let blocks = all_text(&next.call("memory_read", json!({ "script": "revision check" })));
    assert!(blocks[1].contains("[x] 1 \"一\""), "{blocks:?}");
    assert!(blocks[1].contains("[>] 2 \"二\""), "{blocks:?}");
    next.stop();
}

#[test]
fn an_oversized_catalog_cannot_crowd_out_the_result() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut child = Command::new(env!("CARGO_BIN_EXE_memweave"))
        .arg("mcp")
        .arg(&kb)
        .args(["--max-output-bytes", "400"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut client = Client {
        child,
        stdin,
        stdout,
        next_id: 0,
        modern: true,
    };

    let blocks = all_text(&client.call("memory_read", json!({ "script": "revision check" })));

    assert!(blocks[0].contains("check: 没有问题"), "{blocks:?}");
    assert!(
        blocks[1].contains("用 revision catalog show 看全"),
        "{blocks:?}"
    );
    client.stop();
}

/// 用文件大小上限模拟磁盘写满：日志只写进去一部分。之后不能留下半行。
#[cfg(unix)]
#[test]
fn a_full_disk_leaves_the_journal_intact() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let bin = env!("CARGO_BIN_EXE_memweave");
    let run = |script: &str| {
        Command::new(bin)
            .args(["run", "--write"])
            .arg(&kb)
            .arg(script)
            .output()
            .unwrap()
    };
    assert!(run("log \"第一条\"\nsave").status.success());
    let journal = kb.join("journal.jsonl");
    let size = fs::metadata(&journal).unwrap().len();

    // ulimit -f 的单位是 512 字节。写入超过上限时 write 返回错误。
    let long = "很长的内容".repeat(400);
    let limited = Command::new("sh")
        .arg("-c")
        .arg("trap '' XFSZ; ulimit -f 1; exec \"$0\" run --write \"$1\" \"$2\"")
        .arg(bin)
        .arg(&kb)
        .arg(format!("log \"{long}\"\nsave"))
        .output()
        .unwrap();

    assert!(!limited.status.success());
    let text = String::from_utf8_lossy(&limited.stdout);
    assert!(text.contains("error[E903] 保存失败"), "{text}");
    assert_eq!(fs::metadata(&journal).unwrap().len(), size, "半行要去掉");
    assert!(run("log \"磁盘恢复后\"").status.success());
    let check = Command::new(bin)
        .arg("run")
        .arg(&kb)
        .arg("revision timeline")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&check.stdout);
    assert!(
        text.contains("磁盘恢复后") && text.contains("第一条"),
        "{text}"
    );
}

#[test]
fn a_damaged_knowledge_base_points_to_doctor_and_doctor_fixes_it() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    fs::write(kb.join("nodes/drop.json"), "{").unwrap();
    let bin = env!("CARGO_BIN_EXE_memweave");

    let server = Command::new(bin)
        .arg("mcp")
        .arg(&kb)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!server.status.success());
    assert!(String::from_utf8_lossy(&server.stderr).contains("memweave doctor"));
    assert!(server.stdout.is_empty());

    let report = Command::new(bin).arg("doctor").arg(&kb).output().unwrap();
    assert!(String::from_utf8_lossy(&report.stdout).contains("drop.json"));
    let fixed = Command::new(bin)
        .args(["doctor"])
        .arg(&kb)
        .arg("--fix")
        .output()
        .unwrap();
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );

    let mut client = Client::start(&kb, true);
    let (ok, text) = client.read("revision check");
    assert!(ok && text.contains("没有问题"), "{text}");
    client.stop();
}

#[test]
fn maintenance_waits_for_the_server_to_exit() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let bin = env!("CARGO_BIN_EXE_memweave");
    let mut client = Client::start(&kb, true);
    assert!(client.write("log \"x\"\nsave").0);

    let compact = Command::new(bin).arg("compact").arg(&kb).output().unwrap();
    assert!(!compact.status.success());
    assert!(String::from_utf8_lossy(&compact.stderr).contains("另一个进程正在写"));
    assert!(client.read("revision timeline").0);
    client.stop();

    let compact = Command::new(bin).arg("compact").arg(&kb).output().unwrap();
    assert!(
        compact.status.success(),
        "{}",
        String::from_utf8_lossy(&compact.stderr)
    );
    assert!(!kb.join("journal.jsonl").exists());
    let mut next = Client::start(&kb, true);
    assert!(next.read("revision timeline").1.contains("log x"));
    next.stop();
}

#[test]
fn the_draft_gate_works_over_the_tools() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let (ok, text) = client.read("revision display");
    assert!(ok && text.starts_with("has_revision: false"), "{text}");
    let (ok, text) = client.write("set drop title \"草稿里的标题\"");
    assert!(ok && text.contains("revision: 1 段脚本未保存"), "{text}");
    let (ok, text) = client.read("revision display");
    assert!(ok && text.starts_with("has_revision: true"), "{text}");
    assert!(text.contains("草稿里的标题"), "{text}");
    let (ok, text) = client.read("origin show drop");
    assert!(ok && !text.contains("草稿里的标题"), "{text}");
    assert!(!kb.join("journal.jsonl").exists(), "没有 save 就不写日志");

    // 只读工具不能保存。
    let (ok, text) = client.read("save");
    assert!(!ok && text.contains("E301"), "{text}");

    let (ok, text) = client.write("save");
    assert!(ok && text.contains("saved 1 段脚本"), "{text}");
    let (ok, text) = client.read("revision display");
    assert!(ok && text.starts_with("has_revision: false"), "{text}");
    client.stop();

    let mut restarted = Client::start(&kb, true);
    let (ok, text) = restarted.read("origin show drop");
    assert!(ok && text.contains("草稿里的标题"), "{text}");
    restarted.stop();
}

#[test]
fn unsaved_changes_wait_for_the_model_after_a_restart() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    assert!(client.write("set drop title \"没保存的标题\"").0);
    client.stop();

    let mut restarted = Client::start(&kb, true);
    // 第一次调用附带的总表上写着还有没保存的修改。
    let blocks = call_blocks(&mut restarted, "memory_read", "revision check");
    assert!(blocks[1].contains("revision: 1 段脚本未保存"), "{blocks:?}");
    let (ok, text) = restarted.read("revision show drop");
    assert!(ok && text.contains("没保存的标题"), "{text}");
    let (ok, text) = restarted.write("discard");
    assert!(ok && text.contains("discarded 1 段脚本"), "{text}");
    let (ok, text) = restarted.read("revision show drop");
    assert!(ok && !text.contains("没保存的标题"), "{text}");
    restarted.stop();
}

#[test]
fn the_command_line_carries_a_draft_across_calls() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let bin = env!("CARGO_BIN_EXE_memweave");
    let run = |flag: Option<&str>, script: &str| {
        let mut command = Command::new(bin);
        command.arg("run");
        if let Some(flag) = flag {
            command.arg(flag);
        }
        let output = command.arg(&kb).arg(script).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };

    run(Some("--write"), "set drop title \"第一次调用\"");
    run(Some("--write"), "set drop desc \"\"\"第二次调用\"\"\"");
    let text = run(None, "revision display");
    assert!(text.starts_with("has_revision: true\nscripts: 2"), "{text}");
    // 只读的调用不动草稿。
    assert!(kb.join("draft.jsonl").exists());
    assert!(!run(None, "origin show drop").contains("第一次调用"));
    let text = run(Some("--write"), "save");
    assert!(text.contains("saved 2 段脚本"), "{text}");
    let text = run(None, "origin show drop");
    assert!(
        text.contains("第一次调用") && text.contains("第二次调用"),
        "{text}"
    );
    assert_eq!(run(None, "revision display").trim(), "has_revision: false");
}
