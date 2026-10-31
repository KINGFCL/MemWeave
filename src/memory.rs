use std::collections::{HashMap, HashSet};
use std::path::Path;

use thiserror::Error;

use crate::error::Error;
use crate::knowledge::{Catalog, Evidence, Locator, Node, NodeId};
use crate::parse::{ParsedKnowledge, Parser};
use crate::source::{LoadedEvidence, SourceStore, is_valid_source_id};
use crate::store::Store;

/// 节点地址的长度上限。地址可能被用作文件名，大多数文件系统限制为 255 字节。
const MAX_ID_BYTES: usize = 200;

/// 内存中的知识图。只能通过校验后的解析结果创建，所以关联和总表都是一致的。
#[derive(Debug)]
pub struct Memory {
    catalog: Catalog,
    nodes: HashMap<NodeId, Node>,
}

/// 校验时发现的问题。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Problem {
    #[error("节点地址 {id:?} 不合法：只能包含字母、数字、- 和 _，且不超过 {MAX_ID_BYTES} 字节")]
    InvalidId { id: NodeId },

    #[error("节点 {id} 重复")]
    DuplicateNode { id: NodeId },

    #[error("板块“{section}”的代表节点 {id} 不存在")]
    MissingEntry { section: String, id: NodeId },

    #[error("节点 {referrer} 关联的节点 {id} 不存在")]
    BrokenLink { id: NodeId, referrer: NodeId },

    #[error("节点 {id} 无法从总表到达")]
    Unreachable { id: NodeId },

    #[error("节点 {id} 没有任何证据")]
    MissingEvidence { id: NodeId },

    #[error("节点 {referrer} 的证据节点 {id} 不存在")]
    BrokenEvidence { id: NodeId, referrer: NodeId },

    #[error("节点 {} 的证据形成循环，或依赖了这样的循环", .ids.join("、"))]
    EvidenceCycle { ids: Vec<NodeId> },

    #[error("节点 {referrer} 的来源地址 {source_id:?} 不合法")]
    InvalidSource { source_id: String, referrer: NodeId },

    #[error("节点 {referrer} 的来源 {source_id} 不存在")]
    MissingSource { source_id: String, referrer: NodeId },

    #[error("节点 {referrer} 的证据范围 {start}..{end} 无效，来源 {source_id} 共 {size} 字节")]
    InvalidRange {
        source_id: String,
        referrer: NodeId,
        start: usize,
        end: usize,
        size: u64,
    },

    #[error("来源无法检查：{message}")]
    SourceUnreadable { message: String },
}

impl Memory {
    /// 用 `parser` 解析 `source` 处的知识库并校验。证据指向的来源不会被读进内存。
    pub fn load(
        parser: &dyn Parser,
        sources: &dyn SourceStore,
        source: &Path,
    ) -> Result<Self, Error> {
        Self::from_parsed(parser.parse(source)?, sources)
    }

    /// 校验解析结果并建图。有问题时一次报告所有问题。
    pub fn from_parsed(parsed: ParsedKnowledge, sources: &dyn SourceStore) -> Result<Self, Error> {
        let problems = Self::validate(&parsed, sources);
        if !problems.is_empty() {
            return Err(Error::InvalidMemory(problems));
        }
        let ParsedKnowledge { catalog, nodes } = parsed;
        let nodes = nodes
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect();
        Ok(Self { catalog, nodes })
    }

    /// 找出解析结果中的所有问题。
    ///
    /// 来源只检查是否存在和范围是否有效，不读取内容。内容是否被改动在读取证据时校验。
    pub fn validate(parsed: &ParsedKnowledge, sources: &dyn SourceStore) -> Vec<Problem> {
        let mut problems = Vec::new();

        let mut by_id = HashMap::with_capacity(parsed.nodes.len());
        for node in &parsed.nodes {
            if !is_valid_id(&node.id) {
                problems.push(Problem::InvalidId {
                    id: node.id.clone(),
                });
            }
            if by_id.insert(node.id.as_str(), node).is_some() {
                problems.push(Problem::DuplicateNode {
                    id: node.id.clone(),
                });
            }
        }

        for section in &parsed.catalog.sections {
            for entry in &section.nodes {
                if !by_id.contains_key(entry.id.as_str()) {
                    problems.push(Problem::MissingEntry {
                        section: section.name.clone(),
                        id: entry.id.clone(),
                    });
                }
            }
        }

        for node in &parsed.nodes {
            for related in &node.related {
                if !by_id.contains_key(related.as_str()) {
                    problems.push(Problem::BrokenLink {
                        id: related.clone(),
                        referrer: node.id.clone(),
                    });
                }
            }
        }

        let reachable = reachable_from_catalog(&parsed.catalog, &by_id);
        let mut reported = HashSet::new();
        for node in &parsed.nodes {
            if !reachable.contains(node.id.as_str()) && reported.insert(node.id.as_str()) {
                problems.push(Problem::Unreachable {
                    id: node.id.clone(),
                });
            }
        }

        check_evidence(parsed, &by_id, sources, &mut problems);
        problems
    }

    /// 读取节点的证据。来源的内容这时才从固态硬盘加载。
    pub fn evidence(
        &self,
        id: &str,
        sources: &dyn SourceStore,
    ) -> Result<Vec<LoadedEvidence>, Error> {
        let node = self
            .nodes
            .get(id)
            .ok_or_else(|| Error::UnknownNode(id.to_owned()))?;
        node.evidence
            .iter()
            .map(|evidence| match evidence {
                Evidence::Source { source, locator } => Ok(LoadedEvidence::Source {
                    source: source.clone(),
                    locator: locator.clone(),
                    text: sources.read(source, locator)?,
                }),
                Evidence::Node { id } => Ok(LoadedEvidence::Node { id: id.clone() }),
            })
            .collect()
    }

    /// 所有节点，顺序不固定。
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// 从所有代表节点出发，沿关联的方向找出能到达的节点。总表之外的节点只能这样被查到。
fn reachable_from_catalog<'a>(
    catalog: &'a Catalog,
    nodes: &HashMap<&'a str, &'a Node>,
) -> HashSet<&'a str> {
    let mut reachable = HashSet::new();
    let mut pending: Vec<&str> = catalog
        .sections
        .iter()
        .flat_map(|section| section.nodes.iter().map(|entry| entry.id.as_str()))
        .collect();

    while let Some(id) = pending.pop() {
        if let Some(node) = nodes.get(id)
            && reachable.insert(id)
        {
            pending.extend(node.related.iter().map(String::as_str));
        }
    }
    reachable
}

fn check_evidence(
    parsed: &ParsedKnowledge,
    nodes: &HashMap<&str, &Node>,
    sources: &dyn SourceStore,
    problems: &mut Vec<Problem>,
) {
    // 一个来源可能被大量证据引用，每个只查询一次大小。
    let mut sizes: HashMap<&str, Result<Option<u64>, ()>> = HashMap::new();

    for node in &parsed.nodes {
        if node.evidence.is_empty() {
            problems.push(Problem::MissingEvidence {
                id: node.id.clone(),
            });
        }
        for evidence in &node.evidence {
            match evidence {
                Evidence::Node { id } => {
                    if !nodes.contains_key(id.as_str()) {
                        problems.push(Problem::BrokenEvidence {
                            id: id.clone(),
                            referrer: node.id.clone(),
                        });
                    }
                }
                Evidence::Source { source, locator } => {
                    if !is_valid_source_id(source) {
                        problems.push(Problem::InvalidSource {
                            source_id: source.clone(),
                            referrer: node.id.clone(),
                        });
                        continue;
                    }
                    let size = sizes.entry(source).or_insert_with(|| {
                        sources.size(source).map_err(|e| {
                            problems.push(Problem::SourceUnreadable {
                                message: e.to_string(),
                            });
                        })
                    });
                    match size {
                        Ok(Some(size)) => check_locator(node, source, locator, *size, problems),
                        Ok(None) => problems.push(Problem::MissingSource {
                            source_id: source.clone(),
                            referrer: node.id.clone(),
                        }),
                        Err(()) => {}
                    }
                }
            }
        }
    }

    let cyclic = nodes_in_evidence_cycles(parsed, nodes);
    if !cyclic.is_empty() {
        problems.push(Problem::EvidenceCycle { ids: cyclic });
    }
}

fn check_locator(
    node: &Node,
    source: &str,
    locator: &Locator,
    size: u64,
    problems: &mut Vec<Problem>,
) {
    match *locator {
        Locator::TextRange { start, end } => {
            if start >= end || end as u64 > size {
                problems.push(Problem::InvalidRange {
                    source_id: source.to_owned(),
                    referrer: node.id.clone(),
                    start,
                    end,
                    size,
                });
            }
        }
    }
}

/// 找出无法追溯到原始数据的节点：它们的证据形成循环，或者依赖了这样的循环。
///
/// 反复移除所有依据都已确认的节点，剩下的就是这些节点。
fn nodes_in_evidence_cycles(parsed: &ParsedKnowledge, nodes: &HashMap<&str, &Node>) -> Vec<NodeId> {
    let mut waiting_on: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes.values() {
        let dependencies: HashSet<&str> = node
            .evidence
            .iter()
            .filter_map(|evidence| match evidence {
                Evidence::Node { id } if nodes.contains_key(id.as_str()) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        for dependency in &dependencies {
            dependents.entry(dependency).or_default().push(&node.id);
        }
        waiting_on.insert(&node.id, dependencies);
    }

    let mut settled: Vec<&str> = waiting_on
        .iter()
        .filter(|(_, dependencies)| dependencies.is_empty())
        .map(|(id, _)| *id)
        .collect();
    while let Some(id) = settled.pop() {
        waiting_on.remove(id);
        for dependent in dependents.get(id).into_iter().flatten() {
            if let Some(dependencies) = waiting_on.get_mut(dependent) {
                dependencies.remove(id);
                if dependencies.is_empty() {
                    settled.push(dependent);
                }
            }
        }
    }

    // 按输入顺序报告，结果才是确定的。
    let mut reported = HashSet::new();
    parsed
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .filter(|id| waiting_on.contains_key(id) && reported.insert(*id))
        .map(str::to_owned)
        .collect()
}

impl Store for Memory {
    fn catalog(&self) -> Result<Catalog, Error> {
        Ok(self.catalog.clone())
    }

    fn node(&self, id: &str) -> Result<Option<Node>, Error> {
        Ok(self.nodes.get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::knowledge::{CatalogEntry, Section};
    use crate::source::in_memory::InMemorySources;
    use crate::source::source_id;

    const TEXT: &str = "一段原文";

    fn sources() -> InMemorySources {
        let mut sources = InMemorySources::new();
        sources.insert(TEXT.as_bytes().to_vec());
        sources
    }

    fn catalog(entries: &[&str]) -> Catalog {
        let nodes = entries
            .iter()
            .map(|id| CatalogEntry {
                id: id.to_string(),
                title: format!("标题{id}"),
            })
            .collect();
        Catalog {
            sections: vec![Section {
                name: "板块".into(),
                nodes,
            }],
        }
    }

    fn source_evidence(start: usize, end: usize) -> Evidence {
        Evidence::Source {
            source: source_id(TEXT.as_bytes()),
            locator: Locator::TextRange { start, end },
        }
    }

    fn node_with_evidence(id: &str, evidence: Vec<Evidence>) -> Node {
        Node {
            id: id.to_string(),
            title: format!("标题{id}"),
            description: format!("描述{id}"),
            evidence,
            related: Vec::new(),
        }
    }

    fn node(id: &str, related: &[&str]) -> Node {
        Node {
            related: related.iter().map(|r| r.to_string()).collect(),
            ..node_with_evidence(id, vec![source_evidence(0, 3)])
        }
    }

    fn based_on(id: &str, targets: &[&str]) -> Node {
        let evidence = targets
            .iter()
            .map(|target| Evidence::Node {
                id: target.to_string(),
            })
            .collect();
        node_with_evidence(id, evidence)
    }

    fn build(entries: &[&str], nodes: Vec<Node>) -> Result<Memory, Error> {
        Memory::from_parsed(
            ParsedKnowledge {
                catalog: catalog(entries),
                nodes,
            },
            &sources(),
        )
    }

    fn problems(result: Result<Memory, Error>) -> Vec<Problem> {
        match result {
            Err(Error::InvalidMemory(problems)) => problems,
            other => panic!("应为校验错误：{other:?}"),
        }
    }

    #[test]
    fn serves_nodes_outside_the_catalog() {
        let memory = build(
            &["a"],
            vec![node("a", &["b"]), node("b", &["c"]), node("c", &["a"])],
        )
        .unwrap();

        assert_eq!(memory.node_count(), 3);
        assert_eq!(memory.catalog().unwrap(), catalog(&["a"]));
        assert_eq!(memory.node("c").unwrap(), Some(node("c", &["a"])));
        assert_eq!(memory.node("不存在").unwrap(), None);
    }

    #[test]
    fn empty_knowledge_base_is_valid() {
        assert_eq!(build(&[], vec![]).unwrap().node_count(), 0);
    }

    #[test]
    fn reachability_follows_link_direction() {
        // c 指向 a，但没有路径能从 a 走到 c。
        let result = build(&["a"], vec![node("a", &[]), node("c", &["a"])]);

        assert_eq!(problems(result), [Problem::Unreachable { id: "c".into() }]);
    }

    #[test]
    fn isolated_cycle_is_unreachable() {
        let result = build(
            &["a"],
            vec![node("a", &[]), node("x", &["y"]), node("y", &["x"])],
        );

        assert_eq!(
            problems(result),
            [
                Problem::Unreachable { id: "x".into() },
                Problem::Unreachable { id: "y".into() },
            ]
        );
    }

    #[test]
    fn reports_every_problem_at_once() {
        let result = build(
            &["a", "幽灵入口"],
            vec![node("a", &["幽灵关联"]), node("a", &[]), node("孤岛", &[])],
        );

        assert_eq!(
            problems(result),
            [
                Problem::DuplicateNode { id: "a".into() },
                Problem::MissingEntry {
                    section: "板块".into(),
                    id: "幽灵入口".into(),
                },
                Problem::BrokenLink {
                    id: "幽灵关联".into(),
                    referrer: "a".into(),
                },
                Problem::Unreachable {
                    id: "孤岛".into()
                },
            ]
        );
    }

    #[test]
    fn rejects_ids_that_are_unsafe_as_addresses() {
        let long = "长".repeat(100);
        for id in [
            "",
            "../x",
            "a/b",
            "a\\b",
            "a b",
            ".",
            "..",
            "a.json",
            long.as_str(),
        ] {
            let problems = problems(build(&[id], vec![node(id, &[])]));
            assert_eq!(problems, [Problem::InvalidId { id: id.into() }], "{id:?}");
        }
        assert!(build(&["中文-Id_1"], vec![node("中文-Id_1", &[])]).is_ok());
    }

    #[test]
    fn error_message_lists_every_problem() {
        let message = build(&["幽灵"], vec![node("孤岛", &[])])
            .unwrap_err()
            .to_string();

        assert!(message.contains("2 个问题"), "{message}");
        assert!(
            message.contains("幽灵") && message.contains("孤岛"),
            "{message}"
        );
    }

    /// 接入新格式只需要实现 `Parser`，校验和查询都不用改。
    #[test]
    fn accepts_any_parser() {
        struct OneNodeParser;

        impl Parser for OneNodeParser {
            fn parse(&self, source: &Path) -> Result<ParsedKnowledge, Error> {
                let id = source.to_str().unwrap();
                Ok(ParsedKnowledge {
                    catalog: catalog(&[id]),
                    nodes: vec![node(id, &[])],
                })
            }
        }

        let memory = Memory::load(&OneNodeParser, &sources(), Path::new("x")).unwrap();

        assert_eq!(memory.node("x").unwrap(), Some(node("x", &[])));
    }

    #[test]
    fn evidence_may_point_at_other_nodes() {
        let memory = build(
            &["a", "结论"],
            vec![node("a", &[]), based_on("结论", &["a"])],
        )
        .unwrap();

        assert_eq!(memory.node_count(), 2);
    }

    #[test]
    fn every_node_needs_evidence() {
        let result = build(&["a"], vec![node_with_evidence("a", vec![])]);

        assert_eq!(
            problems(result),
            [Problem::MissingEvidence { id: "a".into() }]
        );
    }

    #[test]
    fn evidence_nodes_must_exist() {
        let result = build(&["a"], vec![based_on("a", &["幽灵"])]);

        // 依据不存在时只报这一个问题，不算循环。
        assert_eq!(
            problems(result),
            [Problem::BrokenEvidence {
                id: "幽灵".into(),
                referrer: "a".into(),
            }]
        );
    }

    #[test]
    fn circular_evidence_cannot_be_traced_to_a_source() {
        let result = build(
            &["a", "b", "下游", "地基", "稳固"],
            vec![
                based_on("a", &["b"]),
                based_on("b", &["a"]),
                based_on("下游", &["a"]),
                node("地基", &[]),
                based_on("稳固", &["地基"]),
            ],
        );

        assert_eq!(
            problems(result),
            [Problem::EvidenceCycle {
                ids: vec!["a".into(), "b".into(), "下游".into()],
            }]
        );
    }

    #[test]
    fn a_node_cannot_be_its_own_evidence() {
        let result = build(&["a"], vec![based_on("a", &["a"])]);

        assert_eq!(
            problems(result),
            [Problem::EvidenceCycle {
                ids: vec!["a".into()],
            }]
        );
    }

    #[test]
    fn checks_source_addresses_and_ranges() {
        let missing = source_id(b"not stored");
        let result = build(
            &["地址", "缺失", "越界", "空"],
            vec![
                node_with_evidence(
                    "地址",
                    vec![Evidence::Source {
                        source: "../script.md".into(),
                        locator: Locator::TextRange { start: 0, end: 1 },
                    }],
                ),
                node_with_evidence(
                    "缺失",
                    vec![Evidence::Source {
                        source: missing.clone(),
                        locator: Locator::TextRange { start: 0, end: 1 },
                    }],
                ),
                node_with_evidence("越界", vec![source_evidence(0, 13)]),
                node_with_evidence("空", vec![source_evidence(3, 3)]),
            ],
        );

        let size = TEXT.len() as u64;
        let source = source_id(TEXT.as_bytes());
        assert_eq!(
            problems(result),
            [
                Problem::InvalidSource {
                    source_id: "../script.md".into(),
                    referrer: "地址".into(),
                },
                Problem::MissingSource {
                    source_id: missing,
                    referrer: "缺失".into(),
                },
                Problem::InvalidRange {
                    source_id: source.clone(),
                    referrer: "越界".into(),
                    start: 0,
                    end: 13,
                    size,
                },
                Problem::InvalidRange {
                    source_id: source,
                    referrer: "空".into(),
                    start: 3,
                    end: 3,
                    size,
                },
            ]
        );
    }

    /// 校验不能把来源读进内存，每个来源也只查询一次大小。
    #[test]
    fn validation_only_asks_for_the_size_once_per_source() {
        struct CountingSources {
            inner: InMemorySources,
            size_calls: Cell<usize>,
        }

        impl SourceStore for CountingSources {
            fn size(&self, id: &str) -> Result<Option<u64>, Error> {
                self.size_calls.set(self.size_calls.get() + 1);
                self.inner.size(id)
            }

            fn read(&self, _id: &str, _locator: &Locator) -> Result<String, Error> {
                panic!("校验时不应该读取来源内容");
            }
        }

        let counting = CountingSources {
            inner: sources(),
            size_calls: Cell::new(0),
        };
        let parsed = ParsedKnowledge {
            catalog: catalog(&["a", "b"]),
            nodes: vec![node("a", &[]), node("b", &[])],
        };

        assert!(Memory::validate(&parsed, &counting).is_empty());
        assert_eq!(counting.size_calls.get(), 1);
    }

    #[test]
    fn source_errors_become_problems() {
        struct BrokenSources;

        impl SourceStore for BrokenSources {
            fn size(&self, _id: &str) -> Result<Option<u64>, Error> {
                Err(Error::Source("磁盘错误".into()))
            }

            fn read(&self, _id: &str, _locator: &Locator) -> Result<String, Error> {
                unreachable!()
            }
        }

        let parsed = ParsedKnowledge {
            catalog: catalog(&["a"]),
            nodes: vec![node("a", &[])],
        };

        let problems = Memory::validate(&parsed, &BrokenSources);

        assert_eq!(problems.len(), 1);
        assert!(problems[0].to_string().contains("磁盘错误"));
    }

    #[test]
    fn loads_evidence_on_demand() {
        let memory = build(
            &["a", "结论"],
            vec![
                node("a", &[]),
                node_with_evidence(
                    "结论",
                    vec![source_evidence(3, 6), Evidence::Node { id: "a".into() }],
                ),
            ],
        )
        .unwrap();

        let evidence = memory.evidence("结论", &sources()).unwrap();

        assert_eq!(
            evidence,
            [
                LoadedEvidence::Source {
                    source: source_id(TEXT.as_bytes()),
                    locator: Locator::TextRange { start: 3, end: 6 },
                    text: "段".into(),
                },
                LoadedEvidence::Node { id: "a".into() },
            ]
        );
        assert!(matches!(
            memory.evidence("不存在", &sources()),
            Err(Error::UnknownNode(_))
        ));
    }
}
