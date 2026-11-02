//! 测量从总表出发广度优先走完全图要多久，走法和模型一样：
//!
//!     cargo run --release --example bench_bfs
//!     cargo run --release --example bench_bfs -- --kb <知识库目录>
//!
//! 先 `catalog show` 拿到代表节点，每层用脚本 `show` 读完，从输出里解析 `related` 得到下一层，
//! 读过的不再读。不调用模型，所以这里只有系统的时间；模型的时间按 10000 token/s 另外估算。

use std::collections::HashSet;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use memweave::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use memweave::lang::journal::load_current;
use memweave::lang::{Mode, Session};
use memweave::manual::Manual;
use memweave::memory::{Memory, NodeRef};
use memweave::parse::ParsedKnowledge;
use memweave::parse::json::JsonParser;
use memweave::source::SourceStore;
use memweave::source::file::FileSources;
use tempfile::TempDir;

const COUNTS: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
const LINKS: usize = 4;
const CLUSTER_RADIUS: usize = 32;
const SECTIONS: usize = 10;
const REPRESENTATIVES: usize = 4;
/// 一段脚本最多 1000 条语句。
const API_BATCH: usize = 1000;
/// MCP 默认的 40 KiB 输出上限大约放得下这么多个节点。
const MCP_BATCH: usize = 25;
const MODEL_TOKENS_PER_SECOND: f64 = 10_000.0;
/// 文本大约几个字节一个 token。只用来估算量级。
const BYTES_PER_TOKEN: f64 = 3.0;
const CONTEXT_TOKENS: f64 = 200_000.0;
const DESCRIPTION: &str = "一条用来测试的描述，长度接近真实知识节点里的一段描述文字，包含若干关键事实。这条知识说明了某个决定是什么、为什么这样定，以及以后遇到类似情况应该怎么处理。";
const SOURCE: &str = "用户说：这是一段用来测试读取速度的原文，长度接近真实节点引用的一段用户原话或工具输出。里面有足够的内容，让读取和转义都真实发生，不会被优化掉。";

struct Random(u64);

impl Random {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

fn node_id(index: usize) -> String {
    format!("knowledge-node-{index:08}")
}

fn knowledge(count: usize, source: &str) -> ParsedKnowledge {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let nodes = (0..count)
        .map(|index| {
            let mut related = vec![node_id((index + 1) % count)];
            related.extend((1..LINKS).map(|link| {
                let target = if link % 2 == 0 {
                    random.next() % count
                } else {
                    (index + count + random.next() % (2 * CLUSTER_RADIUS) - CLUSTER_RADIUS) % count
                };
                node_id(target)
            }));
            related.sort();
            related.dedup();
            Node {
                id: node_id(index),
                title: format!("标题 {index}"),
                description: DESCRIPTION.to_owned(),
                evidence: vec![Evidence::Source {
                    source: source.to_owned(),
                    locator: Locator::TextRange {
                        start: 0,
                        end: SOURCE.len(),
                    },
                }],
                related,
            }
        })
        .collect();
    let sections = (0..SECTIONS)
        .map(|section| Section {
            name: format!("板块 {section}"),
            description: "这个领域包含什么，模型靠它判断查询是否属于这个领域。".to_owned(),
            nodes: (0..REPRESENTATIVES)
                .map(|offset| {
                    let index = (section * REPRESENTATIVES + offset) * (count / 41).max(1) % count;
                    CatalogEntry {
                        id: node_id(index),
                        title: format!("标题 {index}"),
                    }
                })
                .collect(),
        })
        .collect();
    ParsedKnowledge {
        catalog: Catalog {
            sections,
            goals: Vec::new(),
        },
        nodes,
    }
}

/// 不经过语言，直接按地址走：这是图本身的速度。
fn raw_walk(memory: &Memory) -> (usize, usize, Duration) {
    let start = Instant::now();
    let mut seen: HashSet<NodeRef<'_>> = HashSet::with_capacity(memory.node_count());
    let mut layer: Vec<NodeRef<'_>> = Vec::new();
    for section in &memory.catalog().sections {
        for entry in &section.nodes {
            if let Some(node) = memory.node(&entry.id)
                && seen.insert(node)
            {
                layer.push(node);
            }
        }
    }
    let mut layers = 0;
    let mut total = 0;
    while !layer.is_empty() {
        layers += 1;
        let mut next = Vec::new();
        for node in layer {
            total += node.description().len();
            for related in node.related() {
                if seen.insert(related) {
                    next.push(related);
                }
            }
        }
        layer = next;
    }
    black_box(total);
    (seen.len(), layers, start.elapsed())
}

/// 走一遍的结果。
struct Walk {
    nodes: usize,
    layers: usize,
    calls: usize,
    /// `Session::run` 花的时间。
    system: Duration,
    /// 加上从文本里解析下一层的时间，也就是调用方要等的全部时间。
    total: Duration,
    /// 模型要写的脚本。
    script_bytes: usize,
    /// 模型要读的输出。
    output_bytes: usize,
}

impl Walk {
    fn print(&self, name: &str) {
        let script_tokens = self.script_bytes as f64 / BYTES_PER_TOKEN;
        let output_tokens = self.output_bytes as f64 / BYTES_PER_TOKEN;
        println!(
            "  {name:<18} {:>7} 节点 {:>3} 层 {:>6} 次调用 | 系统 {:>10.2} ms ({:>6.2} µs/节点) | 含解析 {:>10.2} ms",
            self.nodes,
            self.layers,
            self.calls,
            self.system.as_secs_f64() * 1e3,
            self.system.as_secs_f64() * 1e6 / self.nodes as f64,
            self.total.as_secs_f64() * 1e3,
        );
        println!(
            "  {:<18} 模型写脚本约 {:.0} token，按 10000 token/s 要 {:.3} s | 模型要读 {:.1} MiB，约 {:.0} token，是 20 万上下文的 {:.1} 倍",
            "",
            script_tokens,
            script_tokens / MODEL_TOKENS_PER_SECOND,
            self.output_bytes as f64 / (1024.0 * 1024.0),
            output_tokens,
            output_tokens / CONTEXT_TOKENS,
        );
    }
}

/// 从 `catalog show` 的输出里取代表节点。内置说明的板块不算，它不是知识。
fn entries(catalog: &str, seen: &mut HashSet<String>) -> Vec<String> {
    let mut layer = Vec::new();
    // 总表前面可能有工作板和最近行为，它们的行也是缩进的。
    let mut inside_section = false;
    for line in catalog.lines() {
        if let Some(rest) = line.strip_prefix("section ") {
            inside_section = !rest.contains(memweave::manual::SECTION);
        } else if inside_section
            && let Some(rest) = line.strip_prefix("  ")
            && !rest.starts_with("desc:")
            && !rest.starts_with("没有代表节点")
            && let Some((id, _)) = rest.split_once(" \"")
            && seen.insert(id.to_owned())
        {
            layer.push(id.to_owned());
        }
    }
    layer
}

/// 和模型一样走：总表、每层 `show`、解析 `related`。`depth` 限制走几层，`keep` 限制每层只读几个，
/// 模拟模型判断之后只往有用的方向走。
fn language_walk(
    memory: &mut Memory,
    sources: &dyn SourceStore,
    manual: &Manual,
    batch: usize,
    depth: usize,
    keep: usize,
) -> Walk {
    let mut walk = Walk {
        nodes: 0,
        layers: 0,
        calls: 1,
        system: Duration::ZERO,
        total: Duration::ZERO,
        script_bytes: "revision catalog show".len(),
        output_bytes: 0,
    };
    let everything = Instant::now();
    let mut seen = HashSet::new();

    let start = Instant::now();
    let output = Session::new(memory, sources, Mode::ReadOnly)
        .with_manual(manual)
        .with_time(None)
        .with_output_limit(usize::MAX / 2)
        .run("revision catalog show");
    walk.system += start.elapsed();
    assert!(output.ok, "{}", output.text);
    walk.output_bytes += output.text.len();
    let mut layer = entries(&output.text, &mut seen);
    let mut script = String::new();

    while !layer.is_empty() && walk.layers < depth {
        walk.layers += 1;
        layer.truncate(keep);
        let mut next = Vec::new();
        for chunk in layer.chunks(batch) {
            script.clear();
            script.push_str("revision show ");
            script.push_str(&chunk.join(", "));
            let start = Instant::now();
            let output = Session::new(memory, sources, Mode::ReadOnly)
                .with_manual(manual)
                .with_time(None)
                .with_output_limit(usize::MAX / 2)
                .run(&script);
            walk.system += start.elapsed();
            assert!(output.ok, "{}", output.text);
            walk.calls += 1;
            walk.nodes += chunk.len();
            walk.script_bytes += script.len();
            walk.output_bytes += output.text.len();
            for line in output.text.lines() {
                if let Some(related) = line.strip_prefix("  related: ") {
                    for id in related.split(", ") {
                        if id != "无" && seen.insert(id.to_owned()) {
                            next.push(id.to_owned());
                        }
                    }
                }
            }
        }
        layer = next;
    }
    walk.total = everything.elapsed();
    walk
}

fn bench(name: &str, memory: &mut Memory, sources: &dyn SourceStore, manual: &Manual) {
    println!("\n===== {name} =====");
    let (nodes, layers, elapsed) = raw_walk(memory);
    println!(
        "  裸图（按地址）      {nodes:>7} 节点 {layers:>3} 层 | {:>10.2} ms ({:.0} ns/节点)",
        elapsed.as_secs_f64() * 1e3,
        elapsed.as_nanos() as f64 / nodes as f64
    );
    language_walk(memory, sources, manual, API_BATCH, usize::MAX, usize::MAX)
        .print("直接调用，走完全图");
    language_walk(memory, sources, manual, MCP_BATCH, usize::MAX, usize::MAX)
        .print("分成 MCP 的大小");
    // 走完全图后要释放大量内存，紧接着的那次会被分配器整理拖慢。先走一遍，再走一遍才是它自己的时间。
    let first = language_walk(memory, sources, manual, API_BATCH, 3, 40);
    println!(
        "  （走完全图后第一次查询：系统 {:.2} ms，下面是第二次）",
        first.system.as_secs_f64() * 1e3
    );
    language_walk(memory, sources, manual, API_BATCH, 3, 40).print("只走 3 层每层 40 个");
    language_walk(memory, sources, manual, API_BATCH, 6, 10).print("只走 6 层每层 10 个");
}

fn main() {
    let manual = Manual::new().unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, kb] = args.as_slice()
        && flag == "--kb"
    {
        let path = Path::new(kb);
        let sources = FileSources::new(path);
        let start = Instant::now();
        let mut memory = load_current(&JsonParser, &sources, path).unwrap();
        println!(
            "加载 {} 个节点：{:.2} ms",
            memory.node_count(),
            start.elapsed().as_secs_f64() * 1e3
        );
        bench(kb, &mut memory, &sources, &manual);
        return;
    }
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("sources")).unwrap();
    let sources = FileSources::new(dir.path());
    let source = sources.put(SOURCE.as_bytes()).unwrap();
    for count in COUNTS {
        let parsed = knowledge(count, &source);
        let mut memory = Memory::from_parsed(parsed, &sources).unwrap();
        bench(&format!("{count} 个节点"), &mut memory, &sources, &manual);
    }
}
