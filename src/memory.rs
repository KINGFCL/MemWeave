use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::{self, NonNull};
use std::slice;
use std::str;

use thiserror::Error;

use crate::error::Error;
use crate::ffi::{self, RawBytes, RawEvidence, RawNode, RawNodeList};
use crate::knowledge::{Catalog, Evidence, Locator, Node, NodeId};
use crate::parse::{ParsedKnowledge, Parser};
use crate::source::{LoadedEvidence, SourceStore, is_valid_source_id};

/// 节点地址的长度上限。地址可能被用作文件名，大多数文件系统限制为 255 字节。
const MAX_ID_BYTES: usize = 200;

/// 内存中的知识图。只能通过校验后的解析结果创建，所以关联和总表都是一致的。
///
/// 节点由 C++ 分配，节点之间直接用地址相连。文本地址的索引只用在边界上：
/// 把模型的回答和命令行参数对应回节点，遍历时不经过它。
///
/// 一张图只属于一个线程。编译器会拒绝跨线程共享：
///
/// ```compile_fail
/// fn shared<T: Sync>() {}
/// shared::<memweave::memory::Memory>();
/// ```
pub struct Memory {
    catalog: Catalog,
    index: HashMap<NodeId, NonNull<RawNode>>,
}

// 整张图可以移动到另一个线程，但不实现 Sync：一张图只属于一个线程，别的线程要用就自己加载一份。
unsafe impl Send for Memory {}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("nodes", &self.index.len())
            .finish_non_exhaustive()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        for raw in self.index.values() {
            unsafe { ffi::mw_node_free(raw.as_ptr()) }
        }
    }
}

/// 一个节点的地址。读取字段是直接解引用，复制和比较都是比较地址。
#[derive(Clone, Copy)]
pub struct NodeRef<'m> {
    raw: NonNull<RawNode>,
    memory: PhantomData<&'m Memory>,
}

impl<'m> NodeRef<'m> {
    fn new(raw: NonNull<RawNode>) -> Self {
        Self {
            raw,
            memory: PhantomData,
        }
    }

    fn raw(self) -> &'m RawNode {
        // 节点来自 Memory，在 `'m` 期间一直有效。
        unsafe { self.raw.as_ref() }
    }

    pub fn id(self) -> &'m str {
        text(self.raw().id, self.raw().id_len)
    }

    pub fn title(self) -> &'m str {
        text(self.raw().title, self.raw().title_len)
    }

    pub fn description(self) -> &'m str {
        text(self.raw().description, self.raw().description_len)
    }

    /// 相关节点。遍历时只解引用地址，不做任何查找。
    pub fn related(self) -> impl ExactSizeIterator<Item = NodeRef<'m>> + Clone + use<'m> {
        node_list(self.raw().related)
    }

    /// 证据里引用了这个节点的节点。修改这个节点时，它们需要复查。
    pub fn dependents(self) -> impl ExactSizeIterator<Item = NodeRef<'m>> + Clone + use<'m> {
        node_list(unsafe { (*self.raw().backlinks).dependents })
    }

    /// `related` 里有这个节点的节点。删除这个节点时要清理它们。
    pub fn related_by(self) -> impl ExactSizeIterator<Item = NodeRef<'m>> + Clone + use<'m> {
        node_list(unsafe { (*self.raw().backlinks).related_by })
    }

    /// 这条知识依据的节点，直接沿地址到达。
    pub fn based_on(self) -> impl Iterator<Item = NodeRef<'m>> + use<'m> {
        self.raw_evidence()
            .iter()
            .filter(|evidence| evidence.kind == ffi::EVIDENCE_NODE)
            .map(|evidence| {
                NodeRef::new(NonNull::new(evidence.node).expect("建图时已填写依据节点"))
            })
    }

    /// 节点的证据。
    pub fn evidence(self) -> Vec<Evidence> {
        self.raw_evidence()
            .iter()
            .map(|evidence| {
                let id = text(evidence.id, evidence.id_len).to_owned();
                match evidence.kind {
                    ffi::EVIDENCE_TEXT_RANGE => Evidence::Source {
                        source: id,
                        locator: Locator::TextRange {
                            start: evidence.start as usize,
                            end: evidence.end as usize,
                        },
                    },
                    ffi::EVIDENCE_NODE => Evidence::Node { id },
                    kind => unreachable!("未知的证据种类 {kind}"),
                }
            })
            .collect()
    }

    fn raw_evidence(self) -> &'m [RawEvidence] {
        // 列表由 C++ 分配，头部之后紧跟 count 条证据。
        let list = self.raw().evidence;
        unsafe { slice::from_raw_parts(list.add(1).cast(), (*list).count) }
    }

    /// 复制出完整的节点。
    pub fn to_node(self) -> Node {
        Node {
            id: self.id().to_owned(),
            title: self.title().to_owned(),
            description: self.description().to_owned(),
            evidence: self.evidence(),
            related: self.related().map(|node| node.id().to_owned()).collect(),
        }
    }
}

impl fmt::Debug for NodeRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("NodeRef").field(&self.id()).finish()
    }
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.raw.as_ptr(), other.raw.as_ptr())
    }
}

impl Eq for NodeRef<'_> {}

impl Hash for NodeRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.as_ptr().hash(state);
    }
}

fn node_list<'m>(
    list: *const RawNodeList,
) -> impl ExactSizeIterator<Item = NodeRef<'m>> + Clone + use<'m> {
    let nodes: &'m [NonNull<RawNode>] = if list.is_null() {
        &[]
    } else {
        // 列表由 C++ 分配，头部之后紧跟 count 个节点地址。
        unsafe { slice::from_raw_parts(list.add(1).cast(), (*list).count) }
    };
    nodes.iter().map(|raw| NodeRef::new(*raw))
}

fn text<'m>(data: *const u8, len: u32) -> &'m str {
    // 内容是创建节点时从 &str 复制的，在 Memory 存活期间不变。
    unsafe { str::from_utf8_unchecked(slice::from_raw_parts(data, len as usize)) }
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

        // 反向关系从证据和关联反推，创建节点时就要知道数量。
        let backlinks = ReverseLinks::new(&nodes);

        // 先放进结构体：中途失败时由 Drop 释放已创建的节点。
        let mut memory = Self {
            catalog,
            index: HashMap::with_capacity(nodes.len()),
        };
        // 先创建所有节点，再连接它们，这样节点之间可以直接存地址。
        for node in &nodes {
            let raw = new_node(
                node,
                reverse_ids(&backlinks.dependents, &node.id).len(),
                reverse_ids(&backlinks.related_by, &node.id).len(),
            )?;
            memory.index.insert(node.id.clone(), raw);
        }

        let mut addresses = Vec::new();
        for node in &nodes {
            let raw = memory.index[&node.id].as_ptr();

            addresses.clear();
            addresses.extend(node.related.iter().map(|id| memory.index[id].as_ptr()));
            unsafe { ffi::mw_node_set_related(raw, addresses.as_ptr(), addresses.len()) };

            for (index, evidence) in node.evidence.iter().enumerate() {
                if let Evidence::Node { id } = evidence {
                    let target = memory.index[id].as_ptr();
                    unsafe { ffi::mw_node_set_evidence_target(raw, index, target) };
                }
            }

            addresses.clear();
            let dependents = reverse_ids(&backlinks.dependents, &node.id);
            addresses.extend(dependents.iter().map(|id| memory.index[*id].as_ptr()));
            unsafe { ffi::mw_node_set_dependents(raw, addresses.as_ptr(), addresses.len()) };

            addresses.clear();
            let related_by = reverse_ids(&backlinks.related_by, &node.id);
            addresses.extend(related_by.iter().map(|id| memory.index[*id].as_ptr()));
            unsafe { ffi::mw_node_set_related_by(raw, addresses.as_ptr(), addresses.len()) };
        }
        Ok(memory)
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
            .node(id)
            .ok_or_else(|| Error::UnknownNode(id.to_owned()))?;
        node.evidence()
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

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// 按文本地址找节点。这是边界上的入口，遍历时用 [`NodeRef::related`]。
    pub fn node(&self, id: &str) -> Option<NodeRef<'_>> {
        self.index.get(id).map(|raw| NodeRef::new(*raw))
    }

    /// 所有节点，顺序不固定。
    pub fn nodes(&self) -> impl Iterator<Item = NodeRef<'_>> {
        self.index.values().map(|raw| NodeRef::new(*raw))
    }

    pub fn node_count(&self) -> usize {
        self.index.len()
    }
}

/// 从证据和关联反推的反向关系：节点地址到指向它的节点。
struct ReverseLinks<'a> {
    dependents: HashMap<&'a str, Vec<&'a str>>,
    related_by: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> ReverseLinks<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut links = Self {
            dependents: HashMap::new(),
            related_by: HashMap::new(),
        };
        for node in nodes {
            let based_on = node.evidence.iter().filter_map(|evidence| match evidence {
                Evidence::Node { id } => Some(id.as_str()),
                Evidence::Source { .. } => None,
            });
            add_reverse(&mut links.dependents, &node.id, based_on);
            add_reverse(
                &mut links.related_by,
                &node.id,
                node.related.iter().map(String::as_str),
            );
        }
        links
    }
}

/// 记录 `from` 指向 `targets`。同一个节点多次指向同一个目标时只算一次。
fn add_reverse<'a>(
    reverse: &mut HashMap<&'a str, Vec<&'a str>>,
    from: &'a str,
    targets: impl Iterator<Item = &'a str>,
) {
    let mut seen = HashSet::new();
    for target in targets {
        if seen.insert(target) {
            reverse.entry(target).or_default().push(from);
        }
    }
}

fn reverse_ids<'r, 'a>(reverse: &'r HashMap<&'a str, Vec<&'a str>>, id: &str) -> &'r [&'a str] {
    reverse.get(id).map_or(&[], Vec::as_slice)
}

fn new_node(
    node: &Node,
    dependents_count: usize,
    related_by_count: usize,
) -> Result<NonNull<RawNode>, Error> {
    let mut evidence = Vec::with_capacity(node.evidence.len());
    for entry in &node.evidence {
        let (kind, id, start, end) = match entry {
            Evidence::Source {
                source,
                locator: Locator::TextRange { start, end },
            } => (ffi::EVIDENCE_TEXT_RANGE, source, *start as u64, *end as u64),
            Evidence::Node { id } => (ffi::EVIDENCE_NODE, id, 0, 0),
        };
        evidence.push(RawEvidence {
            kind,
            id_len: u32::try_from(id.len()).map_err(|_| Error::OutOfMemory)?,
            id: id.as_ptr(),
            node: ptr::null_mut(),
            start,
            end,
        });
    }
    let raw = unsafe {
        ffi::mw_node_new(
            RawBytes::from_slice(node.id.as_bytes()),
            RawBytes::from_slice(node.title.as_bytes()),
            RawBytes::from_slice(node.description.as_bytes()),
            evidence.as_ptr(),
            evidence.len(),
            node.related.len(),
            dependents_count,
            related_by_count,
        )
    };
    NonNull::new(raw).ok_or(Error::OutOfMemory)
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
        assert_eq!(memory.catalog(), &catalog(&["a"]));
        assert_eq!(memory.node("c").unwrap().to_node(), node("c", &["a"]));
        assert!(memory.node("不存在").is_none());
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

        assert_eq!(memory.node("x").unwrap().to_node(), node("x", &[]));
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

    #[test]
    fn nodes_are_connected_by_address() {
        let memory = build(
            &["a"],
            vec![node("a", &["b", "c"]), node("b", &["a"]), node("c", &[])],
        )
        .unwrap();

        let a = memory.node("a").unwrap();
        let related: Vec<NodeRef<'_>> = a.related().collect();

        assert_eq!(
            related,
            [memory.node("b").unwrap(), memory.node("c").unwrap()]
        );
        // 沿着地址走回来，得到的是同一个节点。
        assert!(related[0].related().next().unwrap() == a);
        assert_eq!(related[1].related().len(), 0);
        assert_eq!(a.id(), "a");
        assert_eq!(a.title(), "标题a");
        assert_eq!(a.description(), "描述a");
    }

    #[test]
    fn empty_strings_are_stored() {
        let mut empty = node("a", &[]);
        empty.title = String::new();
        empty.description = String::new();

        let memory = build(&["a"], vec![empty.clone()]).unwrap();

        assert_eq!(memory.node("a").unwrap().to_node(), empty);
    }

    #[test]
    fn long_text_survives_the_pool() {
        // 超过 chunk 大小的内容走单独的分配。
        let mut large = node("a", &[]);
        large.description = "长".repeat(2 * 1024 * 1024);

        let memory = build(&["a"], vec![large.clone()]).unwrap();

        assert_eq!(memory.node("a").unwrap().description(), large.description);
    }

    /// 关联再多，列表也紧跟在节点后面，遍历时不用跳到别处。
    #[test]
    fn links_follow_the_node_however_many_there_are() {
        let ids: Vec<String> = (0..100).map(|index| format!("n{index}")).collect();
        let mut nodes: Vec<Node> = ids.iter().map(|id| node(id, &[])).collect();
        let mut hub = node("hub", &[]);
        hub.related = ids.clone();
        nodes.push(hub);
        let memory = build(&["hub"], nodes).unwrap();

        let hub = memory.node("hub").unwrap();
        let address = hub.raw.as_ptr() as usize;

        assert_eq!(address % 128, 0);
        assert_eq!(hub.raw().related as usize, address + 64);
        let related: Vec<&str> = hub.related().map(NodeRef::id).collect();
        assert_eq!(related, ids);
    }

    #[test]
    fn nodes_and_their_links_share_a_cache_line_pair() {
        let memory = build(&["a"], vec![node("a", &["b"]), node("b", &[])]).unwrap();

        let a = memory.node("a").unwrap();
        let address = a.raw.as_ptr() as usize;
        let list = a.raw().related as usize;

        assert_eq!(address % 128, 0);
        assert_eq!(list, address + 64);
    }

    #[test]
    fn memory_can_move_to_another_thread() {
        let memory = build(&["a"], vec![node("a", &[])]).unwrap();

        let count = std::thread::spawn(move || memory.node_count())
            .join()
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn evidence_stores_the_id_and_the_address() {
        let memory = build(
            &["依据", "结论"],
            vec![
                node("依据", &[]),
                node_with_evidence(
                    "结论",
                    vec![
                        source_evidence(0, 3),
                        Evidence::Node {
                            id: "依据".into()
                        },
                    ],
                ),
            ],
        )
        .unwrap();

        let conclusion = memory.node("结论").unwrap();

        assert_eq!(
            conclusion.evidence(),
            [
                source_evidence(0, 3),
                Evidence::Node {
                    id: "依据".into()
                },
            ]
        );
        let based_on: Vec<NodeRef<'_>> = conclusion.based_on().collect();
        assert_eq!(based_on, [memory.node("依据").unwrap()]);
        assert_eq!(memory.node("依据").unwrap().based_on().count(), 0);
    }

    #[test]
    fn dependents_are_the_reverse_of_evidence() {
        let memory = build(
            &["地基", "a", "b", "c"],
            vec![
                node("地基", &[]),
                based_on("a", &["地基"]),
                // 重复引用同一个依据只算一次。
                based_on("b", &["地基", "地基", "a"]),
                based_on("c", &["b"]),
            ],
        )
        .unwrap();

        let ids = |id| -> Vec<String> {
            memory
                .node(id)
                .unwrap()
                .dependents()
                .map(|node| node.id().to_owned())
                .collect()
        };
        assert_eq!(ids("地基"), ["a", "b"]);
        assert_eq!(ids("a"), ["b"]);
        assert_eq!(ids("b"), ["c"]);
        assert!(ids("c").is_empty());
    }

    #[test]
    fn related_by_is_the_reverse_of_related() {
        let memory = build(
            &["a"],
            vec![
                node("a", &["b", "c"]),
                // 重复关联同一个节点只算一次。
                node("b", &["c", "c"]),
                node("c", &["a"]),
            ],
        )
        .unwrap();

        let ids = |id| -> Vec<String> {
            memory
                .node(id)
                .unwrap()
                .related_by()
                .map(|node| node.id().to_owned())
                .collect()
        };
        assert_eq!(ids("a"), ["c"]);
        assert_eq!(ids("b"), ["a"]);
        assert_eq!(ids("c"), ["a", "b"]);
    }

    #[test]
    fn related_and_derivation_are_tracked_separately() {
        // b 既和 a 相关，又依据 a；c 只和 a 相关；d 只依据 a。
        let mut b = based_on("b", &["a"]);
        b.related = vec!["a".into()];
        let mut c = node("c", &["a"]);
        c.related = vec!["a".into()];
        let memory = build(
            &["a", "b", "c", "d"],
            vec![node("a", &[]), b, c, based_on("d", &["a"])],
        )
        .unwrap();

        let a = memory.node("a").unwrap();

        let related_by: Vec<&str> = a.related_by().map(NodeRef::id).collect();
        let dependents: Vec<&str> = a.dependents().map(NodeRef::id).collect();
        assert_eq!(related_by, ["b", "c"]);
        assert_eq!(dependents, ["b", "d"]);
    }

    #[test]
    fn backlinks_are_not_part_of_the_stored_node() {
        let original = based_on("结论", &["依据"]);
        let memory = build(&["依据", "结论"], vec![node("依据", &[]), original.clone()]).unwrap();

        assert_eq!(memory.node("结论").unwrap().to_node(), original);
        assert_eq!(memory.node("依据").unwrap().to_node(), node("依据", &[]));
    }
}
