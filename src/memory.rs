use std::collections::{HashMap, HashSet};
use std::path::Path;

use thiserror::Error;

use crate::error::Error;
use crate::knowledge::{Catalog, Node, NodeId};
use crate::parse::{ParsedKnowledge, Parser};
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
}

impl Memory {
    /// 用 `parser` 解析 `source` 处的知识库并校验。
    pub fn load(parser: &dyn Parser, source: &Path) -> Result<Self, Error> {
        Self::from_parsed(parser.parse(source)?)
    }

    /// 校验解析结果并建图。有问题时一次报告所有问题。
    pub fn from_parsed(parsed: ParsedKnowledge) -> Result<Self, Error> {
        let problems = Self::validate(&parsed);
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
    pub fn validate(parsed: &ParsedKnowledge) -> Vec<Problem> {
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
        problems
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
    use super::*;
    use crate::knowledge::{CatalogEntry, Section};

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

    fn node(id: &str, related: &[&str]) -> Node {
        Node {
            id: id.to_string(),
            title: format!("标题{id}"),
            raw: format!("原始{id}"),
            understanding: format!("理解{id}"),
            related: related.iter().map(|r| r.to_string()).collect(),
        }
    }

    fn build(entries: &[&str], nodes: Vec<Node>) -> Result<Memory, Error> {
        Memory::from_parsed(ParsedKnowledge {
            catalog: catalog(entries),
            nodes,
        })
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

        let memory = Memory::load(&OneNodeParser, Path::new("x")).unwrap();

        assert_eq!(memory.node("x").unwrap(), Some(node("x", &[])));
    }
}
