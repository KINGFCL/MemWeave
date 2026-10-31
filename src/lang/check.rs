//! 语义检查：在不修改记忆的前提下，模拟脚本的效果并找出所有问题。
//!
//! 通过检查后才开始执行，所以脚本写错时记忆完全不变。

use std::collections::{HashMap, HashSet};

use super::Mode;
use super::ast::{AddNode, Located, Name, Statement};
use super::diagnostic::{Diagnostic, Span, closest};
use crate::knowledge::{Evidence, Locator};
use crate::memory::{Memory, check_sources, is_valid_id};
use crate::source::SourceStore;

/// 提示“你是不是想写”时要逐个比较，节点太多就不提示了。
const MAX_SUGGESTION_SCAN: usize = 200_000;

pub fn check(
    statements: &[Located],
    memory: &Memory,
    sources: &dyn SourceStore,
    mode: Mode,
) -> Vec<Diagnostic> {
    let mut checker = Checker {
        memory,
        sources,
        added: HashSet::new(),
        removed: HashSet::new(),
        related: HashMap::new(),
        based_on: HashMap::new(),
        source_counts: HashMap::new(),
        removed_ever: HashSet::new(),
        errors: Vec::new(),
    };
    for located in statements {
        if mode == Mode::ReadOnly && located.statement.writes() {
            checker.errors.push(
                Diagnostic::new("E301", located.span, "当前会话是只读的，不能修改记忆")
                    .with_hint("只读会话只能用 catalog、show、links、evidence、check"),
            );
            continue;
        }
        checker.statement(&located.statement);
    }
    checker.errors
}

struct Checker<'a> {
    memory: &'a Memory,
    sources: &'a dyn SourceStore,
    /// 脚本前面的语句已经造成的变化。
    added: HashSet<String>,
    removed: HashSet<String>,
    related: HashMap<String, Vec<String>>,
    based_on: HashMap<String, Vec<String>>,
    source_counts: HashMap<String, usize>,
    /// 本脚本删除过的节点，内存里指向它们的关联在执行时已经不在了。
    removed_ever: HashSet<String>,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Catalog | Statement::Check => {}
            Statement::Show(id) | Statement::Evidence(id) => {
                self.require(id);
            }
            Statement::Links { id, .. } => {
                self.require(id);
            }
            Statement::Add(add) => self.add(add),
            Statement::Remove(id) => {
                if self.require(id) {
                    self.removed.insert(id.text.clone());
                    self.added.remove(&id.text);
                    // 和执行时一样：别的节点指向它的关联没了，证据里的 id 还留着。
                    self.removed_ever.insert(id.text.clone());
                    for list in self.related.values_mut() {
                        list.retain(|other| *other != id.text);
                    }
                    self.related.remove(&id.text);
                    self.based_on.remove(&id.text);
                    self.source_counts.remove(&id.text);
                }
            }
            Statement::Link { from, to } => self.link(from, to),
            Statement::Unlink { from, to } => self.unlink(from, to),
            Statement::Set {
                id,
                value,
                value_span,
                ..
            } => {
                self.require(id);
                if value.trim().is_empty() {
                    self.errors
                        .push(Diagnostic::new("E206", *value_span, "内容不能为空"));
                }
            }
            Statement::Base { id, on } => self.base(id, on),
            Statement::Unbase { id, on } => self.unbase(id, on),
        }
    }

    fn add(&mut self, add: &AddNode) {
        let id = &add.id;
        if !is_valid_id(&id.text) {
            self.errors.push(
                Diagnostic::new("E204", id.span, format!("节点地址 {} 不合法", id.text))
                    .with_hint("只能包含字母、数字、- 和 _，且不超过 200 字节"),
            );
        }
        if self.exists(&id.text) {
            self.errors.push(
                Diagnostic::new("E205", id.span, format!("节点 {} 已经存在", id.text))
                    .with_hint("换一个地址，或用 set 修改已有节点"),
            );
        }
        for (value, clause) in [(&add.title, "title"), (&add.description, "desc")] {
            match value {
                None => self.errors.push(
                    Diagnostic::new("E206", id.span, format!("add {} 缺少 {clause}", id.text))
                        .with_hint("add 必须写 title 和 desc"),
                ),
                Some(text) if text.trim().is_empty() => self.errors.push(Diagnostic::new(
                    "E206",
                    id.span,
                    format!("add {} 的 {clause} 不能为空", id.text),
                )),
                Some(_) => {}
            }
        }
        if add.sources.is_empty() && add.based_on.is_empty() {
            self.errors.push(
                Diagnostic::new("E207", id.span, format!("add {} 没有任何证据", id.text))
                    .with_hint("至少写一条 source 或 based_on，没有依据的结论不能进入记忆"),
            );
        }
        if add.related_by.is_empty() && add.section.is_none() {
            self.errors.push(
                Diagnostic::new("E208", id.span, format!("从总表到达不了节点 {}", id.text))
                    .with_hint("写 related_by 指定谁关联它，或写 section 放进某个板块"),
            );
        }

        for source in &add.sources {
            let evidence = [Evidence::Source {
                source: source.source.text.clone(),
                locator: Locator::TextRange {
                    start: source.start,
                    end: source.end,
                },
            }];
            for problem in check_sources(&id.text, &evidence, self.sources) {
                self.errors.push(Diagnostic::new(
                    "E209",
                    source.source.span,
                    problem.to_string(),
                ));
            }
        }
        for reference in add.based_on.iter().chain(&add.related_by) {
            self.require(reference);
        }
        for reference in &add.related {
            if reference.text != id.text {
                self.require(reference);
            }
        }

        // 记下这个节点，后面的语句才能引用它。
        self.removed.remove(&id.text);
        self.added.insert(id.text.clone());
        self.related.insert(
            id.text.clone(),
            unique(add.related.iter().map(|n| n.text.clone())),
        );
        self.based_on.insert(
            id.text.clone(),
            unique(add.based_on.iter().map(|n| n.text.clone())),
        );
        self.source_counts
            .insert(id.text.clone(), add.sources.len());
        for from in &add.related_by {
            if self.exists(&from.text) {
                self.related_of(&from.text).push(id.text.clone());
            }
        }
    }

    fn link(&mut self, from: &Name, to: &Name) {
        let known = self.require(from) & self.require(to);
        if !known {
            return;
        }
        if self.related_of(&from.text).contains(&to.text) {
            self.errors.push(
                Diagnostic::new(
                    "E210",
                    to.span,
                    format!("{} -> {} 已经存在", from.text, to.text),
                )
                .with_hint("不需要重复添加"),
            );
            return;
        }
        self.related_of(&from.text).push(to.text.clone());
    }

    fn unlink(&mut self, from: &Name, to: &Name) {
        let known = self.require(from) & self.require(to);
        if !known {
            return;
        }
        let list = self.related_of(&from.text);
        if !list.contains(&to.text) {
            self.errors.push(
                Diagnostic::new(
                    "E211",
                    to.span,
                    format!("{} -> {} 不存在", from.text, to.text),
                )
                .with_hint(format!("用 show {} 查看它现有的关联", from.text)),
            );
            return;
        }
        list.retain(|id| *id != to.text);
    }

    fn base(&mut self, id: &Name, on: &Name) {
        let known = self.require(id) & self.require(on);
        if !known {
            return;
        }
        if self.based_on_of(&id.text).contains(&on.text) {
            self.errors.push(Diagnostic::new(
                "E210",
                on.span,
                format!("{} 已经依据 {}", id.text, on.text),
            ));
            return;
        }
        if id.text == on.text || self.depends_on(&on.text, &id.text) {
            self.errors.push(
                Diagnostic::new(
                    "E212",
                    on.span,
                    format!("{} 依据 {} 会形成循环", id.text, on.text),
                )
                .with_hint("结论必须能追溯到原始数据，不能互相依据"),
            );
            return;
        }
        self.based_on_of(&id.text).push(on.text.clone());
    }

    fn unbase(&mut self, id: &Name, on: &Name) {
        if !self.require(id) {
            return;
        }
        let sources = self.source_count_of(&id.text);
        let list = self.based_on_of(&id.text);
        if !list.contains(&on.text) {
            self.errors.push(
                Diagnostic::new("E211", on.span, format!("{} 没有依据 {}", id.text, on.text))
                    .with_hint(format!("用 show {} 查看它现有的依据", id.text)),
            );
            return;
        }
        if sources == 0 && list.iter().all(|other| *other == on.text) {
            self.errors.push(
                Diagnostic::new(
                    "E207",
                    on.span,
                    format!("去掉后 {} 就没有任何证据了", id.text),
                )
                .with_hint("先用 base 加上新的依据，或者用 remove 删除这个结论"),
            );
            return;
        }
        list.retain(|other| *other != on.text);
    }

    /// `node` 是否直接或间接依据 `target`。
    fn depends_on(&mut self, node: &str, target: &str) -> bool {
        let mut seen = HashSet::new();
        let mut pending = vec![node.to_owned()];
        while let Some(current) = pending.pop() {
            if current == target {
                return true;
            }
            if seen.insert(current.clone()) && self.exists(&current) {
                pending.extend(self.based_on_of(&current).iter().cloned());
            }
        }
        false
    }

    fn exists(&self, id: &str) -> bool {
        self.added.contains(id) || (!self.removed.contains(id) && self.memory.contains(id))
    }

    fn require(&mut self, name: &Name) -> bool {
        if self.exists(&name.text) {
            return true;
        }
        let mut error = Diagnostic::new("E201", name.span, format!("节点 {} 不存在", name.text));
        if self.removed.contains(&name.text) {
            error = error.with_hint("这个节点已被本脚本前面的 remove 删除");
        } else if let Some(hint) = self.suggestion(&name.text) {
            error = error.with_hint(hint);
        }
        self.errors.push(error);
        false
    }

    fn suggestion(&self, id: &str) -> Option<String> {
        if self.memory.node_count() > MAX_SUGGESTION_SCAN {
            return None;
        }
        let nodes: Vec<&str> = self.memory.nodes().map(|node| node.id()).collect();
        let candidates = nodes
            .into_iter()
            .chain(self.added.iter().map(String::as_str));
        closest(
            id,
            candidates.filter(|candidate| !self.removed.contains(*candidate)),
        )
        .map(|found| format!("你是不是想写 {found}？"))
    }

    fn related_of(&mut self, id: &str) -> &mut Vec<String> {
        let memory = self.memory;
        let removed_ever = &self.removed_ever;
        self.related.entry(id.to_owned()).or_insert_with(|| {
            memory.node(id).map_or_else(Vec::new, |node| {
                node.related()
                    .map(|n| n.id().to_owned())
                    .filter(|other| !removed_ever.contains(other))
                    .collect()
            })
        })
    }

    fn based_on_of(&mut self, id: &str) -> &mut Vec<String> {
        let memory = self.memory;
        self.based_on.entry(id.to_owned()).or_insert_with(|| {
            memory.node(id).map_or_else(Vec::new, |node| {
                node.evidence()
                    .into_iter()
                    .filter_map(|evidence| match evidence {
                        Evidence::Node { id } => Some(id),
                        Evidence::Source { .. } => None,
                    })
                    .collect()
            })
        })
    }

    fn source_count_of(&mut self, id: &str) -> usize {
        let memory = self.memory;
        *self.source_counts.entry(id.to_owned()).or_insert_with(|| {
            memory.node(id).map_or(0, |node| {
                node.evidence()
                    .iter()
                    .filter(|evidence| matches!(evidence, Evidence::Source { .. }))
                    .count()
            })
        })
    }
}

fn unique(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.filter(|item| seen.insert(item.clone())).collect()
}

/// 脚本过长时的报错。
pub fn too_large(what: &str, limit: usize) -> Diagnostic {
    Diagnostic::new(
        "E302",
        Span {
            line: 1,
            column: 1,
            length: 1,
        },
        format!("脚本的{what}超过上限 {limit}"),
    )
    .with_hint("拆成多段脚本分别执行")
}
