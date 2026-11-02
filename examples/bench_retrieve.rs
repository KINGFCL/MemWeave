//! 测量一次 `retrieve()` 的框架开销，不含模型：
//!
//!     cargo run --release --example bench_retrieve
//!
//! 用立即回答的假模型，假模型自己花的时间已经减掉。框架开销包括拼提示词、解析模型的回答、
//! 走图和生成知识树。

use std::cell::Cell;
use std::hint::black_box;
use std::time::{Duration, Instant};

use memweave::error::Error;
use memweave::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use memweave::memory::Memory;
use memweave::model::{Model, Prompt};
use memweave::parse::ParsedKnowledge;
use memweave::retrieve::retrieve;
use memweave::source::in_memory::InMemorySources;
use serde_json::Value;

const COUNTS: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
const LINKS: usize = 4;
const CLUSTER_RADIUS: usize = 32;
const SECTIONS: usize = 10;
const REPRESENTATIVES: usize = 4;
const REPEATS: usize = 20;
const DESCRIPTION: &str = "一条用来测试的描述，长度接近真实知识节点里的一段描述文字，包含若干关键事实。这条知识说明了某个决定是什么、为什么这样定，以及以后遇到类似情况应该怎么处理。";
/// 文本大约几个字节一个 token。只用来估算量级。
const BYTES_PER_TOKEN: usize = 3;

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
                    locator: Locator::TextRange { start: 0, end: 4 },
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

/// 立即回答的假模型：选前几个入口，每层保留前几个，走到指定层数后什么都不保留。
struct InstantModel {
    entries: usize,
    keep: usize,
    depth: usize,
    calls: Cell<usize>,
    /// 假模型自己花的时间，不算系统的。
    own_time: Cell<Duration>,
    prompt_bytes: Cell<usize>,
}

impl InstantModel {
    fn new(entries: usize, keep: usize, depth: usize) -> Self {
        Self {
            entries,
            keep,
            depth,
            calls: Cell::new(0),
            own_time: Cell::new(Duration::ZERO),
            prompt_bytes: Cell::new(0),
        }
    }
}

impl Model for InstantModel {
    fn context_limit(&self) -> usize {
        usize::MAX
    }

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error> {
        Ok(prompt.user.len() / BYTES_PER_TOKEN)
    }

    fn generate(&self, prompt: &Prompt, _schema: &Value) -> Result<String, Error> {
        let start = Instant::now();
        let call = self.calls.get();
        self.calls.set(call + 1);
        self.prompt_bytes
            .set(self.prompt_bytes.get() + prompt.user.len());
        let (wanted, text) = if call == 0 {
            (self.entries, prompt.user.as_str())
        } else if call > self.depth {
            (0, "")
        } else {
            // JSON 的键按字典序排列，已保留的节点在候选后面。
            let end = prompt.user.find("\"kept\":").unwrap_or(prompt.user.len());
            (self.keep, &prompt.user[..end])
        };
        let mut output = String::from("{\"ids\":[");
        for (index, piece) in text.split("\"id\":\"").skip(1).take(wanted).enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push('"');
            output.push_str(&piece[..piece.find('"').unwrap_or(0)]);
            output.push('"');
        }
        output.push_str("]}");
        self.own_time.set(self.own_time.get() + start.elapsed());
        Ok(output)
    }
}

fn simulated(
    memory: &Memory,
    name: &str,
    entries: usize,
    keep: usize,
    depth: usize,
    repeats: usize,
) {
    let mut samples = Vec::with_capacity(repeats);
    let mut calls = 0;
    let mut bytes = 0;
    let mut kept = 0;
    for _ in 0..repeats {
        let model = InstantModel::new(entries, keep, depth);
        let start = Instant::now();
        let tree = retrieve(memory, &model, "局部变量按什么顺序释放？").unwrap();
        let elapsed = start.elapsed().saturating_sub(model.own_time.get());
        samples.push(elapsed);
        calls = model.calls.get();
        bytes = model.prompt_bytes.get();
        kept = count_tree(&tree.roots);
        black_box(tree);
    }
    samples.sort();
    let middle = samples[samples.len() / 2];
    println!(
        "  {name:<22} 保留 {kept:>7} 节点 {calls:>3} 次模型调用 | 系统 {:>11.3} ms | 发给模型 {:>9.2} MiB，约 {:>10} token",
        middle.as_secs_f64() * 1e3,
        bytes as f64 / (1024.0 * 1024.0),
        bytes / BYTES_PER_TOKEN
    );
}

fn count_tree(nodes: &[memweave::knowledge::TreeNode]) -> usize {
    nodes.len() + nodes.iter().map(|n| count_tree(&n.children)).sum::<usize>()
}

fn main() {
    let mut sources = InMemorySources::new();
    let source = sources.insert(b"text".to_vec());
    for count in COUNTS {
        println!("\n===== {count} 个节点 =====");
        let memory = Memory::from_parsed(knowledge(count, &source), &sources).unwrap();
        simulated(&memory, "总表选 3 个，每层 5 个，3 层", 3, 5, 3, REPEATS);
        simulated(&memory, "总表选 5 个，每层 10 个，6 层", 5, 10, 6, REPEATS);
        simulated(
            &memory,
            "全部保留，走完全图",
            usize::MAX,
            usize::MAX,
            usize::MAX,
            1,
        );
    }
}
