//! 测量进程内直接调用记忆系统的速度，不经过 MCP 和管道：
//!
//!     cargo run --release --example bench_api
//!     cargo run --release --example bench_api -- 1000 10000
//!
//! 系统是为约 10000 token/s 的模型设计的，这时一个 token 只有 100 µs。所以每项都换算成“相当于
//! 模型生成几个 token 的时间”：明显小于 1，模型就不会等记忆系统。
//!
//! 分四层测：
//! - 裸图：按地址走图，这是硬件能做到的。
//! - 语言：模型真正调用的入口 `Session::run`，并拆开看一次 `show` 的时间花在哪。
//! - 原文：内存里和磁盘上各读一遍。
//! - 写入：不落盘和落盘分开测，落盘的时间主要由磁盘决定。

use std::fs;
use std::hint::black_box;
use std::time::{Duration, Instant};

use memweave::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use memweave::lang::journal::Journal;
use memweave::lang::{Mode, Session, check, exec, lexer, parser};
use memweave::manual::Manual;
use memweave::memory::Memory;
use memweave::parse::ParsedKnowledge;
use memweave::source::SourceStore;
use memweave::source::file::FileSources;
use memweave::source::in_memory::InMemorySources;
use tempfile::TempDir;

const DEFAULT_COUNTS: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
const LINKS: usize = 4;
const CLUSTER_RADIUS: usize = 32;
/// 按地址走图的跳数。
const HOPS: usize = 5_000_000;
/// 每项读操作重复的次数。
const READS: usize = 20_000;
/// 模型生成一个 token 的时间：10000 token/s。
const TOKEN_MICROS: f64 = 100.0;
/// 你记得的那个数字：连续做这么多次要多久。
const BATCH: usize = 18_000;
const SECTIONS: usize = 10;
const REPRESENTATIVES: usize = 4;
const DESCRIPTION: &str = "一条用来测试的描述，长度接近真实知识节点里的一段描述文字，包含若干关键事实。这条知识说明了某个决定是什么、为什么这样定，以及以后遇到类似情况应该怎么处理。";
const SOURCE: &str = "用户说：这是一段用来测试读取速度的原文，长度接近真实节点引用的一段用户原话或工具输出。里面有足够的内容，让读取和转义都真实发生，不会被优化掉。";

/// 确定性的伪随机数，保证每次运行的图相同。
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

/// 第一条关联连向下一个节点，保证都能到达。其余一半连向邻近节点，一半随机，接近真实的知识图。
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
                    let index = (section * REPRESENTATIVES + offset) % count;
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

/// 一组耗时的统计。
struct Stats {
    samples: Vec<Duration>,
}

impl Stats {
    fn new(mut samples: Vec<Duration>) -> Self {
        samples.sort();
        Self { samples }
    }

    fn at(&self, fraction: f64) -> f64 {
        let index = ((self.samples.len() - 1) as f64 * fraction) as usize;
        self.samples[index].as_secs_f64() * 1e6
    }

    fn print(&self, name: &str) {
        let p50 = self.at(0.5);
        println!(
            "  {name:<30} {p50:>10.2} µs  p99 {:>10.2} µs  相当 {:>8.3} token  每秒 {:>10.0} 次",
            self.at(0.99),
            p50 / TOKEN_MICROS,
            1e6 / p50
        );
    }
}

fn measure(times: usize, mut work: impl FnMut(usize)) -> Stats {
    let mut samples = Vec::with_capacity(times);
    for index in 0..times {
        let start = Instant::now();
        work(index);
        samples.push(start.elapsed());
    }
    Stats::new(samples)
}

fn resident_mib() -> f64 {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0.0, |kib| kib / 1024.0)
}

fn raw_graph(memory: &Memory, count: usize) {
    let mut random = Random(0x1234_5678_9abc_def1);
    let ids: Vec<String> = (0..4096).map(|_| node_id(random.next() % count)).collect();
    let start = Instant::now();
    let mut found = 0usize;
    for index in 0..HOPS {
        found += usize::from(memory.node(&ids[index & 4095]).is_some());
    }
    let lookup = start.elapsed().as_nanos() as f64 / HOPS as f64;
    black_box(found);

    let mut current = memory.node(&node_id(0)).unwrap();
    let mut total = 0usize;
    let start = Instant::now();
    for _ in 0..HOPS {
        let related = current.related();
        let links = related.len();
        current = related.clone().nth(random.next() % links).unwrap();
        total += current.description().len();
    }
    let hop = start.elapsed().as_nanos() as f64 / HOPS as f64;
    black_box(total);

    // 写错地址时给提示要读一遍所有地址，这是它的下限。
    let start = Instant::now();
    let total: usize = memory
        .nodes()
        .map(|node| node.id().bytes().map(usize::from).sum::<usize>())
        .sum();
    let scan = start.elapsed().as_nanos() as f64 / count as f64;
    black_box(total);

    println!("  裸图");
    println!("    按文本地址找节点 {lookup:>7.1} ns");
    println!(
        "    按内存地址走一跳 {hop:>7.1} ns    连续 {BATCH} 跳共 {:.3} ms",
        hop * BATCH as f64 / 1e6
    );
    println!(
        "    读一遍所有地址   {scan:>7.1} ns/个  共 {:.2} ms",
        scan * count as f64 / 1e6
    );
}

fn show_breakdown(memory: &mut Memory, sources: &InMemorySources, count: usize) {
    let mut random = Random(0x0bad_cafe_1234_5678);
    let scripts: Vec<String> = (0..READS)
        .map(|_| format!("show {}", node_id(random.next() % count)))
        .collect();
    let (mut lexing, mut parsing, mut checking, mut executing) = (
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    );
    for script in &scripts {
        let start = Instant::now();
        let (tokens, _) = lexer::tokenize(script);
        lexing += start.elapsed();
        let start = Instant::now();
        let (statements, _) = parser::parse(tokens);
        parsing += start.elapsed();
        let start = Instant::now();
        black_box(check::check(
            &statements,
            memory,
            sources,
            Mode::ReadOnly,
            None,
        ));
        checking += start.elapsed();
        let start = Instant::now();
        let report = exec::execute(&statements, memory, sources, None, None);
        black_box(report.map(|report| report.text.len()).ok());
        executing += start.elapsed();
    }
    let per = |total: Duration| total.as_secs_f64() * 1e6 / READS as f64;
    println!(
        "  一次 show 拆开：词法 {:.2} µs  语法 {:.2} µs  检查 {:.2} µs  执行和渲染 {:.2} µs",
        per(lexing),
        per(parsing),
        per(checking),
        per(executing)
    );
}

fn language(memory: &mut Memory, sources: &InMemorySources, manual: &Manual, count: usize) {
    let mut random = Random(0x0dd0_face_8765_4321);
    let mut pick = move || node_id(random.next() % count);
    let run = |memory: &mut Memory, script: &str| {
        let output = Session::new(memory, sources, Mode::ReadOnly)
            .with_manual(manual)
            .with_time(None)
            .run(script);
        black_box(output.text.len());
        output.ok
    };

    println!("  语言（Session::run，原文在内存里）");
    let scripts: Vec<String> = (0..READS).map(|_| format!("show {}", pick())).collect();
    measure(READS, |i| {
        assert!(run(memory, &scripts[i]));
    })
    .print("show 一个节点");

    let start = Instant::now();
    for script in scripts.iter().cycle().take(BATCH) {
        run(memory, script);
    }
    println!(
        "  {:<30} {:>10.2} ms",
        format!("连续 show {BATCH} 次共"),
        start.elapsed().as_secs_f64() * 1e3
    );

    let batches: Vec<String> = (0..READS / 4)
        .map(|_| {
            let ids: Vec<String> = (0..10).map(|_| pick()).collect();
            format!("show {}", ids.join(", "))
        })
        .collect();
    measure(batches.len(), |i| {
        assert!(run(memory, &batches[i]));
    })
    .print("show 一层 10 个节点");

    measure(READS, |_| {
        assert!(run(memory, "catalog show"));
    })
    .print("catalog show");

    let links: Vec<String> = (0..READS / 4)
        .map(|_| format!("links {} depth 3 limit 50", pick()))
        .collect();
    measure(links.len(), |i| {
        assert!(run(memory, &links[i]));
    })
    .print("links 走 3 层");

    let evidence: Vec<String> = (0..READS).map(|_| format!("evidence {}", pick())).collect();
    measure(READS, |i| {
        assert!(run(memory, &evidence[i]));
    })
    .print("evidence");

    measure(READS, |_| {
        assert!(run(memory, "show manual-start"));
    })
    .print("show manual-start");

    // 写错地址要逐个比较才能给出提示，这是唯一随节点数变慢的读路径。
    let wrong: Vec<String> = (0..50).map(|_| format!("show {}x", pick())).collect();
    measure(wrong.len(), |i| {
        assert!(!run(memory, &wrong[i]));
    })
    .print("写错一个字符（带提示）");

    // 和所有节点都不像，又和它们一样长，是最慢的情况。
    measure(50, |_| {
        assert!(!run(memory, "show zzzzzzzzzzzzzzzzzzzzzzzz"));
    })
    .print("写错地址（完全不像）");

    let times = if count > 100_000 { 5 } else { 50 };
    measure(times, |_| {
        assert!(run(memory, "check"));
    })
    .print("check 遍历全图");
}

fn on_disk(count: usize) {
    let dir = TempDir::new().unwrap();
    let sources = FileSources::new(dir.path());
    let source = sources.put(SOURCE.as_bytes()).unwrap();
    let parsed = knowledge(count, &source);
    let mut memory = Memory::from_parsed(parsed, &sources).unwrap();
    let manual = Manual::new().unwrap();
    let journal = Journal::new(dir.path());
    let mut random = Random(0xfeed_beef_0000_0001);
    let mut pick = move || node_id(random.next() % count);

    println!("  磁盘（原文在文件里，写入要落盘）");
    let scripts: Vec<String> = (0..READS).map(|_| format!("show {}", pick())).collect();
    measure(READS, |i| {
        let output = Session::new(&mut memory, &sources, Mode::ReadOnly)
            .with_manual(&manual)
            .run(&scripts[i]);
        assert!(output.ok);
        black_box(output.text.len());
    })
    .print("show（读原文文件）");

    const WRITES: usize = 200;
    let edits: Vec<String> = (0..WRITES)
        .map(|index| format!("set {} desc \"\"\"新的描述 {index}\"\"\"", pick()))
        .collect();
    measure(WRITES, |i| {
        let output = Session::new(&mut memory, &sources, Mode::ReadWrite)
            .with_journal(&journal)
            .with_manual(&manual)
            .run(&edits[i]);
        assert!(output.ok, "{}", output.text);
    })
    .print("set（落盘）");

    let mut anchor = pick();
    let additions: Vec<String> = (0..WRITES)
        .map(|index| {
            let script = format!(
                "source 新节点-{index}\n  title \"新节点\"\n  desc \"\"\"新的知识。\"\"\"\n  text \"\"\"用户说：第 {index} 条新的知识。\"\"\"\n  related_by {anchor}\nend"
            );
            anchor = pick();
            script
        })
        .collect();
    measure(WRITES, |i| {
        let output = Session::new(&mut memory, &sources, Mode::ReadWrite)
            .with_journal(&journal)
            .with_manual(&manual)
            .run(&additions[i]);
        assert!(output.ok, "{}", output.text);
    })
    .print("source 新节点（原文和日志落盘）");
}

fn writes_in_memory(memory: &mut Memory, sources: &InMemorySources, manual: &Manual, count: usize) {
    let mut random = Random(0xabad_1dea_0000_0002);
    let mut pick = move || node_id(random.next() % count);
    println!("  写入（只改内存，不落盘）");
    const WRITES: usize = 5_000;
    let edits: Vec<String> = (0..WRITES)
        .map(|index| format!("set {} desc \"\"\"新的描述 {index}\"\"\"", pick()))
        .collect();
    measure(WRITES, |i| {
        let output = Session::new(memory, sources, Mode::ReadWrite)
            .with_manual(manual)
            .with_time(None)
            .run(&edits[i]);
        assert!(output.ok, "{}", output.text);
    })
    .print("set");
}

fn main() {
    let counts: Vec<usize> = {
        let given: Vec<usize> = std::env::args()
            .skip(1)
            .filter_map(|arg| arg.parse().ok())
            .collect();
        if given.is_empty() {
            DEFAULT_COUNTS.to_vec()
        } else {
            given
        }
    };
    let timer = measure(1_000_000, |_| {});
    println!(
        "计时本身约 {:.0} ns。一个 token 按 {TOKEN_MICROS} µs 算（10000 token/s）。",
        timer.at(0.5) * 1000.0
    );
    let manual = Manual::new().unwrap();

    for count in counts {
        println!("\n===== {count} 个节点 =====");
        let mut sources = InMemorySources::new();
        let source = sources.insert(SOURCE.as_bytes().to_vec());
        let parsed = knowledge(count, &source);
        let before = resident_mib();
        let start = Instant::now();
        let mut memory = Memory::from_parsed(parsed, &sources).unwrap();
        println!(
            "  建图 {:.1} ms，常驻内存约 {:.0} MiB（增加 {:.0} MiB，含解析结果）",
            start.elapsed().as_secs_f64() * 1e3,
            resident_mib(),
            resident_mib() - before
        );

        raw_graph(&memory, count);
        show_breakdown(&mut memory, &sources, count);
        language(&mut memory, &sources, &manual, count);
        writes_in_memory(&mut memory, &sources, &manual, count);
        drop(memory);
        on_disk(count);
    }
}
