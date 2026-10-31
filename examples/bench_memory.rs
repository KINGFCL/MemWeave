//! 测量记忆图的遍历速度，并和两个基准对比：
//!
//!     cargo run --release --example bench_memory
//!
//! - 硬件极限：同样大小的内存上，随机访问一次要多久。走地址不可能比它快太多。
//! - 哈希表：每一跳都用文本地址查一次标准库的 HashMap。
//!
//! 随机图的每一跳都可能缓存未命中，是最坏的情况。聚集图的关联集中在邻近节点上，遍历时在一片
//! 区域内来回走，节点大多已经在缓存里，是最好的情况。

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use memweave::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use memweave::memory::Memory;
use memweave::parse::ParsedKnowledge;
use memweave::source::in_memory::InMemorySources;

const NODE_COUNTS: [usize; 4] = [1_000, 10_000, 100_000, 1_000_000];
const LINK_COUNTS: [usize; 3] = [4, 7, 8];
const HOPS: usize = 5_000_000;
const CLUSTER_RADIUS: usize = 32;
const DESCRIPTION: &str =
    "一条用来测试的描述，长度接近真实知识节点里的一段描述文字，包含若干关键事实。";

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

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Random,
    Clustered,
}

fn build_nodes(count: usize, links: usize, shape: Shape, source: &str) -> Vec<Node> {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut nodes: Vec<Node> = (0..count)
        .map(|index| {
            // 第一条关联连向下一个节点，保证所有节点都能从总表到达。
            let mut related = vec![node_id((index + 1) % count)];
            related.extend((1..links).map(|_| {
                let target = match shape {
                    Shape::Random => random.next() % count,
                    // 前后都有关联，遍历时会在一片区域内来回走，和真实的知识图一样。
                    Shape::Clustered => {
                        (index + count + random.next() % (2 * CLUSTER_RADIUS) - CLUSTER_RADIUS)
                            % count
                    }
                };
                node_id(target)
            }));
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
    // 打乱输入顺序：解析结果按文件名排序，和图的结构无关。
    for index in (1..count).rev() {
        nodes.swap(index, random.next() % (index + 1));
    }
    nodes
}

fn nanos_per_hop(start: Instant) -> f64 {
    start.elapsed().as_nanos() as f64 / HOPS as f64
}

/// 在 `bytes` 大小的内存上，沿随机排列逐条缓存行访问，得到随机访问一次的延迟。
fn memory_latency(bytes: usize) -> f64 {
    #[repr(align(64))]
    #[derive(Clone, Copy)]
    struct Line(usize);

    let count = (bytes / 64).max(2);
    let mut lines = vec![Line(0); count];
    let mut order: Vec<usize> = (0..count).collect();
    let mut random = Random(0xdead_beef_cafe_f00d);
    for index in (1..count).rev() {
        order.swap(index, random.next() % index);
    }
    for pair in order.windows(2) {
        lines[pair[0]].0 = pair[1];
    }
    lines[order[count - 1]].0 = order[0];

    let mut current = 0;
    let start = Instant::now();
    for _ in 0..HOPS {
        current = lines[current].0;
    }
    black_box(current);
    nanos_per_hop(start)
}

fn bench_graph(count: usize, links: usize, shape: Shape) {
    let mut sources = InMemorySources::new();
    let source = sources.insert(b"text".to_vec());
    let nodes = build_nodes(count, links, shape, &source);
    let by_id: HashMap<String, Node> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
    let catalog = Catalog {
        sections: vec![Section {
            name: "板块".into(),
            description: String::new(),
            nodes: vec![CatalogEntry {
                id: node_id(0),
                title: "入口".into(),
            }],
        }],
    };

    let start = Instant::now();
    let memory = Memory::from_parsed(ParsedKnowledge { catalog, nodes }, &sources).unwrap();
    let build = start.elapsed();

    // 按地址：直接解引用节点和关联。
    let mut random = Random(0x1234_5678_9abc_def1);
    let mut current = memory.node(&node_id(0)).unwrap();
    let mut total = 0usize;
    let start = Instant::now();
    for _ in 0..HOPS {
        let pick = random.next() % links;
        current = current.related().nth(pick).unwrap();
        total += current.description().len();
    }
    let by_address = nanos_per_hop(start);
    black_box(total);

    // 按文本地址：每一跳都查一次哈希表。
    let mut random = Random(0x1234_5678_9abc_def1);
    let mut current = by_id.get(&node_id(0)).unwrap();
    let mut total = 0usize;
    let start = Instant::now();
    for _ in 0..HOPS {
        let pick = random.next() % links;
        current = &by_id[&current.related[pick]];
        total += current.description.len();
    }
    let by_hash = nanos_per_hop(start);
    black_box(total);

    let shape = if shape == Shape::Random {
        "随机"
    } else {
        "聚集"
    };
    println!(
        "{shape} {count:>9} 节点 {links} 关联 | 建图 {:>8.1} ms | 按地址 {by_address:>6.1} ns/跳 | 哈希表 {by_hash:>6.1} ns/跳 | 快 {:>4.1} 倍",
        build.as_secs_f64() * 1000.0,
        by_hash / by_address
    );
}

fn main() {
    println!("硬件极限（一次随机内存访问）：");
    for (name, bytes) in [
        ("128 KiB", 128usize << 10),
        ("2 MiB", 2 << 20),
        ("32 MiB", 32 << 20),
        ("256 MiB", 256 << 20),
        ("1 GiB", 1 << 30),
    ] {
        println!("  {name:>8}: {:>6.1} ns", memory_latency(bytes));
    }
    println!();
    for count in NODE_COUNTS {
        for links in LINK_COUNTS {
            bench_graph(count, links, Shape::Random);
        }
    }
    println!();
    for count in NODE_COUNTS {
        bench_graph(count, 4, Shape::Clustered);
    }
}
