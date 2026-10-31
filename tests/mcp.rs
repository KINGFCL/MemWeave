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
    assert_eq!(names, ["memory_read", "memory_write", "memory_manual"]);
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[2]["annotations"]["readOnlyHint"], true);
}

#[test]
fn legacy_clients_get_the_instructions_from_initialize() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, false);

    let response = client.raw_request("ping", json!({}));
    assert!(response.get("error").is_none(), "{response}");
    check_tools(&mut client);
    let (ok, text) = client.read("catalog show");
    assert!(ok, "{text}");
    assert!(text.contains("Rust 内存管理"), "{text}");
    client.stop();

    let mut fresh = Client::start(&kb, true);
    fresh.modern = false;
    let response = fresh.initialize();
    let instructions = response["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("memory_manual"), "{instructions}");
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
            .contains("memory_manual"),
        "{response}"
    );
    check_tools(&mut client);
    let (ok, text) = client.read("catalog show");
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
    let (ok, text) = client.read("show ownership");
    assert!(ok && text.contains("新标题"), "{text}");
    client.stop();

    let mut restarted = Client::start(&kb, false);
    let (ok, text) = restarted.read("show ownership");
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
    let (ok, text) = client.read("catalog show");
    assert!(ok, "服务应该还活着：{text}");
    client.stop();
}

#[test]
fn the_manual_is_available_through_the_tool() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);

    let (ok, text) = split(&client.call("memory_manual", json!({})));
    assert!(ok && text.contains("这是你的长期记忆"), "{text}");
    for (topic, expected) in [
        ("recall", "都由你决定"),
        ("write", "什么该记"),
        ("repair", "两条记忆互相矛盾"),
        ("language", "catalog create"),
        ("errors", "E217"),
    ] {
        let (ok, text) = split(&client.call("memory_manual", json!({ "topic": topic })));
        assert!(ok && text.contains(expected), "{topic}：{text}");
    }
    let (ok, text) = split(&client.call("memory_manual", json!({ "topic": "没有" })));
    assert!(!ok && text.contains("可用的主题"), "{text}");
    client.stop();
}

#[test]
fn only_one_process_owns_a_knowledge_base() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    assert!(client.read("catalog show").0);

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
        .arg("catalog show")
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
        let (ok, text) = client.read("catalog show\ncheck");
        assert!(ok, "{text}");
        assert!(text.contains("catalog: 空"), "{text}");

        let script = "catalog create \"咖啡\" desc \"\"\"咖啡知识。\"\"\"\nsource a\n title \"t\"\n desc \"\"\"d\"\"\"\n text \"\"\"原文\"\"\"\nend\ncatalog add a to \"咖啡\"";
        let (ok, text) = client.write(script);
        assert!(ok, "{text}");
        client.stop();

        let mut restarted = Client::start(&kb, false);
        let (ok, text) = restarted.read("show a");
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

    let (ok, text) = client.read("show ownership\nshow borrowing\nshow drop");

    assert!(ok);
    assert!(text.contains("输出超过上限"), "{text}");
    assert!(
        text.contains("第 1 行的 show ownership 没有显示完"),
        "{text}"
    );
    assert!(text.contains("后面还有 2 条语句"), "{text}");
    assert!(!text.contains("show drop\n"), "{text}");
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

#[test]
fn every_call_comes_with_the_board_and_catalog() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let mut client = Client::start(&kb, true);
    let header = "当前工作板和总表（每次调用自动附带）";

    // 读、写、报错和说明书都带。
    let blocks = all_text(&client.call("memory_read", json!({ "script": "show drop" })));
    assert_eq!(blocks.len(), 2, "{blocks:?}");
    assert!(blocks[0].starts_with("node drop"), "{blocks:?}");
    assert!(blocks[1].contains(header), "{blocks:?}");
    assert!(
        blocks[1].contains("section \"Rust 内存管理\""),
        "{blocks:?}"
    );

    let blocks = all_text(&client.call(
        "memory_write",
        json!({ "script": "goal create g \"目标\"\n desc \"d\"\n step \"一\"\nend" }),
    ));
    assert!(blocks[1].contains("[>] 1 \"一\""), "{blocks:?}");
    assert!(blocks[1].contains("goal create g"), "{blocks:?}");

    let result = client.call("memory_write", json!({ "script": "show 没有" }));
    assert_eq!(result["isError"], true);
    let blocks = all_text(&result);
    assert!(blocks[0].contains("error[E201]"), "{blocks:?}");
    assert!(blocks[1].contains(header), "{blocks:?}");

    let blocks = all_text(&client.call("memory_manual", json!({ "topic": "work" })));
    assert!(blocks[0].contains("工作板"), "{blocks:?}");
    assert!(blocks[1].contains(header), "{blocks:?}");

    // 只查总表时，结果里已经有了。
    let blocks = all_text(&client.call("memory_read", json!({ "script": " catalog show\n" })));
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(blocks[0].contains("工作板"), "{blocks:?}");
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
    let blocks = all_text(&next.call("memory_read", json!({ "script": "check" })));
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

    let blocks = all_text(&client.call("memory_read", json!({ "script": "check" })));

    assert!(blocks[0].contains("check: 没有问题"), "{blocks:?}");
    assert!(blocks[1].contains("用 catalog show 看全"), "{blocks:?}");
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
    assert!(run("log \"第一条\"").status.success());
    let journal = kb.join("journal.jsonl");
    let size = fs::metadata(&journal).unwrap().len();

    // ulimit -f 的单位是 512 字节。写入超过上限时 write 返回错误。
    let long = "很长的内容".repeat(400);
    let limited = Command::new("sh")
        .arg("-c")
        .arg("trap '' XFSZ; ulimit -f 1; exec \"$0\" run --write \"$1\" \"$2\"")
        .arg(bin)
        .arg(&kb)
        .arg(format!("log \"{long}\""))
        .output()
        .unwrap();

    assert!(!limited.status.success());
    let text = String::from_utf8_lossy(&limited.stdout);
    assert!(text.contains("写入日志失败"), "{text}");
    assert_eq!(fs::metadata(&journal).unwrap().len(), size, "半行要去掉");
    assert!(run("log \"磁盘恢复后\"").status.success());
    let check = Command::new(bin)
        .arg("run")
        .arg(&kb)
        .arg("timeline")
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
    let (ok, text) = client.read("check");
    assert!(ok && text.contains("没有问题"), "{text}");
    client.stop();
}

#[test]
fn maintenance_waits_for_the_server_to_exit() {
    let dir = TempDir::new().unwrap();
    let kb = demo_copy(&dir);
    let bin = env!("CARGO_BIN_EXE_memweave");
    let mut client = Client::start(&kb, true);
    assert!(client.write("log \"x\"").0);

    let compact = Command::new(bin).arg("compact").arg(&kb).output().unwrap();
    assert!(!compact.status.success());
    assert!(String::from_utf8_lossy(&compact.stderr).contains("另一个进程正在写"));
    assert!(client.read("timeline").0);
    client.stop();

    let compact = Command::new(bin).arg("compact").arg(&kb).output().unwrap();
    assert!(
        compact.status.success(),
        "{}",
        String::from_utf8_lossy(&compact.stderr)
    );
    assert!(!kb.join("journal.jsonl").exists());
    let mut next = Client::start(&kb, true);
    assert!(next.read("timeline").1.contains("log x"));
    next.stop();
}
