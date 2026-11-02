//! 语义检查：在不修改记忆的前提下，模拟脚本的效果并找出所有问题。
//!
//! 通过检查后才开始执行，所以脚本写错时记忆完全不变。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::Mode;
use super::ast::{AddNode, Citation, Label, Located, Name, SourceRef, Statement, Version};
use super::diagnostic::{Diagnostic, Span, closest};
use crate::board::{self, BoardOp, Goal};
use crate::export::target_problem;
use crate::knowledge::{Evidence, Locator};
use crate::manual::{self, Manual};
use crate::memory::{Memory, check_sources, is_valid_id};
use crate::source::{SourceStore, source_id};

/// 提示“你是不是想写”时要逐个比较，节点太多就不提示了。
const MAX_SUGGESTION_SCAN: usize = 200_000;

pub fn check(
    statements: &[Located],
    memory: &Memory,
    sources: &dyn SourceStore,
    mode: Mode,
    manual: Option<&Manual>,
) -> Vec<Diagnostic> {
    let mut checker = Checker {
        memory,
        sources,
        manual,
        added: HashSet::new(),
        removed: HashSet::new(),
        related: HashMap::new(),
        based_on: HashMap::new(),
        cited: HashMap::new(),
        removed_ever: HashSet::new(),
        sections: memory
            .catalog()
            .sections
            .iter()
            .map(|section| {
                let nodes = section.nodes.iter().map(|entry| entry.id.clone()).collect();
                (section.name.clone(), nodes)
            })
            .collect(),
        new_nodes: Vec::new(),
        export_targets: HashSet::new(),
        goals: memory.catalog().goals.clone(),
        declared: statements
            .iter()
            .filter_map(|located| match &located.statement {
                Statement::Add(add) | Statement::Source(add) => Some(add.id.text.clone()),
                _ => None,
            })
            .collect(),
        errors: Vec::new(),
    };
    let last = statements.len().saturating_sub(1);
    for (position, located) in statements.iter().enumerate() {
        if located.statement.is_bare_read() {
            checker.errors.push(bare_read(located));
            continue;
        }
        if mode == Mode::ReadOnly && located.statement.needs_write_session() {
            checker.errors.push(
                Diagnostic::new(
                    "E301",
                    located.span,
                    "这个调用是只读的，不能修改记忆、保存或导出",
                )
                .with_hint(
                    "换可写的入口重新提交整段脚本：MCP 用 memory_write，命令行用 memweave run --write",
                ),
            );
            continue;
        }
        match located.statement {
            Statement::Save if position != last => {
                checker.errors.push(
                    Diagnostic::new("E225", located.span, "save 只能写在脚本的最后一行")
                        .with_hint("它保存的是这条语句之前的所有修改，后面的修改要再 save 一次"),
                );
            }
            Statement::Discard if statements.len() != 1 => {
                checker.errors.push(
                    Diagnostic::new(
                        "E226",
                        located.span,
                        "discard 要单独执行，不能和别的语句写在一起",
                    )
                    .with_hint("它会放弃所有未保存的修改。先单独执行 discard，再提交新的修改"),
                );
            }
            _ => {}
        }
        if checker.touches_manual(&located.statement) {
            continue;
        }
        if let Statement::Board(op) = &located.statement {
            checker.board(op, located.span);
            continue;
        }
        checker.statement(&located.statement);
    }
    // 重放时没有 save，这些脚本当时已经检查过了。
    if mode == Mode::ReadWrite
        && let Some(Located {
            statement: Statement::Save,
            span,
        }) = statements.last()
    {
        checker.require_new_nodes_reachable(*span);
    }
    checker.errors
}

/// 读语句没有写版本。
fn bare_read(located: &Located) -> Diagnostic {
    let example = match &located.statement {
        Statement::CatalogShow => "catalog show".to_owned(),
        Statement::Check => "check".to_owned(),
        Statement::Timeline { .. } => "timeline".to_owned(),
        Statement::Export(_) => "export \"路径\"".to_owned(),
        Statement::Show(id) => format!("show {}", id.text),
        Statement::Links { id, .. } => format!("links {}", id.text),
        Statement::Evidence(id) => format!("evidence {}", id.text),
        Statement::BasedBy(id) => format!("based_by {}", id.text),
        Statement::RelatedBy(id) => format!("related_by {}", id.text),
        Statement::FullSource { id, .. } => format!("full_source {}", id.text),
        _ => unreachable!("调用方只传读语句"),
    };
    Diagnostic::new("E224", located.span, "读语句要写明读哪个版本").with_hint(format!(
        "revision {example} 读当前草稿，origin {example} 读已保存的版本。没有未保存的修改时两者一样"
    ))
}

/// 一段被引用的原文：来源地址、起始和结束。
type Cited = (String, usize, usize);

fn cited_range(source: &SourceRef) -> Cited {
    (source.source.text.clone(), source.start, source.end)
}

fn cited_text(text: &str) -> Cited {
    (source_id(text.as_bytes()), 0, text.len())
}

struct Checker<'a> {
    memory: &'a Memory,
    sources: &'a dyn SourceStore,
    /// 内置说明。重放日志时没有它：那些脚本当时已经检查过了，旧知识库里可能有用这个前缀的节点。
    manual: Option<&'a Manual>,
    /// 脚本前面的语句已经造成的变化。
    added: HashSet<String>,
    removed: HashSet<String>,
    related: HashMap<String, Vec<String>>,
    based_on: HashMap<String, Vec<String>>,
    /// 每个节点引用的原文：来源地址和范围。
    cited: HashMap<String, Vec<Cited>>,
    /// 本脚本删除过的节点，内存里指向它们的关联在执行时已经不在了。
    removed_ever: HashSet<String>,
    /// 模拟到当前语句时的总表：板块的名字和代表节点。
    sections: Vec<(String, Vec<String>)>,
    /// 本脚本新建的节点，保存时报错用它们的位置。
    new_nodes: Vec<Name>,
    /// 本脚本已经导出过的路径。
    export_targets: HashSet<String>,
    /// 模拟到当前语句时的工作板。
    goals: Vec<Goal>,
    /// 这段脚本要新建的所有节点，报错时用来判断是不是写反了顺序。
    declared: HashSet<String>,
    errors: Vec<Diagnostic>,
}

impl Checker<'_> {
    /// 写语句碰到内置说明时报错。内置说明只能读，也不能和记忆相连，否则它们就分不开了。
    fn touches_manual(&mut self, statement: &Statement) -> bool {
        let Some(manual) = self.manual else {
            return false;
        };
        let before = self.errors.len();
        for name in statement.written_nodes() {
            let creates = matches!(statement, Statement::Add(add) | Statement::Source(add) if add.id == *name);
            if creates && name.text.starts_with(manual::RESERVED_PREFIX) {
                self.errors.push(
                    Diagnostic::new(
                        "E223",
                        name.span,
                        format!(
                            "{} 开头的地址留给内置说明，新节点不能用",
                            manual::RESERVED_PREFIX
                        ),
                    )
                    .with_hint("换一个能看出内容的地址"),
                );
            } else if manual.contains(&name.text) && !self.memory.contains(&name.text) {
                // 旧知识库里可能已经有同名节点，让模型能把它迁走。
                self.errors.push(
                    Diagnostic::new(
                        "E223",
                        name.span,
                        format!(
                            "{} 是内置说明，只能读，不能修改，也不能和记忆相连",
                            name.text
                        ),
                    )
                    .with_hint("用户的知识写成普通节点，放在自己的板块里"),
                );
            }
        }
        for section in statement.written_sections() {
            if section.text == manual::SECTION {
                self.errors.push(
                    Diagnostic::new(
                        "E223",
                        section.span,
                        format!("板块“{}”是内置的，只能读", section.text),
                    )
                    .with_hint("用户的知识放在自己的板块里，用 catalog create 新建"),
                );
            }
        }
        self.errors.len() > before
    }

    /// 读语句要的节点：内置说明也算。
    fn require_readable(&mut self, name: &Name) {
        if !self
            .manual
            .is_some_and(|manual| manual.contains(&name.text))
        {
            self.require(name);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Versioned(Version::Revision, inner) => self.statement(inner),
            Statement::Versioned(Version::Origin, inner) => self.origin_read(inner),
            Statement::RevisionDisplay { .. } | Statement::Save | Statement::Discard => {}
            Statement::CatalogShow | Statement::Check | Statement::Timeline { .. } => {}
            Statement::Board(_) => unreachable!("工作板语句在 check 里单独处理"),
            Statement::Log(text) => self.nonblank(text, "要记下的内容"),
            Statement::Export(path) => self.export(path),
            Statement::Cite { id, citation } => self.cite(id, citation),
            Statement::Uncite { id, source } => self.uncite(id, source),
            Statement::CatalogCreate { name, description } => {
                self.nonblank(name, "板块的名字");
                self.nonblank(description, "板块的描述");
                if self.section(&name.text).is_some() {
                    self.errors.push(
                        Diagnostic::new("E215", name.span, format!("板块“{}”已经存在", name.text))
                            .with_hint("换一个名字，或用 catalog describe 修改它的描述"),
                    );
                } else {
                    self.sections.push((name.text.clone(), Vec::new()));
                }
            }
            Statement::CatalogDescribe { name, description } => {
                self.require_section(name);
                self.nonblank(description, "板块的描述");
            }
            Statement::CatalogRename { name, new_name } => {
                self.nonblank(new_name, "板块的名字");
                let known = self.require_section(name);
                if self.section(&new_name.text).is_some() {
                    self.errors.push(Diagnostic::new(
                        "E215",
                        new_name.span,
                        format!("板块“{}”已经存在", new_name.text),
                    ));
                } else if let Some(index) = known {
                    self.sections[index].0.clone_from(&new_name.text);
                }
            }
            Statement::CatalogDelete(name) => {
                if let Some(index) = self.require_section(name) {
                    self.sections.remove(index);
                }
            }
            Statement::CatalogAdd { id, section } => {
                let known_node = self.require(id);
                if let Some(index) = self.require_section(section)
                    && known_node
                {
                    if self.sections[index].1.contains(&id.text) {
                        self.errors.push(Diagnostic::new(
                            "E210",
                            id.span,
                            format!("{} 已经是板块“{}”的代表", id.text, section.text),
                        ));
                    } else {
                        self.sections[index].1.push(id.text.clone());
                    }
                }
            }
            Statement::CatalogRemove { id, section } => {
                if let Some(index) = self.require_section(section) {
                    if let Some(position) =
                        self.sections[index].1.iter().position(|n| *n == id.text)
                    {
                        self.sections[index].1.remove(position);
                    } else {
                        self.errors.push(
                            Diagnostic::new(
                                "E211",
                                id.span,
                                format!("{} 不是板块“{}”的代表", id.text, section.text),
                            )
                            .with_hint("用 revision catalog show 查看现有的代表"),
                        );
                    }
                }
            }
            Statement::Show(id)
            | Statement::Evidence(id)
            | Statement::BasedBy(id)
            | Statement::RelatedBy(id)
            | Statement::FullSource { id, .. }
            | Statement::Links { id, .. } => self.require_readable(id),
            Statement::Add(add) => self.add(add, "add"),
            Statement::Source(add) => self.add(add, "source"),
            Statement::Remove(id) => {
                if self.require(id) && !self.is_based_on(id) {
                    self.removed.insert(id.text.clone());
                    self.added.remove(&id.text);
                    // 和执行时一样：别的节点指向它的关联没了。
                    self.removed_ever.insert(id.text.clone());
                    for list in self.related.values_mut() {
                        list.retain(|other| *other != id.text);
                    }
                    for (_, nodes) in &mut self.sections {
                        nodes.retain(|other| *other != id.text);
                    }
                    self.new_nodes.retain(|name| name.text != id.text);
                    self.related.remove(&id.text);
                    self.based_on.remove(&id.text);
                    self.cited.remove(&id.text);
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

    fn add(&mut self, add: &AddNode, keyword: &str) {
        let id = &add.id;
        let is_source = keyword == "source";
        if !is_valid_id(&id.text) {
            self.errors.push(
                Diagnostic::new("E204", id.span, format!("节点地址 {} 不合法", id.text))
                    .with_hint("只能包含字母、数字、- 和 _，且不超过 200 字节"),
            );
        }
        let duplicate = self.exists(&id.text);
        if duplicate {
            self.errors.push(
                Diagnostic::new("E205", id.span, format!("节点 {} 已经存在", id.text))
                    .with_hint("换一个地址，或用 set 修改已有节点"),
            );
        }
        for (value, clause) in [(&add.title, "title"), (&add.description, "desc")] {
            match value {
                None => self.errors.push(
                    Diagnostic::new(
                        "E206",
                        id.span,
                        format!("{keyword} {} 缺少 {clause}", id.text),
                    )
                    .with_hint(format!("{keyword} 必须写 title 和 desc")),
                ),
                Some(text) if text.trim().is_empty() => self.errors.push(Diagnostic::new(
                    "E206",
                    id.span,
                    format!("{keyword} {} 的 {clause} 不能为空", id.text),
                )),
                Some(_) => {}
            }
        }
        if add.text.as_ref().is_some_and(|text| text.trim().is_empty()) {
            self.errors.push(Diagnostic::new(
                "E206",
                id.span,
                format!("{keyword} {} 的 text 不能为空", id.text),
            ));
        }
        if is_source && add.text.is_none() && add.sources.is_empty() {
            self.errors.push(
                Diagnostic::new("E207", id.span, format!("source {} 没有原文", id.text))
                    .with_hint("用 text 写入新的原文，或用 source 子句引用已有的原文"),
            );
        }
        if !is_source && add.based_on.is_empty() {
            self.errors.push(
                Diagnostic::new("E207", id.span, format!("add {} 没有 based_on", id.text))
                    .with_hint(
                        "add 用于由别的节点推出的知识，必须写 based_on；只来自原文的知识请用 source 语句",
                    ),
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
            self.check_citation(&id.text, source, &evidence);
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
        if !duplicate {
            self.new_nodes.push(id.clone());
        }
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
        let mut cited: Vec<Cited> = add.sources.iter().map(cited_range).collect();
        cited.extend(add.text.as_deref().map(cited_text));
        cited.dedup();
        self.cited.insert(id.text.clone(), cited);
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
        let known = self.require(id) & self.require(on);
        if !known {
            return;
        }
        let sources = self.cited_of(&id.text).len();
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
                .with_hint("先用 base 或 cite 加上新的证据，或者用 remove 删除这个结论"),
            );
            return;
        }
        list.retain(|other| *other != on.text);
    }

    /// 和执行时调用同一个函数，所以结果一定一致。
    fn board(&mut self, op: &BoardOp, span: Span) {
        if let Err(error) = board::apply(&mut self.goals, op) {
            let mut diagnostic = Diagnostic::new(error.code, span, error.message);
            if let Some(hint) = error.hint {
                diagnostic = diagnostic.with_hint(hint);
            }
            self.errors.push(diagnostic);
        }
    }

    /// 来源和范围都有效，并且范围里是完整的文字。切在字符中间的范围现在拦不住，以后每次读都会失败。
    fn check_citation(&mut self, id: &str, source: &SourceRef, evidence: &[Evidence; 1]) -> bool {
        let problems = check_sources(id, evidence, self.sources);
        let span = source.source.span;
        if !problems.is_empty() {
            for problem in problems {
                self.errors
                    .push(Diagnostic::new("E209", span, problem.to_string()));
            }
            return false;
        }
        let locator = Locator::TextRange {
            start: source.start,
            end: source.end,
        };
        if let Err(error) = self.sources.read(&source.source.text, &locator) {
            self.errors
                .push(Diagnostic::new("E209", span, error.to_string()));
            return false;
        }
        true
    }

    fn cite(&mut self, id: &Name, citation: &Citation) {
        let known = self.require(id);
        let (cited, span) = match citation {
            Citation::Text(text) => {
                self.nonblank(text, "原文");
                if text.text.trim().is_empty() {
                    return;
                }
                (cited_text(&text.text), text.span)
            }
            Citation::Source(source) => {
                let evidence = [Evidence::Source {
                    source: source.source.text.clone(),
                    locator: Locator::TextRange {
                        start: source.start,
                        end: source.end,
                    },
                }];
                if !self.check_citation(&id.text, source, &evidence) {
                    return;
                }
                (cited_range(source), source.source.span)
            }
        };
        if !known {
            return;
        }
        if self.cited_of(&id.text).contains(&cited) {
            self.errors.push(Diagnostic::new(
                "E210",
                span,
                format!(
                    "{} 已经引用了 {} 的 {}..{}",
                    id.text, cited.0, cited.1, cited.2
                ),
            ));
            return;
        }
        self.cited_of(&id.text).push(cited);
    }

    fn uncite(&mut self, id: &Name, source: &SourceRef) {
        if !self.require(id) {
            return;
        }
        let cited = cited_range(source);
        let span = source.source.span;
        let no_nodes = self.based_on_of(&id.text).is_empty();
        let list = self.cited_of(&id.text);
        if !list.contains(&cited) {
            self.errors.push(
                Diagnostic::new(
                    "E211",
                    span,
                    format!(
                        "{} 没有引用 {} 的 {}..{}",
                        id.text, cited.0, cited.1, cited.2
                    ),
                )
                .with_hint(format!("用 show {} 查看它现有的原文", id.text)),
            );
            return;
        }
        if no_nodes && list.len() == 1 {
            self.errors.push(
                Diagnostic::new("E207", span, format!("去掉后 {} 就没有任何证据了", id.text))
                    .with_hint("先用 cite 或 base 加上新的证据，或者用 remove 删除这个节点"),
            );
            return;
        }
        list.retain(|other| *other != cited);
    }

    /// 保存时，草稿里所有新节点都必须接入图。草稿期间可以暂时没接上，那时候还不算数。
    fn require_new_nodes_reachable(&mut self, save: Span) {
        let new_nodes: HashSet<String> = self
            .memory
            .draft_new_nodes()
            .into_iter()
            .chain(self.added.iter().cloned())
            .filter(|id| self.exists(id))
            .collect();
        let representatives: HashSet<&str> = self
            .sections
            .iter()
            .flat_map(|(_, nodes)| nodes.iter().map(String::as_str))
            .collect();
        let mut waiting: Vec<String> = new_nodes
            .iter()
            .filter(|id| !self.has_way_in(id, &new_nodes, &representatives))
            .cloned()
            .collect();
        waiting.sort();
        for id in waiting {
            // 这段脚本里建的就指向那一行，之前的脚本里建的只能指向 save。
            let span = self
                .new_nodes
                .iter()
                .find(|name| name.text == id)
                .map_or(save, |name| name.span);
            self.errors.push(
                Diagnostic::new("E208", span, format!("从总表到达不了节点 {id}，不能保存")).with_hint(
                    format!(
                        "保存前接入它：link 某节点 -> {id}，或 catalog add {id} to \"板块\"。不要了就 remove {id}"
                    ),
                ),
            );
        }
    }

    /// 这个新节点有没有接入：它是代表，或者有已有节点关联它，或者有已接入的新节点关联它。
    /// 只在新节点之间走，不走完全图，所以保存的时间和图的大小无关。已有节点本身是否可达由 `check`
    /// 负责，新节点只是互相关联不算接入。
    fn has_way_in(
        &self,
        id: &str,
        new_nodes: &HashSet<String>,
        representatives: &HashSet<&str>,
    ) -> bool {
        let mut seen = HashSet::from([id.to_owned()]);
        let mut pending = vec![id.to_owned()];
        while let Some(current) = pending.pop() {
            if representatives.contains(current.as_str()) {
                return true;
            }
            for from in self.pointing_at(&current) {
                if !new_nodes.contains(&from) {
                    return true;
                }
                if seen.insert(from.clone()) {
                    pending.push(from);
                }
            }
        }
        false
    }

    /// 模拟到当前语句时，哪些节点关联了 `id`。
    fn pointing_at(&self, id: &str) -> Vec<String> {
        // 脚本前面的语句改过的关联，以模拟的结果为准。
        let mut from: Vec<String> = self
            .related
            .iter()
            .filter(|(other, list)| self.exists(other) && list.iter().any(|to| to == id))
            .map(|(other, _)| other.clone())
            .collect();
        // 本脚本删除过的节点，内存里的关联已经不算数了。
        if !self.removed_ever.contains(id)
            && let Some(node) = self.memory.node(id)
        {
            from.extend(
                node.related_by()
                    .map(|other| other.id())
                    .filter(|other| {
                        self.exists(other)
                            && !self.removed_ever.contains(*other)
                            && !self.related.contains_key(*other)
                    })
                    .map(str::to_owned),
            );
        }
        from
    }

    /// 读已保存的版本。脚本前面的修改和草稿都不影响它。
    fn origin_read(&mut self, statement: &Statement) {
        match statement {
            Statement::Show(id)
            | Statement::Evidence(id)
            | Statement::BasedBy(id)
            | Statement::RelatedBy(id)
            | Statement::FullSource { id, .. }
            | Statement::Links { id, .. } => {
                let builtin = self.manual.is_some_and(|manual| manual.contains(&id.text));
                if builtin || self.memory.in_origin(&id.text) {
                    return;
                }
                let mut error = Diagnostic::new(
                    "E201",
                    id.span,
                    format!("已保存的版本里没有节点 {}", id.text),
                );
                if self.exists(&id.text) || self.declared.contains(&id.text) {
                    error = error.with_hint(format!(
                        "{0} 是草稿里新建的，还没有保存。用 revision show {0} 读它",
                        id.text
                    ));
                }
                self.errors.push(error);
            }
            // 走不走得到要到执行时才知道，走不到时报 E902。
            Statement::Export(path) => self.export_target(path),
            _ => {}
        }
    }

    /// 导出的是这条语句执行时的状态，所以用模拟到这里的总表和关联判断。
    fn export(&mut self, path: &Label) {
        self.export_target(path);
        let lost = self.unreachable();
        if !lost.is_empty() {
            self.errors.push(
                Diagnostic::new(
                    "E217",
                    path.span,
                    format!("有 {} 个节点从总表到达不了，不能导出", lost.len()),
                )
                .with_hint(format!(
                    "先用 catalog add 或 link 接入，或者 remove 删除：{}",
                    lost.join(", ")
                )),
            );
        }
    }

    fn export_target(&mut self, path: &Label) {
        self.nonblank(path, "导出路径");
        let hint = "换一个不存在的目录，或者空目录";
        if !self.export_targets.insert(path.text.clone()) {
            self.errors.push(
                Diagnostic::new(
                    "E216",
                    path.span,
                    format!("这段脚本前面已经导出到 {}", path.text),
                )
                .with_hint(hint),
            );
        } else if let Some(problem) = target_problem(Path::new(&path.text)) {
            self.errors
                .push(Diagnostic::new("E216", path.span, problem).with_hint(hint));
        }
    }

    /// 模拟到当前语句时，从总表走不到的节点，和执行后 [`Memory::unreachable`] 的结果一致。
    fn unreachable(&self) -> Vec<String> {
        let mut reachable = HashSet::new();
        let mut pending: Vec<String> = self
            .sections
            .iter()
            .flat_map(|(_, nodes)| nodes.iter().cloned())
            .collect();
        while let Some(id) = pending.pop() {
            if !self.exists(&id) || !reachable.insert(id.clone()) {
                continue;
            }
            if let Some(list) = self.related.get(&id) {
                pending.extend(list.iter().cloned());
            } else if let Some(node) = self.memory.node(&id) {
                pending.extend(
                    node.related()
                        .map(|other| other.id().to_owned())
                        .filter(|other| !self.removed_ever.contains(other)),
                );
            }
        }
        let mut lost: Vec<String> = self
            .memory
            .nodes()
            .map(|node| node.id().to_owned())
            .chain(self.added.iter().cloned())
            .filter(|id| self.exists(id) && !reachable.contains(id))
            .collect();
        lost.sort();
        lost.dedup();
        lost
    }

    fn section(&self, name: &str) -> Option<usize> {
        self.sections
            .iter()
            .position(|(section, _)| section == name)
    }

    fn require_section(&mut self, name: &Label) -> Option<usize> {
        let index = self.section(&name.text);
        if index.is_none() {
            let mut error =
                Diagnostic::new("E214", name.span, format!("板块“{}”不存在", name.text));
            let names: Vec<&str> = self.sections.iter().map(|(n, _)| n.as_str()).collect();
            error = match closest(&name.text, names.into_iter()) {
                Some(found) => error.with_hint(format!("你是不是想写“{found}”？")),
                None => error.with_hint(format!(
                    "先用 catalog create \"{}\" desc \"\"\"描述\"\"\" 新建",
                    name.text
                )),
            };
            self.errors.push(error);
        }
        index
    }

    fn nonblank(&mut self, value: &Label, what: &str) {
        if value.text.trim().is_empty() {
            self.errors.push(Diagnostic::new(
                "E206",
                value.span,
                format!("{what}不能为空"),
            ));
        }
    }

    /// 还有结论依据这个节点时不能删除，推导关系必须永远指向真实存在的节点。
    fn is_based_on(&mut self, id: &Name) -> bool {
        let mut candidates: Vec<String> = self.based_on.keys().cloned().collect();
        if let Some(node) = self.memory.node(&id.text) {
            candidates.extend(node.based_by().map(|other| other.id().to_owned()));
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.retain(|candidate| {
            self.exists(candidate) && self.based_on_of(candidate).contains(&id.text)
        });
        if candidates.is_empty() {
            return false;
        }
        self.errors.push(
            Diagnostic::new(
                "E213",
                id.span,
                format!("不能删除 {}，{} 还依据它", id.text, candidates.join("、")),
            )
            .with_hint(format!(
                "先用 base 和 unbase 给它们换依据，或者先删除它们，比如 unbase {} on {}",
                candidates[0], id.text
            )),
        );
        true
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
        } else if self.declared.contains(&name.text) {
            error = error.with_hint(format!(
                "{0} 在这段脚本后面才新建。把它写在前面；要互相指向，就都建好之后再 link",
                name.text
            ));
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
        let builtin: Vec<&str> = self
            .manual
            .iter()
            .flat_map(|manual| manual.memory().nodes().map(|node| node.id()))
            .collect();
        let candidates = self
            .memory
            .nodes()
            .map(|node| node.id())
            .chain(builtin)
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

    fn cited_of(&mut self, id: &str) -> &mut Vec<Cited> {
        let memory = self.memory;
        self.cited.entry(id.to_owned()).or_insert_with(|| {
            memory.node(id).map_or_else(Vec::new, |node| {
                let mut cited = Vec::new();
                for evidence in node.evidence() {
                    if let Evidence::Source {
                        source,
                        locator: Locator::TextRange { start, end },
                    } = evidence
                    {
                        let entry = (source, start, end);
                        if !cited.contains(&entry) {
                            cited.push(entry);
                        }
                    }
                }
                cited
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
