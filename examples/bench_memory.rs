//! 对比按地址遍历和按文本地址查哈希表的速度：
//!
//!     cargo run --release --example bench_memory
//!
//! 节点随机相连，每一跳都可能缓存未命中，这是图遍历最坏的情况。

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use memweave::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use memweave::memory::Memory;
use memweave::parse::ParsedKnowledge;
use memweave::pool::memory_pool;
use memweave::source::in_memory::InMemorySources;

const NODE_COUNTS: [usize; 3] = [10_000, 100_000, 1_000_000];
const RELATED: usize = 8;
const HOPS: usize = 5_000_000;
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

fn build_nodes(count: usize, source: &str) -> Vec<Node> {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    (0..count)
        .map(|index| {
            // 第一条关联连向下一个节点，保证所有节点都能从总表到达。
            let mut related = vec![node_id((index + 1) % count)];
            related.extend((1..RELATED).map(|_| node_id(random.next() % count)));
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
        .collect()
}

fn nanos_per_hop(start: Instant) -> f64 {
    start.elapsed().as_nanos() as f64 / HOPS as f64
}

fn bench_graph(count: usize) {
    let mut sources = InMemorySources::new();
    let source = sources.insert(b"text".to_vec());
    let nodes = build_nodes(count, &source);
    let by_id: HashMap<String, Node> = nodes.iter().map(|n| (n.id.clone(), n.clone())).collect();
    let catalog = Catalog {
        sections: vec![Section {
            name: "板块".into(),
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
        let pick = random.next() % RELATED;
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
        let pick = random.next() % RELATED;
        current = &by_id[&current.related[pick]];
        total += current.description.len();
    }
    let by_hash = nanos_per_hop(start);
    black_box(total);

    println!(
        "{count:>9} 节点 | 建图 {:>7.1} ms | 按地址 {by_address:>6.1} ns/跳 | 哈希表 {by_hash:>6.1} ns/跳 | 快 {:.1} 倍",
        build.as_secs_f64() * 1000.0,
        by_hash / by_address
    );
}

fn bench_pool() {
    const BLOCKS: usize = 5_000_000;
    const SIZE: usize = 128;

    let (mut allocator, deallocator) = memory_pool().unwrap();
    let start = Instant::now();
    for _ in 0..BLOCKS {
        let block = allocator.allocate(SIZE).unwrap();
        // 释放后立刻重新分配，只测无锁栈本身。
        unsafe { deallocator.deallocate(block, SIZE) };
    }
    let pool_nanos = start.elapsed().as_nanos() as f64 / BLOCKS as f64;

    let start = Instant::now();
    for _ in 0..BLOCKS {
        black_box(Box::new([0u8; SIZE]));
    }
    let system_nanos = start.elapsed().as_nanos() as f64 / BLOCKS as f64;

    println!(
        "分配并释放 {SIZE} 字节：内存池 {pool_nanos:.1} ns，系统分配器 {system_nanos:.1} ns（含清零）"
    );
}

fn main() {
    for count in NODE_COUNTS {
        bench_graph(count);
    }
    bench_pool();
}
