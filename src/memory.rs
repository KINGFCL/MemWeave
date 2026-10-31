use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::{self, NonNull};
use std::slice;
use std::str;

use serde::Serialize;
use thiserror::Error;

use crate::error::Error;
use crate::ffi::{self, RawBytes, RawEvidence, RawNode, RawNodeList};
use crate::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, NodeId, Section, SourceId};
use crate::parse::{ParsedKnowledge, Parser};
use crate::source::{LoadedEvidence, SourceStore, is_valid_source_id};

/// 节点地址的长度上限。地址可能被用作文件名，大多数文件系统限制为 255 字节。
const MAX_ID_BYTES: usize = 200;

/// 内存中的知识图。创建和 [`add_node`](Memory::add_node) 时都会校验，所以节点之间的地址总是有效的。
///
/// 节点由 C++ 分配，节点之间直接用地址相连。文本地址只用在边界上：
/// 把模型的回答和命令行参数对应回节点，遍历时不经过它。这个 set 由 C++ 的 `NodeIndex` 实现。
///
/// 一张图只属于一个线程。编译器会拒绝跨线程共享：
///
/// ```compile_fail
/// fn shared<T: Sync>() {}
/// shared::<memweave::memory::Memory>();
/// ```
pub struct Memory {
    catalog: Catalog,
    index: NonNull<ffi::RawIndex>,
}

// 整张图可以移动到另一个线程，但不实现 Sync：一张图只属于一个线程，别的线程要用就自己加载一份。
unsafe impl Send for Memory {}

impl fmt::Debug for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Memory")
            .field("nodes", &self.node_count())
            .finish_non_exhaustive()
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let mut cursor = 0;
        loop {
            let raw = unsafe { ffi::mw_index_next(self.index.as_ptr(), &raw mut cursor) };
            if raw.is_null() {
                break;
            }
            unsafe { ffi::mw_node_free(raw) };
        }
        unsafe { ffi::mw_index_free(self.index.as_ptr()) };
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
    pub fn based_by(self) -> impl ExactSizeIterator<Item = NodeRef<'m>> + Clone + use<'m> {
        node_list(unsafe { (*self.raw().backlinks).based_by })
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
            .filter_map(|evidence| NonNull::new(evidence.node).map(NodeRef::new))
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
        unsafe { slice::from_raw_parts(list.add(1).cast(), (*list).count as usize) }
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

    #[error("要关联到节点 {target} 的节点 {id} 不存在")]
    MissingIncoming { id: NodeId, target: NodeId },

    #[error("节点 {id} 没有引用来源 {cited} 的 {start}..{end}")]
    NotCited {
        id: NodeId,
        cited: SourceId,
        start: usize,
        end: usize,
    },

    #[error("板块“{name}”重复")]
    DuplicateSection { name: String },

    #[error("板块“{name}”不存在")]
    UnknownSection { name: String },

    #[error("节点 {id} 已经是板块“{section}”的代表")]
    AlreadyRepresentative { id: NodeId, section: String },

    #[error("节点 {id} 不是板块“{section}”的代表")]
    NotRepresentative { id: NodeId, section: String },

    #[error("不能删除节点 {id}，{} 还依据它", .by.join("、"))]
    StillBasedOn { id: NodeId, by: Vec<NodeId> },
}

/// 删除一个节点的结果。
///
/// 有结论依据的节点不能删除，所以这里只有相关性受影响。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovedNode {
    /// 被删除节点的完整内容。
    pub node: Node,
    /// 关联它的节点。它们少了一条路径，有些节点可能因此查不到了，见 [`Memory::unreachable`]。
    pub related_by: Vec<NodeId>,
    /// 它原来是哪些板块的代表节点。这些板块可能已经空了，要不要保留由模型决定。
    pub sections: Vec<String>,
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
        let ParsedKnowledge { catalog, mut nodes } = parsed;
        nodes.iter_mut().for_each(remove_duplicates);

        // 反向关系从证据和关联反推，创建节点时就要知道数量。
        let backlinks = ReverseLinks::new(&nodes);

        // 先放进结构体：中途失败时由 Drop 释放已创建的节点。
        let index = NonNull::new(unsafe { ffi::mw_index_new() }).ok_or(Error::OutOfMemory)?;
        let memory = Self { catalog, index };
        // 先创建所有节点，再连接它们，这样节点之间可以直接存地址。
        for node in &nodes {
            let raw = new_node(
                node,
                reverse_ids(&backlinks.based_by, &node.id).len(),
                reverse_ids(&backlinks.related_by, &node.id).len(),
            )?;
            if let Err(error) = memory.index_insert(raw) {
                unsafe { ffi::mw_node_free(raw.as_ptr()) };
                return Err(error);
            }
        }

        let mut addresses = Vec::new();
        for node in &nodes {
            let raw = memory.lookup(&node.id).as_ptr();

            addresses.clear();
            addresses.extend(node.related.iter().map(|id| memory.lookup(id).as_ptr()));
            set_links(raw, ffi::LINK_RELATED, &addresses);

            for (index, evidence) in node.evidence.iter().enumerate() {
                if let Evidence::Node { id } = evidence {
                    let target = memory.lookup(id).as_ptr();
                    unsafe { ffi::mw_node_set_evidence_target(raw, index, target) };
                }
            }

            addresses.clear();
            let based_by = reverse_ids(&backlinks.based_by, &node.id);
            addresses.extend(based_by.iter().map(|id| memory.lookup(id).as_ptr()));
            set_links(raw, ffi::LINK_BASED_BY, &addresses);

            addresses.clear();
            let related_by = reverse_ids(&backlinks.related_by, &node.id);
            addresses.extend(related_by.iter().map(|id| memory.lookup(id).as_ptr()));
            set_links(raw, ffi::LINK_RELATED_BY, &addresses);
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

        let mut section_names = HashSet::new();
        for section in &parsed.catalog.sections {
            if !section_names.insert(section.name.as_str()) {
                problems.push(Problem::DuplicateSection {
                    name: section.name.clone(),
                });
            }
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

        check_evidence(
            &parsed.nodes,
            &|id| by_id.contains_key(id),
            sources,
            &mut problems,
        );
        let cyclic = nodes_in_evidence_cycles(parsed, &by_id);
        if !cyclic.is_empty() {
            problems.push(Problem::EvidenceCycle { ids: cyclic });
        }
        problems
    }

    /// 添加一个节点，并接入图。`related_by` 里的节点会关联它。
    ///
    /// 证据只能依据已有的节点，所以不会形成循环。来源必须已经在 `sources` 里。
    /// 这里不要求它能从总表到达，之后可以用 [`Memory::catalog_add`] 或 [`Memory::link`] 接入。
    /// 有问题时一次报告所有问题，图保持不变。
    pub fn add_node(
        &mut self,
        mut node: Node,
        related_by: &[NodeId],
        sources: &dyn SourceStore,
    ) -> Result<NodeRef<'_>, Error> {
        let problems = self.check_new_node(&node, related_by, sources);
        if !problems.is_empty() {
            return Err(Error::InvalidMemory(problems));
        }
        remove_duplicates(&mut node);

        let mut seen = HashSet::new();
        let related_targets: Vec<NonNull<RawNode>> = node
            .related
            .iter()
            .filter(|id| **id != node.id && seen.insert(id.as_str()))
            .map(|id| self.lookup(id))
            .collect();
        let mut seen = HashSet::new();
        let evidence_targets: Vec<NonNull<RawNode>> = node
            .evidence
            .iter()
            .filter_map(|evidence| match evidence {
                Evidence::Node { id } if seen.insert(id.as_str()) => Some(self.lookup(id)),
                _ => None,
            })
            .collect();
        let mut seen = HashSet::new();
        let incoming: Vec<NonNull<RawNode>> = related_by
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .map(|id| self.lookup(id))
            .collect();

        let links_to_itself = node.related.contains(&node.id);
        let raw = new_node(&node, 0, incoming.len() + usize::from(links_to_itself))?;

        // 先把所有位置都预留好，后面的连接就不会失败，图不会停在只连了一半的状态。
        let reserved = related_targets
            .iter()
            .all(|target| reserve_link(*target, ffi::LINK_RELATED_BY))
            && evidence_targets
                .iter()
                .all(|target| reserve_link(*target, ffi::LINK_BASED_BY))
            && incoming
                .iter()
                .all(|from| reserve_link(*from, ffi::LINK_RELATED));
        // set 也可能需要扩容，放在连接之前，这样后面的步骤都不会失败。
        if !reserved || self.index_insert(raw).is_err() {
            unsafe { ffi::mw_node_free(raw.as_ptr()) };
            return Err(Error::OutOfMemory);
        }

        let related: Vec<*mut RawNode> = node
            .related
            .iter()
            .map(|id| {
                if *id == node.id {
                    raw.as_ptr()
                } else {
                    self.lookup(id).as_ptr()
                }
            })
            .collect();
        set_links(raw.as_ptr(), ffi::LINK_RELATED, &related);

        for (index, evidence) in node.evidence.iter().enumerate() {
            if let Evidence::Node { id } = evidence {
                let target = self.lookup(id).as_ptr();
                unsafe { ffi::mw_node_set_evidence_target(raw.as_ptr(), index, target) };
            }
        }

        let mut related_by: Vec<*mut RawNode> = incoming.iter().map(|n| n.as_ptr()).collect();
        if links_to_itself {
            related_by.push(raw.as_ptr());
        }
        set_links(raw.as_ptr(), ffi::LINK_RELATED_BY, &related_by);

        for target in &related_targets {
            append_link(*target, ffi::LINK_RELATED_BY, raw);
        }
        for target in &evidence_targets {
            append_link(*target, ffi::LINK_BASED_BY, raw);
        }
        for from in &incoming {
            append_link(*from, ffi::LINK_RELATED, raw);
        }

        Ok(NodeRef::new(raw))
    }

    fn check_new_node(
        &self,
        node: &Node,
        related_by: &[NodeId],
        sources: &dyn SourceStore,
    ) -> Vec<Problem> {
        let mut problems = Vec::new();
        if !is_valid_id(&node.id) {
            problems.push(Problem::InvalidId {
                id: node.id.clone(),
            });
        }
        if self.contains(&node.id) {
            problems.push(Problem::DuplicateNode {
                id: node.id.clone(),
            });
        }
        for related in &node.related {
            if *related != node.id && !self.contains(related) {
                problems.push(Problem::BrokenLink {
                    id: related.clone(),
                    referrer: node.id.clone(),
                });
            }
        }
        for id in related_by {
            if !self.contains(id) {
                problems.push(Problem::MissingIncoming {
                    id: id.clone(),
                    target: node.id.clone(),
                });
            }
        }
        check_evidence(
            slice::from_ref(node),
            &|id| self.contains(id),
            sources,
            &mut problems,
        );
        problems
    }

    /// 删除一个节点，并清除图里所有指向它的地址，不会留下悬空指针。
    ///
    /// 还有结论依据它时不能删除，所以推导关系永远指向真实存在的节点。怎么处理这些结论由模型
    /// 决定：换依据，或者先删除它们。相关性可以直接清理，但可能有节点因此查不到了，
    /// 见 [`Memory::unreachable`]。
    pub fn remove_node(&mut self, id: &str) -> Result<RemovedNode, Error> {
        let raw = self.find(id)?;
        let node = NodeRef::new(raw);
        if node.based_by().len() != 0 {
            return Err(Error::InvalidMemory(vec![Problem::StillBasedOn {
                id: id.to_owned(),
                by: node.based_by().map(|other| other.id().to_owned()).collect(),
            }]));
        }
        let other_ids = |nodes: Vec<NodeRef<'_>>| -> Vec<NodeId> {
            nodes
                .into_iter()
                .filter(|other| other.raw != raw)
                .map(|other| other.id().to_owned())
                .collect()
        };
        let removed = RemovedNode {
            node: node.to_node(),
            related_by: other_ids(node.related_by().collect()),
            sections: self
                .catalog
                .sections
                .iter()
                .filter(|section| section.nodes.iter().any(|entry| entry.id == id))
                .map(|section| section.name.clone())
                .collect(),
        };

        for section in &mut self.catalog.sections {
            section.nodes.retain(|entry| entry.id != id);
        }
        unsafe {
            ffi::mw_index_erase(self.index.as_ptr(), RawBytes::from_slice(id.as_bytes()));
            ffi::mw_node_detach(raw.as_ptr());
            ffi::mw_node_free(raw.as_ptr());
        }
        Ok(removed)
    }

    /// 从总表出发沿关联走不到的节点，按 id 排序。要走遍全图，适合一批修改结束后调用一次。
    pub fn unreachable(&self) -> Vec<NodeId> {
        let mut reachable = HashSet::new();
        let mut pending: Vec<NodeRef<'_>> = self
            .catalog
            .sections
            .iter()
            .flat_map(|section| &section.nodes)
            .filter_map(|entry| self.node(&entry.id))
            .collect();
        while let Some(node) = pending.pop() {
            if reachable.insert(node) {
                pending.extend(node.related());
            }
        }
        let mut lost: Vec<NodeId> = self
            .nodes()
            .filter(|node| !reachable.contains(node))
            .map(|node| node.id().to_owned())
            .collect();
        lost.sort_unstable();
        lost
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
        self.find_raw(id).map(NodeRef::new)
    }

    /// 这个文本地址是否存在。由 C++ 的 set 回答，模型访问的每个地址都要经过它。
    pub fn contains(&self, id: &str) -> bool {
        self.find_raw(id).is_some()
    }

    /// 所有节点，顺序不固定。
    pub fn nodes(&self) -> impl Iterator<Item = NodeRef<'_>> {
        let mut cursor = 0;
        std::iter::from_fn(move || {
            let raw = unsafe { ffi::mw_index_next(self.index.as_ptr(), &raw mut cursor) };
            NonNull::new(raw).map(NodeRef::new)
        })
    }

    pub fn node_count(&self) -> usize {
        unsafe { ffi::mw_index_size(self.index.as_ptr()) }
    }

    /// 新建一个空板块。
    pub fn catalog_create(&mut self, name: &str, description: &str) -> Result<(), Error> {
        if self.section_index(name).is_some() {
            return Err(Error::InvalidMemory(vec![Problem::DuplicateSection {
                name: name.to_owned(),
            }]));
        }
        self.catalog.sections.push(Section {
            name: name.to_owned(),
            description: description.to_owned(),
            nodes: Vec::new(),
        });
        Ok(())
    }

    /// 修改板块的描述。
    pub fn catalog_describe(&mut self, name: &str, description: &str) -> Result<(), Error> {
        let index = self.find_section(name)?;
        description.clone_into(&mut self.catalog.sections[index].description);
        Ok(())
    }

    /// 修改板块的名字。
    pub fn catalog_rename(&mut self, name: &str, new_name: &str) -> Result<(), Error> {
        let index = self.find_section(name)?;
        if self.section_index(new_name).is_some() {
            return Err(Error::InvalidMemory(vec![Problem::DuplicateSection {
                name: new_name.to_owned(),
            }]));
        }
        new_name.clone_into(&mut self.catalog.sections[index].name);
        Ok(())
    }

    /// 删除板块，返回它原来的代表节点。节点本身保留，但可能有节点因此查不到了。
    pub fn catalog_delete(&mut self, name: &str) -> Result<Vec<CatalogEntry>, Error> {
        let index = self.find_section(name)?;
        Ok(self.catalog.sections.remove(index).nodes)
    }

    /// 让节点成为板块的代表。标题取自节点，之后修改标题时会同步。
    pub fn catalog_add(&mut self, id: &str, section: &str) -> Result<(), Error> {
        let title = NodeRef::new(self.find(id)?).title().to_owned();
        let index = self.find_section(section)?;
        let nodes = &mut self.catalog.sections[index].nodes;
        if nodes.iter().any(|entry| entry.id == id) {
            return Err(Error::InvalidMemory(vec![Problem::AlreadyRepresentative {
                id: id.to_owned(),
                section: section.to_owned(),
            }]));
        }
        nodes.push(CatalogEntry {
            id: id.to_owned(),
            title,
        });
        Ok(())
    }

    /// 撤下板块的代表，节点本身保留。之后可能有节点查不到了，见 [`Memory::unreachable`]。
    pub fn catalog_remove(&mut self, id: &str, section: &str) -> Result<(), Error> {
        let index = self.find_section(section)?;
        let nodes = &mut self.catalog.sections[index].nodes;
        let Some(position) = nodes.iter().position(|entry| entry.id == id) else {
            return Err(Error::InvalidMemory(vec![Problem::NotRepresentative {
                id: id.to_owned(),
                section: section.to_owned(),
            }]));
        };
        nodes.remove(position);
        Ok(())
    }

    fn section_index(&self, name: &str) -> Option<usize> {
        self.catalog.sections.iter().position(|s| s.name == name)
    }

    fn find_section(&self, name: &str) -> Result<usize, Error> {
        self.section_index(name).ok_or_else(|| {
            Error::InvalidMemory(vec![Problem::UnknownSection {
                name: name.to_owned(),
            }])
        })
    }

    /// 修改节点的标题，总表里的标题也跟着改。
    pub fn set_title(&mut self, id: &str, title: &str) -> Result<(), Error> {
        self.set_text(id, ffi::TEXT_TITLE, title)?;
        for entry in self.catalog.sections.iter_mut().flat_map(|s| &mut s.nodes) {
            if entry.id == id {
                title.clone_into(&mut entry.title);
            }
        }
        Ok(())
    }

    /// 修改节点的描述。
    pub fn set_description(&mut self, id: &str, description: &str) -> Result<(), Error> {
        self.set_text(id, ffi::TEXT_DESCRIPTION, description)
    }

    /// 新增一条关联 `from -> to`，两个方向都会更新。
    pub fn link(&mut self, from: &str, to: &str) -> Result<(), Error> {
        let (from, to) = (self.find(from)?, self.find(to)?);
        if unsafe { ffi::mw_node_link(from.as_ptr(), to.as_ptr()) } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    /// 去掉关联 `from -> to`，两个方向都会更新。之后可能有节点查不到了，见 [`Memory::unreachable`]。
    pub fn unlink(&mut self, from: &str, to: &str) -> Result<(), Error> {
        let (from, to) = (self.find(from)?, self.find(to)?);
        unsafe { ffi::mw_node_unlink(from.as_ptr(), to.as_ptr()) };
        Ok(())
    }

    /// 让 `id` 依据 `on`，两个方向都会更新。证据不能形成循环。
    pub fn base(&mut self, id: &str, on: &str) -> Result<(), Error> {
        let (node, target) = (self.find(id)?, self.find(on)?);
        if id == on || depends_on(NodeRef::new(target), NodeRef::new(node)) {
            return Err(Error::InvalidMemory(vec![Problem::EvidenceCycle {
                ids: vec![id.to_owned(), on.to_owned()],
            }]));
        }
        if unsafe { ffi::mw_node_add_evidence(node.as_ptr(), target.as_ptr()) } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    /// 去掉 `id` 对 `on` 的依据。节点不能因此失去所有证据。
    pub fn unbase(&mut self, id: &str, on: &str) -> Result<(), Error> {
        let raw = self.find(id)?;
        self.find(on)?;
        let evidence = NodeRef::new(raw).evidence();
        let remaining = evidence
            .iter()
            .filter(|entry| !matches!(entry, Evidence::Node { id: other } if other == on))
            .count();
        if remaining == 0 {
            return Err(Error::InvalidMemory(vec![Problem::MissingEvidence {
                id: id.to_owned(),
            }]));
        }
        let bytes = RawBytes::from_slice(on.as_bytes());
        if unsafe { ffi::mw_node_remove_evidence(raw.as_ptr(), bytes) } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    /// 让 `id` 引用来源里的一段，加在证据列表末尾。已经引用了同一段时什么也不做。
    pub fn cite(
        &mut self,
        id: &str,
        source: &str,
        locator: &Locator,
        sources: &dyn SourceStore,
    ) -> Result<(), Error> {
        let raw = self.find(id)?;
        let citation = Evidence::Source {
            source: source.to_owned(),
            locator: locator.clone(),
        };
        let problems = check_sources(id, slice::from_ref(&citation), sources);
        if !problems.is_empty() {
            return Err(Error::InvalidMemory(problems));
        }
        if NodeRef::new(raw).evidence().contains(&citation) {
            return Ok(());
        }
        let Locator::TextRange { start, end } = *locator;
        let bytes = RawBytes::from_slice(source.as_bytes());
        if unsafe {
            ffi::mw_node_add_source_evidence(raw.as_ptr(), bytes, start as u64, end as u64)
        } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    /// 去掉 `id` 对来源里这一段的引用，同一来源的其他范围保留。节点不能因此失去所有证据。
    pub fn uncite(&mut self, id: &str, source: &str, locator: &Locator) -> Result<(), Error> {
        let raw = self.find(id)?;
        let citation = Evidence::Source {
            source: source.to_owned(),
            locator: locator.clone(),
        };
        let evidence = NodeRef::new(raw).evidence();
        let Locator::TextRange { start, end } = *locator;
        if !evidence.contains(&citation) {
            return Err(Error::InvalidMemory(vec![Problem::NotCited {
                id: id.to_owned(),
                cited: source.to_owned(),
                start,
                end,
            }]));
        }
        if evidence.iter().all(|entry| *entry == citation) {
            return Err(Error::InvalidMemory(vec![Problem::MissingEvidence {
                id: id.to_owned(),
            }]));
        }
        let bytes = RawBytes::from_slice(source.as_bytes());
        if unsafe {
            ffi::mw_node_remove_source_evidence(raw.as_ptr(), bytes, start as u64, end as u64)
        } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    fn set_text(&mut self, id: &str, field: u32, text: &str) -> Result<(), Error> {
        let raw = self.find(id)?;
        let bytes = RawBytes::from_slice(text.as_bytes());
        if unsafe { ffi::mw_node_set_text(raw.as_ptr(), field, bytes) } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }

    fn find_raw(&self, id: &str) -> Option<NonNull<RawNode>> {
        let bytes = RawBytes::from_slice(id.as_bytes());
        NonNull::new(unsafe { ffi::mw_index_find(self.index.as_ptr(), bytes) })
    }

    fn find(&self, id: &str) -> Result<NonNull<RawNode>, Error> {
        self.find_raw(id)
            .ok_or_else(|| Error::UnknownNode(id.to_owned()))
    }

    /// 校验已经保证这个地址存在。
    fn lookup(&self, id: &str) -> NonNull<RawNode> {
        self.find_raw(id).expect("校验已经保证节点存在")
    }

    fn index_insert(&self, raw: NonNull<RawNode>) -> Result<(), Error> {
        if unsafe { ffi::mw_index_insert(self.index.as_ptr(), raw.as_ptr()) } {
            Ok(())
        } else {
            Err(Error::OutOfMemory)
        }
    }
}

/// 去掉重复的关联和证据，保留第一次出现的。重复的条目没有含义，还会让证据的数量算错。
fn remove_duplicates(node: &mut Node) {
    let mut seen = HashSet::new();
    node.related.retain(|id| seen.insert(id.clone()));
    let mut kept: Vec<Evidence> = Vec::with_capacity(node.evidence.len());
    for evidence in node.evidence.drain(..) {
        if !kept.contains(&evidence) {
            kept.push(evidence);
        }
    }
    node.evidence = kept;
}

/// 从证据和关联反推的反向关系：节点地址到指向它的节点。
struct ReverseLinks<'a> {
    based_by: HashMap<&'a str, Vec<&'a str>>,
    related_by: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> ReverseLinks<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut links = Self {
            based_by: HashMap::new(),
            related_by: HashMap::new(),
        };
        for node in nodes {
            let based_on = node.evidence.iter().filter_map(|evidence| match evidence {
                Evidence::Node { id } => Some(id.as_str()),
                Evidence::Source { .. } => None,
            });
            add_reverse(&mut links.based_by, &node.id, based_on);
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
    based_by_count: usize,
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
            based_by_count,
            related_by_count,
        )
    };
    NonNull::new(raw).ok_or(Error::OutOfMemory)
}

/// `node` 是否直接或间接依据 `target`。
fn depends_on(node: NodeRef<'_>, target: NodeRef<'_>) -> bool {
    let mut seen = HashSet::new();
    let mut pending = vec![node.raw];
    while let Some(raw) = pending.pop() {
        if raw == target.raw {
            return true;
        }
        if seen.insert(raw) {
            pending.extend(NodeRef::new(raw).based_on().map(|other| other.raw));
        }
    }
    false
}

/// 检查一组证据指向的来源是否存在、范围是否有效。
pub(crate) fn check_sources(
    id: &str,
    evidence: &[Evidence],
    sources: &dyn SourceStore,
) -> Vec<Problem> {
    let node = Node {
        id: id.to_owned(),
        title: String::new(),
        description: String::new(),
        evidence: evidence.to_vec(),
        related: Vec::new(),
    };
    let mut problems = Vec::new();
    check_evidence(slice::from_ref(&node), &|_| true, sources, &mut problems);
    problems
}

fn set_links(raw: *mut RawNode, kind: u32, nodes: &[*mut RawNode]) {
    unsafe { ffi::mw_node_set_links(raw, kind, nodes.as_ptr(), nodes.len()) };
}

fn reserve_link(raw: NonNull<RawNode>, kind: u32) -> bool {
    unsafe { ffi::mw_node_reserve_links(raw.as_ptr(), kind, 1) }
}

fn append_link(raw: NonNull<RawNode>, kind: u32, target: NonNull<RawNode>) {
    unsafe { ffi::mw_node_append_link(raw.as_ptr(), kind, target.as_ptr()) };
}

pub(crate) fn is_valid_id(id: &str) -> bool {
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
    nodes: &[Node],
    node_exists: &dyn Fn(&str) -> bool,
    sources: &dyn SourceStore,
    problems: &mut Vec<Problem>,
) {
    // 一个来源可能被大量证据引用，每个只查询一次大小。
    let mut sizes: HashMap<&str, Result<Option<u64>, ()>> = HashMap::new();

    for node in nodes {
        if node.evidence.is_empty() {
            problems.push(Problem::MissingEvidence {
                id: node.id.clone(),
            });
        }
        for evidence in &node.evidence {
            match evidence {
                Evidence::Node { id } => {
                    if !node_exists(id) {
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
    let mut based_by: HashMap<&str, Vec<&str>> = HashMap::new();
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
            based_by.entry(dependency).or_default().push(&node.id);
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
        for dependent in based_by.get(id).into_iter().flatten() {
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
                description: String::new(),
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

            fn read_all(&self, _id: &str) -> Result<Vec<u8>, Error> {
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

            fn read_all(&self, _id: &str) -> Result<Vec<u8>, Error> {
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
    fn based_by_are_the_reverse_of_evidence() {
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
                .based_by()
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
        let based_by: Vec<&str> = a.based_by().map(NodeRef::id).collect();
        assert_eq!(related_by, ["b", "c"]);
        assert_eq!(based_by, ["b", "d"]);
    }

    #[test]
    fn backlinks_are_not_part_of_the_stored_node() {
        let original = based_on("结论", &["依据"]);
        let memory = build(&["依据", "结论"], vec![node("依据", &[]), original.clone()]).unwrap();

        assert_eq!(memory.node("结论").unwrap().to_node(), original);
        assert_eq!(memory.node("依据").unwrap().to_node(), node("依据", &[]));
    }

    fn memory_of(entries: &[&str], nodes: Vec<Node>) -> Memory {
        build(entries, nodes).unwrap()
    }

    fn linked_from(ids: &[&str]) -> Vec<NodeId> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn ids<'m>(nodes: impl Iterator<Item = NodeRef<'m>>) -> Vec<&'m str> {
        nodes.map(NodeRef::id).collect()
    }

    #[test]
    fn add_connects_every_direction() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);
        memory
            .add_node(node("基础", &[]), &linked_from(&["a"]), &sources())
            .unwrap();
        let mut new = based_on("新", &["基础"]);
        new.related = vec!["a".into()];

        let added = memory
            .add_node(new.clone(), &linked_from(&["基础"]), &sources())
            .unwrap();
        assert_eq!(added.to_node(), new);
        assert_eq!(ids(added.related()), ["a"]);
        assert_eq!(ids(added.related_by()), ["基础"]);
        assert_eq!(ids(added.based_on()), ["基础"]);

        let foundation = memory.node("基础").unwrap();
        assert_eq!(ids(foundation.related()), ["新"]);
        assert_eq!(ids(foundation.based_by()), ["新"]);
        assert_eq!(ids(memory.node("a").unwrap().related_by()), ["新"]);
        assert!(memory.unreachable().is_empty());
        assert_eq!(memory.node_count(), 3);
    }

    #[test]
    fn citing_and_unciting_change_only_that_range() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);
        let original = memory.node("a").unwrap().evidence();
        let Evidence::Source { source, locator } = original[0].clone() else {
            panic!("测试节点应该引用来源");
        };
        let Locator::TextRange { start, end } = locator;
        let other = Locator::TextRange {
            start,
            end: end - 1,
        };
        let cited = Evidence::Source {
            source: source.clone(),
            locator: other.clone(),
        };

        memory.cite("a", &source, &other, &sources()).unwrap();
        // 重复引用不会多出一条。
        memory.cite("a", &source, &other, &sources()).unwrap();
        assert_eq!(
            memory.node("a").unwrap().evidence(),
            [original[0].clone(), cited.clone()]
        );

        memory.uncite("a", &source, &locator).unwrap();
        assert_eq!(memory.node("a").unwrap().evidence(), [cited]);

        // 没有引用的范围、最后一条证据、不存在的节点和来源，都不能改动。
        let problem = |error: Error| match error {
            Error::InvalidMemory(problems) => problems[0].clone(),
            other => panic!("{other}"),
        };
        assert_eq!(
            problem(memory.uncite("a", &source, &locator).unwrap_err()),
            Problem::NotCited {
                id: "a".into(),
                cited: source.clone(),
                start,
                end,
            }
        );
        assert_eq!(
            problem(memory.uncite("a", &source, &other).unwrap_err()),
            Problem::MissingEvidence { id: "a".into() }
        );
        assert!(matches!(
            memory.uncite("幽灵", &source, &other),
            Err(Error::UnknownNode(_))
        ));
        let missing = source_id(b"not stored");
        assert!(matches!(
            memory.cite("a", &missing, &other, &sources()),
            Err(Error::InvalidMemory(_))
        ));
        assert_eq!(memory.node("a").unwrap().evidence().len(), 1);
    }

    #[test]
    fn catalog_operations_update_sections() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);
        memory.add_node(node("b", &[]), &[], &sources()).unwrap();

        memory.catalog_create("新板块", "描述").unwrap();
        memory.catalog_add("b", "新板块").unwrap();
        memory.catalog_add("b", "板块").unwrap();
        memory.catalog_describe("新板块", "新描述").unwrap();
        memory.catalog_rename("新板块", "改名").unwrap();
        memory.set_title("b", "新标题").unwrap();

        let sections = &memory.catalog().sections;
        assert_eq!(sections[0].nodes[1].id, "b");
        assert_eq!(sections[0].nodes[1].title, "新标题");
        assert_eq!(sections[1].name, "改名");
        assert_eq!(sections[1].description, "新描述");
        assert_eq!(sections[1].nodes[0].title, "新标题");

        memory.catalog_remove("b", "板块").unwrap();
        assert_eq!(memory.catalog().sections[0].nodes.len(), 1);
        let entries = memory.catalog_delete("改名").unwrap();
        assert_eq!(entries[0].id, "b");
        assert_eq!(memory.catalog().sections.len(), 1);
        // 节点本身还在，只是走不到了。
        assert_eq!(memory.unreachable(), ["b"]);
    }

    #[test]
    fn catalog_operations_report_mistakes() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);
        let problem = |error: Error| match error {
            Error::InvalidMemory(problems) => problems[0].clone(),
            other => panic!("{other}"),
        };

        assert_eq!(
            problem(memory.catalog_create("板块", "d").unwrap_err()),
            Problem::DuplicateSection {
                name: "板块".into()
            }
        );
        assert_eq!(
            problem(memory.catalog_add("a", "不存在").unwrap_err()),
            Problem::UnknownSection {
                name: "不存在".into()
            }
        );
        assert_eq!(
            problem(memory.catalog_add("a", "板块").unwrap_err()),
            Problem::AlreadyRepresentative {
                id: "a".into(),
                section: "板块".into()
            }
        );
        memory.catalog_create("空", "d").unwrap();
        assert_eq!(
            problem(memory.catalog_remove("a", "空").unwrap_err()),
            Problem::NotRepresentative {
                id: "a".into(),
                section: "空".into()
            }
        );
        assert!(matches!(
            memory.catalog_add("幽灵", "板块"),
            Err(Error::UnknownNode(_))
        ));
        assert_eq!(
            problem(memory.catalog_rename("空", "板块").unwrap_err()),
            Problem::DuplicateSection {
                name: "板块".into()
            }
        );
    }

    #[test]
    fn duplicate_section_names_fail_validation() {
        let mut duplicate = catalog(&["a"]);
        duplicate.sections.push(duplicate.sections[0].clone());
        let parsed = ParsedKnowledge {
            catalog: duplicate,
            nodes: vec![node("a", &[])],
        };

        let problems = Memory::validate(&parsed, &sources());

        assert_eq!(
            problems,
            [Problem::DuplicateSection {
                name: "板块".into()
            }]
        );
    }

    #[test]
    fn lists_grow_past_their_reserved_capacity() {
        let mut memory = memory_of(&["hub"], vec![node("hub", &[])]);

        for index in 0..100 {
            let mut new = node(&format!("n{index}"), &["hub"]);
            new.evidence.push(Evidence::Node { id: "hub".into() });
            memory
                .add_node(new, &linked_from(&["hub"]), &sources())
                .unwrap();
        }

        let hub = memory.node("hub").unwrap();
        assert_eq!(hub.related().len(), 100);
        assert_eq!(hub.related_by().len(), 100);
        assert_eq!(hub.based_by().len(), 100);
        assert_eq!(hub.related().last().unwrap().id(), "n99");
    }

    #[test]
    fn a_new_node_may_link_to_itself() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);

        memory
            .add_node(node("自己", &["自己", "a"]), &[], &sources())
            .unwrap();

        let itself = memory.node("自己").unwrap();
        assert_eq!(ids(itself.related()), ["自己", "a"]);
        assert_eq!(ids(itself.related_by()), ["自己"]);
    }

    #[test]
    fn rejected_nodes_leave_the_graph_unchanged() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);
        let missing_source = source_id(b"not stored");

        let cases = [
            (node("a", &[]), Vec::new(), "重复"),
            (node("../x", &[]), Vec::new(), "不合法"),
            (node("b", &["幽灵"]), Vec::new(), "关联的节点 幽灵 不存在"),
            (based_on("b", &["幽灵"]), Vec::new(), "证据节点 幽灵 不存在"),
            (based_on("b", &["b"]), Vec::new(), "证据节点 b 不存在"),
            (node_with_evidence("b", vec![]), Vec::new(), "没有任何证据"),
            (node("b", &[]), linked_from(&["幽灵"]), "节点 幽灵 不存在"),
            (
                node_with_evidence(
                    "b",
                    vec![Evidence::Source {
                        source: missing_source,
                        locator: Locator::TextRange { start: 0, end: 1 },
                    }],
                ),
                Vec::new(),
                "来源",
            ),
        ];
        for (new, related_by, expected) in cases {
            let message = memory
                .add_node(new, &related_by, &sources())
                .unwrap_err()
                .to_string();
            assert!(message.contains(expected), "{expected}：{message}");
        }

        assert_eq!(memory.node_count(), 1);
        assert_eq!(memory.catalog(), &catalog(&["a"]));
        assert_eq!(memory.node("a").unwrap().related().len(), 0);
    }

    #[test]
    fn remove_clears_every_address_and_reports_the_impact() {
        let mut x = based_on("x", &["依据"]);
        x.related = vec!["后继".into(), "x".into()];
        let mut memory = memory_of(
            &["入口", "x"],
            vec![
                node("入口", &["x", "依据", "后继"]),
                node("依据", &[]),
                node("后继", &[]),
                x,
            ],
        );

        let removed = memory.remove_node("x").unwrap();

        assert_eq!(removed.node.id, "x");
        assert_eq!(removed.related_by, ["入口"]);
        assert_eq!(removed.sections, ["板块"]);

        assert!(memory.node("x").is_none());
        assert_eq!(memory.node_count(), 3);
        assert_eq!(
            ids(memory.node("入口").unwrap().related()),
            ["依据", "后继"]
        );
        assert_eq!(memory.node("后继").unwrap().related_by().len(), 1);
        assert_eq!(memory.node("依据").unwrap().based_by().len(), 0);
        assert_eq!(memory.catalog(), &catalog(&["入口"]));
    }

    #[test]
    fn a_node_that_others_are_based_on_cannot_be_removed() {
        let mut conclusion = based_on("结论", &["依据"]);
        conclusion.evidence.push(source_evidence(0, 3));
        let mut memory = memory_of(
            &["依据", "结论"],
            vec![node("依据", &[]), conclusion.clone()],
        );

        let error = memory.remove_node("依据").unwrap_err();

        assert!(matches!(
            &error,
            Error::InvalidMemory(problems)
                if problems[..] == [Problem::StillBasedOn {
                    id: "依据".into(),
                    by: vec!["结论".into()],
                }]
        ));
        // 什么都没有变。
        assert_eq!(memory.node_count(), 2);
        assert_eq!(memory.node("结论").unwrap().to_node(), conclusion);

        // 模型先处理好依赖，就可以删除了。
        memory.unbase("结论", "依据").unwrap();
        memory.remove_node("依据").unwrap();
        assert_eq!(memory.node("结论").unwrap().based_on().count(), 0);
    }

    #[test]
    fn remove_reports_nodes_that_became_unreachable() {
        let mut memory = memory_of(
            &["a"],
            vec![
                node("a", &["桥"]),
                node("桥", &["c"]),
                node("c", &["d"]),
                node("d", &["c"]),
            ],
        );

        memory.remove_node("桥").unwrap();

        assert_eq!(memory.unreachable(), ["c", "d"]);
        // 模型可以把它们重新接回来。
        memory
            .add_node(node("新桥", &["c"]), &linked_from(&["a"]), &sources())
            .unwrap();
        assert!(memory.unreachable().is_empty());
    }

    #[test]
    fn removing_an_unknown_node_is_an_error() {
        let mut memory = memory_of(&["a"], vec![node("a", &[])]);

        assert!(matches!(
            memory.remove_node("不存在"),
            Err(Error::UnknownNode(_))
        ));
    }

    #[test]
    fn nodes_with_grown_lists_can_be_removed() {
        let mut memory = memory_of(&["hub"], vec![node("hub", &[])]);
        for index in 0..20 {
            let id = format!("n{index}");
            memory
                .add_node(node(&id, &["hub"]), &linked_from(&["hub"]), &sources())
                .unwrap();
        }

        let removed = memory.remove_node("hub").unwrap();

        assert_eq!(removed.related_by.len(), 20);
        for index in 0..20 {
            let node = memory.node(&format!("n{index}")).unwrap();
            assert_eq!(node.related().len(), 0);
            assert_eq!(node.related_by().len(), 0);
        }
    }

    /// 随机增删，每一步都核对所有关系都是对称的，并且没有地址指向已删除的节点。
    #[test]
    fn random_changes_keep_the_graph_consistent() {
        let mut memory = memory_of(&["根"], vec![node("根", &[])]);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        let mut next_id = 0;

        for _ in 0..3000 {
            let live: Vec<String> = memory.nodes().map(|n| n.id().to_owned()).collect();
            if next() % 100 < 55 || live.is_empty() {
                let pick = |roll: usize| -> Vec<String> {
                    if live.is_empty() {
                        return Vec::new();
                    }
                    (0..roll % 4)
                        .map(|i| live[(roll >> 3).wrapping_add(i * 7) % live.len()].clone())
                        .collect()
                };
                let id = format!("n{next_id}");
                next_id += 1;
                let mut new = node(&id, &[]);
                new.related = pick(next());
                new.evidence
                    .extend(pick(next()).into_iter().map(|id| Evidence::Node { id }));
                let related_by = pick(next());
                memory.add_node(new, &related_by, &sources()).unwrap();
            } else {
                let target = live[next() % live.len()].clone();
                let based_by = memory.node(&target).unwrap().based_by().len();
                let result = memory.remove_node(&target);
                assert_eq!(result.is_ok(), based_by == 0);
            }
            assert_consistent(&memory);
        }
    }

    fn assert_consistent(memory: &Memory) {
        let live: HashSet<NodeRef<'_>> = memory.nodes().collect();
        for node in memory.nodes() {
            assert_eq!(memory.node(node.id()), Some(node));
            for target in node.related() {
                assert!(live.contains(&target));
                assert!(target.related_by().any(|other| other == node));
            }
            for from in node.related_by() {
                assert!(live.contains(&from));
                assert!(from.related().any(|other| other == node));
            }
            for target in node.based_on() {
                assert!(live.contains(&target));
                assert!(target.based_by().any(|other| other == node));
            }
            for dependent in node.based_by() {
                assert!(live.contains(&dependent));
                assert!(dependent.based_on().any(|other| other == node));
            }
        }
        for entry in memory.catalog().sections.iter().flat_map(|s| &s.nodes) {
            assert!(memory.node(&entry.id).is_some());
        }
    }
}
