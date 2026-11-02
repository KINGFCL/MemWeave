//! 执行通过检查的语句，并把结果渲染成文本。
//!
//! 记忆里的内容一律用 JSON 字符串输出：有明确的边界，内容里就算写着指令，也只是数据。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write;
use std::path::Path;

use super::ast::{AddNode, Citation, Field, Located, Statement, Version};
use crate::board::{Goal, StepStatus};
use crate::error::Error;
use crate::export::Exporter;
use crate::export::json::JsonExporter;
use crate::knowledge::{Catalog, Evidence, Locator, Node, SourceId};
use crate::manual::{self, Manual};
use crate::memory::{Memory, NodeChange, NodeRef, RemovedNode};
use crate::source::{SourceStore, source_id};
use crate::timeline::{Action, Event, NodeTimes, format_time};

/// 原文读不到时模型该告诉用户什么。模型不一定读过说明，所以写在报错里。
const DOCTOR_HINT: &str = "。告诉用户可以运行 memweave doctor，不要改记忆来掩盖";

/// 板块的代表超过这个数量时提醒模型：总表不是索引。
const MANY_REPRESENTATIVES: usize = 5;

/// 执行结果。
pub struct Report {
    pub text: String,
    /// 读取或导出失败的语句数。它们不影响后面的语句，这样实际效果才和重放日志一致。
    pub failures: usize,
    /// 每条语句的输出在 `text` 里结束的位置，输出被截断时用来说明停在了哪里。
    pub ends: Vec<usize>,
}

/// 依次执行语句。`Err` 里的 usize 是已经执行完的语句数，只有内存不足这类意外才会走到那里。
pub fn execute(
    statements: &[Located],
    memory: &mut Memory,
    sources: &dyn SourceStore,
    manual: Option<&Manual>,
    at: Option<u64>,
) -> Result<Report, (usize, String, Error)> {
    let mut report = Report {
        text: String::new(),
        failures: 0,
        ends: Vec::with_capacity(statements.len()),
    };
    for (done, located) in statements.iter().enumerate() {
        if let Err(error) = statement(&located.statement, memory, sources, manual, &mut report) {
            return Err((done, report.text, error));
        }
        if let Some(event) = event_for(&located.statement, at) {
            memory.record(event);
        }
        report.ends.push(report.text.len());
    }
    Ok(report)
}

fn statement(
    statement: &Statement,
    memory: &mut Memory,
    sources: &dyn SourceStore,
    manual: Option<&Manual>,
    report: &mut Report,
) -> Result<(), Error> {
    match statement {
        Statement::Versioned(Version::Revision, inner) => {
            if **inner == Statement::CatalogShow
                && let Some(note) = revision_note(memory)
            {
                let _ = writeln!(report.text, "{note}");
            }
            read(inner, memory, sources, manual, report)?;
            if matches!(**inner, Statement::Export(_)) && memory.has_revision() {
                report
                    .text
                    .push_str("warning: 导出的是未保存的草稿。之后 discard 不会删除这个目录\n");
            }
            return Ok(());
        }
        Statement::Versioned(Version::Origin, inner) => {
            return memory.with_origin(|origin| read(inner, origin, sources, manual, report));
        }
        Statement::RevisionDisplay(node) => {
            render_revision(
                memory,
                node.as_ref().map(|n| n.text.as_str()),
                &mut report.text,
            );
            return Ok(());
        }
        _ => {}
    }
    let output = &mut report.text;
    match statement {
        Statement::Save | Statement::Discard => unreachable!("这两条由 Session 处理"),
        Statement::Versioned(..) | Statement::RevisionDisplay(_) => {
            unreachable!("读语句在上面处理")
        }
        Statement::CatalogShow
        | Statement::Check
        | Statement::Timeline { .. }
        | Statement::Export(_)
        | Statement::Show(_)
        | Statement::Links { .. }
        | Statement::Evidence(_)
        | Statement::BasedBy(_)
        | Statement::RelatedBy(_)
        | Statement::FullSource { .. } => {
            unreachable!("不带版本的读语句过不了检查，重放时也跳过了")
        }
        Statement::Board(op) => {
            let text = memory
                .apply_board(op)
                .map_err(|error| Error::Board(error.message))?;
            let _ = writeln!(output, "{text}");
        }
        Statement::Log(text) => {
            let _ = writeln!(output, "logged {}", quote(&text.text));
        }
        Statement::CatalogCreate { name, description } => {
            memory.catalog_create(&name.text, &description.text)?;
            let _ = writeln!(output, "created section {}", quote(&name.text));
        }
        Statement::CatalogDescribe { name, description } => {
            memory.catalog_describe(&name.text, &description.text)?;
            let _ = writeln!(output, "described section {}", quote(&name.text));
        }
        Statement::CatalogRename { name, new_name } => {
            memory.catalog_rename(&name.text, &new_name.text)?;
            let _ = writeln!(
                output,
                "renamed section {} to {}",
                quote(&name.text),
                quote(&new_name.text)
            );
        }
        Statement::CatalogDelete(name) => {
            let entries = memory.catalog_delete(&name.text)?;
            let _ = writeln!(output, "deleted section {}", quote(&name.text));
            if !entries.is_empty() {
                let ids: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
                let _ = writeln!(
                    output,
                    "warning: {} 不再是这个板块的代表，节点本身保留",
                    ids.join(", ")
                );
                let _ = writeln!(output, "note: 可以用 revision check 确认是否有节点查不到了");
            }
        }
        Statement::CatalogAdd { id, section } => {
            memory.catalog_add(&id.text, &section.text)?;
            let _ = writeln!(
                output,
                "added {} to section {}",
                id.text,
                quote(&section.text)
            );
            let count = memory
                .catalog()
                .sections
                .iter()
                .find(|s| s.name == section.text)
                .map_or(0, |s| s.nodes.len());
            if count > MANY_REPRESENTATIVES {
                let _ = writeln!(
                    output,
                    "warning: 板块 {} 有 {count} 个代表节点。总表不是索引，每个领域只需要几个代表，其他节点靠 related 到达",
                    quote(&section.text)
                );
            }
        }
        Statement::CatalogRemove { id, section } => {
            memory.catalog_remove(&id.text, &section.text)?;
            let _ = writeln!(
                output,
                "removed {} from section {}",
                id.text,
                quote(&section.text)
            );
            let _ = writeln!(output, "note: 可以用 revision check 确认是否有节点查不到了");
        }
        Statement::Add(add) | Statement::Source(add) => {
            let (node, related_by) = build_node(add);
            memory.add_node(node, &related_by, sources)?;
            let _ = writeln!(output, "added {}", add.id.text);
            if let Some(text) = &add.text {
                let source = source_id(text.as_bytes());
                let _ = writeln!(output, "  source: {source} 0..{}", text.len());
            }
        }
        Statement::Remove(id) => {
            let removed = memory.remove_node(&id.text)?;
            render_removed(&removed, output);
        }
        Statement::Link { from, to } => {
            memory.link(&from.text, &to.text)?;
            let _ = writeln!(output, "linked {} -> {}", from.text, to.text);
        }
        Statement::Unlink { from, to } => {
            memory.unlink(&from.text, &to.text)?;
            let _ = writeln!(output, "unlinked {} -> {}", from.text, to.text);
            let _ = writeln!(
                output,
                "note: 关联减少后可能有节点查不到了，可以用 revision check 确认"
            );
        }
        Statement::Set {
            id, field, value, ..
        } => {
            match field {
                Field::Title => memory.set_title(&id.text, value)?,
                Field::Description => memory.set_description(&id.text, value)?,
            }
            let _ = writeln!(output, "updated {}", id.text);
            let node = existing(memory, &id.text)?;
            if *field == Field::Description {
                render_review(node, "内容", output);
            }
        }
        Statement::Cite { id, citation } => {
            let (source, start, end) = match citation {
                Citation::Text(text) => (source_id(text.text.as_bytes()), 0, text.text.len()),
                Citation::Source(source) => (source.source.text.clone(), source.start, source.end),
            };
            let locator = Locator::TextRange { start, end };
            memory.cite(&id.text, &source, &locator, sources)?;
            let _ = writeln!(output, "cited {}", id.text);
            if matches!(citation, Citation::Text(_)) {
                let _ = writeln!(output, "  source: {source} {start}..{end}");
            }
            render_review(existing(memory, &id.text)?, "证据", output);
        }
        Statement::Uncite { id, source } => {
            let locator = Locator::TextRange {
                start: source.start,
                end: source.end,
            };
            memory.uncite(&id.text, &source.source.text, &locator)?;
            let _ = writeln!(output, "uncited {}", id.text);
            render_review(existing(memory, &id.text)?, "证据", output);
        }
        Statement::Base { id, on } => {
            memory.base(&id.text, &on.text)?;
            let _ = writeln!(output, "based {} on {}", id.text, on.text);
        }
        Statement::Unbase { id, on } => {
            memory.unbase(&id.text, &on.text)?;
            let _ = writeln!(output, "unbased {} on {}", id.text, on.text);
        }
    }
    Ok(())
}

/// 执行一条读语句。只读，所以可以在任何一个版本上执行。
fn read(
    statement: &Statement,
    memory: &Memory,
    sources: &dyn SourceStore,
    manual: Option<&Manual>,
    report: &mut Report,
) -> Result<(), Error> {
    if let Some(id) = node_read_by(statement) {
        // 内置说明是另一张图，读的时候换过去。两张图之间没有边，所以不会走串。
        return match manual.filter(|manual| manual.contains(id)) {
            Some(manual) => read_node(statement, manual.memory(), manual.sources(), report),
            None => read_node(statement, memory, sources, report),
        };
    }
    let output = &mut report.text;
    match statement {
        Statement::CatalogShow => output.push_str(&render_overview(memory, manual)),
        Statement::Check => render_check(memory, sources, manual, output),
        Statement::Timeline {
            limit,
            offset,
            node,
        } => render_timeline(
            memory,
            *limit,
            *offset,
            node.as_ref().map(|n| n.text.as_str()),
            output,
        ),
        Statement::Export(path) => {
            match JsonExporter.export(memory, sources, Path::new(&path.text)) {
                Ok(exported) => {
                    let _ = writeln!(
                        output,
                        "exported {} nodes, {} sections, {} sources to {}",
                        exported.nodes,
                        exported.sections,
                        exported.sources,
                        quote(&path.text)
                    );
                }
                Err(error) => {
                    report.failures += 1;
                    let _ = writeln!(output, "error[E902] 导出失败，目标目录没有变化：{error}");
                }
            }
        }
        _ => unreachable!("Versioned 里只有读语句"),
    }
    Ok(())
}

/// 提醒还有没保存的修改。没有时返回 `None`。
pub fn revision_note(memory: &Memory) -> Option<String> {
    memory.has_revision().then(|| {
        format!(
            "revision: {} 段脚本未保存。save 保存，discard 放弃，revision display 看差异",
            memory.draft_scripts().len()
        )
    })
}

/// 这条语句要读哪个节点。
fn node_read_by(statement: &Statement) -> Option<&str> {
    match statement {
        Statement::Show(id)
        | Statement::Links { id, .. }
        | Statement::Evidence(id)
        | Statement::BasedBy(id)
        | Statement::RelatedBy(id)
        | Statement::FullSource { id, .. } => Some(&id.text),
        _ => None,
    }
}

/// 读节点的语句。只读，所以可以在记忆或内置说明上执行。
fn read_node(
    statement: &Statement,
    memory: &Memory,
    sources: &dyn SourceStore,
    report: &mut Report,
) -> Result<(), Error> {
    let output = &mut report.text;
    match statement {
        Statement::Show(id) => {
            let node = existing(memory, &id.text)?;
            let times = memory.node_times(&id.text);
            render_node(node, times, sources, output, &mut report.failures);
        }
        Statement::Links {
            id,
            depth,
            limit,
            offset,
        } => {
            render_links(existing(memory, &id.text)?, *depth, *limit, *offset, output);
        }
        Statement::Evidence(id) => {
            render_evidence(
                existing(memory, &id.text)?,
                memory,
                sources,
                output,
                &mut report.failures,
            );
        }
        Statement::BasedBy(id) => {
            let ids: Vec<String> = existing(memory, &id.text)?
                .based_by()
                .map(|node| node.id().to_owned())
                .collect();
            let _ = writeln!(output, "based_by {}: {}", id.text, join_ids(&ids));
        }
        Statement::RelatedBy(id) => {
            let ids: Vec<String> = existing(memory, &id.text)?
                .related_by()
                .map(|node| node.id().to_owned())
                .collect();
            let _ = writeln!(output, "related_by {}: {}", id.text, join_ids(&ids));
        }
        Statement::FullSource { id, offset, limit } => {
            let node = existing(memory, &id.text)?;
            render_full_source(node, *offset, *limit, sources, output, &mut report.failures);
        }
        _ => unreachable!("node_read_by 只放行读节点的语句"),
    }
    Ok(())
}

fn existing<'m>(memory: &'m Memory, id: &str) -> Result<NodeRef<'m>, Error> {
    memory
        .node(id)
        .ok_or_else(|| Error::UnknownNode(id.to_owned()))
}

fn build_node(add: &AddNode) -> (Node, Vec<String>) {
    let mut evidence: Vec<Evidence> = add
        .sources
        .iter()
        .map(|source| Evidence::Source {
            source: source.source.text.clone(),
            locator: Locator::TextRange {
                start: source.start,
                end: source.end,
            },
        })
        .collect();
    if let Some(text) = &add.text {
        evidence.push(Evidence::Source {
            source: source_id(text.as_bytes()),
            locator: Locator::TextRange {
                start: 0,
                end: text.len(),
            },
        });
    }
    evidence.extend(add.based_on.iter().map(|name| Evidence::Node {
        id: name.text.clone(),
    }));
    let node = Node {
        id: add.id.text.clone(),
        title: add.title.clone().unwrap_or_default(),
        description: add.description.clone().unwrap_or_default(),
        evidence,
        related: add.related.iter().map(|name| name.text.clone()).collect(),
    };
    let related_by = add
        .related_by
        .iter()
        .map(|name| name.text.clone())
        .collect();
    (node, related_by)
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("字符串总是可以序列化")
}

/// 工作板上最近行为显示几条。更多的用 `timeline` 看。
const RECENT_EVENTS: usize = 5;
/// 最近行为里每条最多显示多少字，完整内容用 `timeline` 看。
const RECENT_EVENT_CHARS: usize = 80;

/// 工作板、最近行为和总表：模型一眼就知道在做什么、做过什么、记忆里有什么。
pub fn render_overview(memory: &Memory, manual: Option<&Manual>) -> String {
    let mut output = String::new();
    let goals = &memory.catalog().goals;
    let recent = memory.timeline().iter().rev().take(RECENT_EVENTS);
    if !goals.is_empty() {
        output.push_str("工作板\n");
        for goal in goals {
            render_goal(goal, &mut output);
        }
    }
    if recent.len() > 0 {
        output.push_str("最近行为\n");
        for event in recent {
            let text: String = event.text.chars().take(RECENT_EVENT_CHARS).collect();
            let more = if event.text.chars().count() > RECENT_EVENT_CHARS {
                "…"
            } else {
                ""
            };
            let _ = writeln!(
                output,
                "  {} {}{more}",
                format_time(event.at),
                one_line(&text)
            );
        }
    }
    if !output.is_empty() {
        output.push_str("总表\n");
    }
    // 内置板块放在最前面：总表太长被截断时，模型仍然知道说明在哪。
    if let Some(manual) = manual {
        render_sections(manual.memory(), &mut output);
    }
    render_catalog(memory, &mut output);
    output
}

fn render_goal(goal: &Goal, output: &mut String) {
    let _ = writeln!(output, "goal {} {}", goal.id, quote(&goal.title));
    let _ = writeln!(output, "  desc: {}", quote(&goal.description));
    if goal.steps.is_empty() {
        output.push_str("  还没有步骤\n");
    }
    for step in &goal.steps {
        let mark = match step.status {
            StepStatus::Pending => "[ ]",
            StepStatus::Doing => "[>]",
            StepStatus::Done => "[x]",
            StepStatus::Blocked => "[!]",
        };
        let _ = write!(output, "  {mark} {} {}", step.number, quote(&step.text));
        if let Some(note) = &step.note {
            let _ = write!(output, " 卡住：{}", quote(note));
        }
        output.push('\n');
    }
}

/// 时间线一条占一行。内容里的换行要转义，否则多行内容能伪造出一条带时间的记录。
fn one_line(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// 从新到旧。`node` 给定时只看和这个节点有关的事。
fn render_timeline(
    memory: &Memory,
    limit: usize,
    offset: usize,
    node: Option<&str>,
    output: &mut String,
) {
    let matching: Vec<&Event> = memory
        .timeline()
        .iter()
        .rev()
        .filter(|event| node.is_none_or(|id| event.nodes.iter().any(|other| other == id)))
        .collect();
    let total = matching.len();
    if total == 0 {
        output.push_str("时间线上没有记录\n");
        return;
    }
    let shown: Vec<&&Event> = matching.iter().skip(offset).take(limit).collect();
    for event in &shown {
        let _ = writeln!(
            output,
            "{} {}",
            format_time(event.at),
            one_line(&event.text)
        );
    }
    if offset >= total {
        let _ = writeln!(output, "（共 {total} 条，offset {offset} 已经超过末尾）");
    } else if shown.len() < total {
        let (first, last) = (offset + 1, offset + shown.len());
        if last < total {
            let _ = writeln!(
                output,
                "（共 {total} 条，这里是第 {first}–{last} 条，下一段用 offset {last}）"
            );
        } else {
            let _ = writeln!(
                output,
                "（共 {total} 条，这里是第 {first}–{last} 条，后面没有了）"
            );
        }
    }
}

/// 写语句对应的时间线事件。读语句不记。
fn event_for(statement: &Statement, at: Option<u64>) -> Option<Event> {
    let change = |text: String, nodes: Vec<String>| (text, nodes, Action::Change);
    let (text, nodes, action) = match statement {
        Statement::Add(add) | Statement::Source(add) => {
            let keyword = if matches!(statement, Statement::Add(_)) {
                "add"
            } else {
                "source"
            };
            let title = add.title.as_deref().unwrap_or_default();
            let mut nodes = vec![add.id.text.clone()];
            nodes.extend(add.related_by.iter().map(|name| name.text.clone()));
            (
                format!("{keyword} {} {}", add.id.text, quote(title)),
                nodes,
                Action::Create,
            )
        }
        Statement::Remove(id) => (
            format!("remove {}", id.text),
            vec![id.text.clone()],
            Action::Remove,
        ),
        Statement::Link { from, to } => change(
            format!("link {} -> {}", from.text, to.text),
            vec![from.text.clone()],
        ),
        Statement::Unlink { from, to } => change(
            format!("unlink {} -> {}", from.text, to.text),
            vec![from.text.clone()],
        ),
        Statement::Set { id, field, .. } => {
            let field = match field {
                Field::Title => "title",
                Field::Description => "desc",
            };
            change(format!("set {} {field}", id.text), vec![id.text.clone()])
        }
        Statement::Base { id, on } => change(
            format!("base {} on {}", id.text, on.text),
            vec![id.text.clone()],
        ),
        Statement::Unbase { id, on } => change(
            format!("unbase {} on {}", id.text, on.text),
            vec![id.text.clone()],
        ),
        Statement::Cite { id, citation } => {
            let what = match citation {
                Citation::Text(text) => {
                    format!("{} 0..{}", source_id(text.text.as_bytes()), text.text.len())
                }
                Citation::Source(source) => {
                    format!("{} {}..{}", source.source.text, source.start, source.end)
                }
            };
            change(format!("cite {} {what}", id.text), vec![id.text.clone()])
        }
        Statement::Uncite { id, source } => change(
            format!(
                "uncite {} {} {}..{}",
                id.text, source.source.text, source.start, source.end
            ),
            vec![id.text.clone()],
        ),
        Statement::CatalogCreate { name, .. } => {
            change(format!("catalog create {}", quote(&name.text)), Vec::new())
        }
        Statement::CatalogDescribe { name, .. } => change(
            format!("catalog describe {}", quote(&name.text)),
            Vec::new(),
        ),
        Statement::CatalogRename { name, new_name } => change(
            format!(
                "catalog rename {} to {}",
                quote(&name.text),
                quote(&new_name.text)
            ),
            Vec::new(),
        ),
        Statement::CatalogDelete(name) => {
            change(format!("catalog delete {}", quote(&name.text)), Vec::new())
        }
        Statement::CatalogAdd { id, section } => change(
            format!("catalog add {} to {}", id.text, quote(&section.text)),
            Vec::new(),
        ),
        Statement::CatalogRemove { id, section } => change(
            format!("catalog remove {} from {}", id.text, quote(&section.text)),
            Vec::new(),
        ),
        Statement::Board(op) => change(op.summary(), Vec::new()),
        Statement::Log(text) => change(format!("log {}", text.text), Vec::new()),
        Statement::Versioned(..)
        | Statement::RevisionDisplay(_)
        | Statement::Save
        | Statement::Discard
        | Statement::CatalogShow
        | Statement::Check
        | Statement::Export(_)
        | Statement::Timeline { .. }
        | Statement::Show(_)
        | Statement::Links { .. }
        | Statement::Evidence(_)
        | Statement::BasedBy(_)
        | Statement::RelatedBy(_)
        | Statement::FullSource { .. } => return None,
    };
    Some(Event {
        at,
        text,
        nodes,
        action,
    })
}

fn render_catalog(memory: &Memory, output: &mut String) {
    if memory.catalog().sections.is_empty() {
        output.push_str("catalog: 空\n");
    } else {
        render_sections(memory, output);
    }
}

fn render_sections(memory: &Memory, output: &mut String) {
    for section in &memory.catalog().sections {
        let _ = writeln!(output, "section {}", quote(&section.name));
        if section.description.is_empty() {
            output.push_str("  desc: 无\n");
        } else {
            let _ = writeln!(output, "  desc: {}", quote(&section.description));
        }
        if section.nodes.is_empty() {
            output.push_str("  没有代表节点\n");
        }
        for entry in &section.nodes {
            let _ = writeln!(output, "  {} {}", entry.id, quote(&entry.title));
        }
    }
}

/// 输出节点的全部内容。原文这时才从固态硬盘读取，读不到时其他内容照常输出。
fn render_node(
    node: NodeRef<'_>,
    times: NodeTimes,
    sources: &dyn SourceStore,
    output: &mut String,
    failures: &mut usize,
) {
    let _ = writeln!(output, "node {}", node.id());
    let _ = writeln!(output, "  title: {}", quote(node.title()));
    let _ = writeln!(output, "  desc: {}", quote(node.description()));
    render_list("related", node.related(), output);
    render_list("related_by", node.related_by(), output);
    render_list("based_on", node.based_on(), output);
    render_list("based_by", node.based_by(), output);
    let _ = writeln!(output, "  created: {}", format_time(times.created));
    let _ = writeln!(output, "  updated: {}", format_time(times.updated));

    for evidence in node.evidence() {
        if let Evidence::Source { source, locator } = evidence {
            let Locator::TextRange { start, end } = locator;
            match sources.read(&source, &locator) {
                Ok(text) => {
                    let _ = writeln!(
                        output,
                        "  source: {source} {start}..{end}: {}",
                        quote(&text)
                    );
                }
                Err(error) => {
                    *failures += 1;
                    let _ = writeln!(output, "  source: {source} {start}..{end}");
                    let _ = writeln!(output, "error[E901] 原文读取失败：{error}{DOCTOR_HINT}");
                }
            }
        }
    }
}

/// 输出节点指向的每个来源文件的完整内容，一个文件在多条证据里出现时只输出一次。
/// 一个字符最多占的字节数。
const MAX_CHAR_BYTES: usize = 4;

/// 读原文的一段，不用把整个文件读进内存。范围两端切在字符中间时，退回到字符开头。
/// 返回总字节数、实际的起始和结束，以及这段文字。
fn read_text_window(
    sources: &dyn SourceStore,
    id: &str,
    offset: usize,
    limit: usize,
) -> Result<(usize, usize, usize, String), String> {
    let describe = |error: Error| format!("原文读取失败：{error}");
    let total = sources
        .size(id)
        .map_err(describe)?
        .ok_or_else(|| format!("原文读取失败：来源 {id} 不存在"))?;
    let total = usize::try_from(total).unwrap_or(usize::MAX);
    let wanted_start = offset.min(total);
    let wanted_end = wanted_start.saturating_add(limit).min(total);
    // 多读几个字节，才能看出两端是不是切在字符中间。
    let first = wanted_start.saturating_sub(MAX_CHAR_BYTES);
    let last = wanted_end.saturating_add(MAX_CHAR_BYTES).min(total);
    let bytes = sources
        .read_bytes(id, first as u64, last as u64)
        .map_err(describe)?;
    let is_boundary = |position: usize| {
        position == total
            || bytes
                .get(position - first)
                .is_some_and(|byte| (*byte & 0xC0) != 0x80)
    };
    let mut start = wanted_start;
    while start > first && !is_boundary(start) {
        start -= 1;
    }
    let mut end = wanted_end;
    while end > start && !is_boundary(end) {
        end -= 1;
    }
    // limit 小于一个字符时也要往前走，否则永远读不完。
    if end == start && start < total {
        end = start + 1;
        while end < last && !is_boundary(end) {
            end += 1;
        }
    }
    let text = std::str::from_utf8(&bytes[start - first..end - first])
        .map_err(|_| "这份原文不是文本，无法显示".to_owned())?;
    Ok((total, start, end, text.to_owned()))
}

fn render_full_source(
    node: NodeRef<'_>,
    offset: usize,
    limit: usize,
    sources: &dyn SourceStore,
    output: &mut String,
    failures: &mut usize,
) {
    let _ = writeln!(output, "full_source {}", node.id());
    let mut seen = HashSet::new();
    for evidence in node.evidence() {
        let Evidence::Source { source, .. } = evidence else {
            continue;
        };
        if !seen.insert(source.clone()) {
            continue;
        }
        let window = read_text_window(sources, &source, offset, limit);
        let (total, start, end, text) = match window {
            Ok(window) => window,
            Err(message) => {
                *failures += 1;
                let _ = writeln!(output, "  source: {source}");
                let _ = writeln!(output, "error[E901] {message}");
                continue;
            }
        };
        let _ = writeln!(
            output,
            "  source: {source} 共 {total} 字节，显示 {start}..{end}: {}",
            quote(&text)
        );
        if end < total {
            let _ = writeln!(
                output,
                "  note: 还没有显示完，版本前缀不变，下一段用 full_source {} offset {end}",
                node.id()
            );
        }
    }
    if seen.is_empty() {
        let _ = writeln!(
            output,
            "  这个节点没有直接指向原文，用 show 查看 based_on 里的节点，版本前缀不变"
        );
    }
}

fn render_list<'m>(name: &str, nodes: impl Iterator<Item = NodeRef<'m>>, output: &mut String) {
    let ids: Vec<String> = nodes.map(|node| node.id().to_owned()).collect();
    let _ = writeln!(output, "  {name}: {}", join_ids(&ids));
}

fn join_ids(ids: &[String]) -> String {
    if ids.is_empty() {
        "无".to_owned()
    } else {
        ids.join(", ")
    }
}

fn render_links(
    start: NodeRef<'_>,
    depth: usize,
    limit: usize,
    offset: usize,
    output: &mut String,
) {
    let mut seen = HashSet::from([start]);
    let mut pending = VecDeque::from([(start, 0)]);
    let mut total = 0;
    let mut shown = 0;
    while let Some((node, level)) = pending.pop_front() {
        if level >= depth {
            continue;
        }
        for target in node.related() {
            if total >= offset && shown < limit {
                let _ = writeln!(output, "{} -> {}", node.id(), target.id());
                shown += 1;
            }
            total += 1;
            if seen.insert(target) {
                pending.push_back((target, level + 1));
            }
        }
    }
    if total == 0 {
        let _ = writeln!(output, "{} 没有关联", start.id());
    } else if offset >= total {
        let _ = writeln!(output, "（共 {total} 条，offset {offset} 已经超过末尾）");
    } else if total > shown {
        let (first, last) = (offset + 1, offset + shown);
        if last < total {
            let _ = writeln!(
                output,
                "（共 {total} 条，这里是第 {first}–{last} 条，下一段用 offset {last}）"
            );
        } else {
            let _ = writeln!(
                output,
                "（共 {total} 条，这里是第 {first}–{last} 条，后面没有了）"
            );
        }
    }
}

/// 沿依据一路追到原文。依据节点带上标题和描述，推理才看得完整。每个节点只展开一次。
fn render_evidence(
    start: NodeRef<'_>,
    memory: &Memory,
    sources: &dyn SourceStore,
    output: &mut String,
    failures: &mut usize,
) {
    let _ = writeln!(output, "evidence {}", start.id());
    let mut seen = HashSet::from([start]);
    // 用栈而不是递归：依据链再长也不会撑爆调用栈。
    let mut pending: Vec<(Evidence, usize)> = start
        .evidence()
        .into_iter()
        .rev()
        .map(|entry| (entry, 1))
        .collect();
    while let Some((entry, depth)) = pending.pop() {
        let indent = "  ".repeat(depth);
        match entry {
            Evidence::Source { source, locator } => match sources.read(&source, &locator) {
                Ok(text) => {
                    let Locator::TextRange { start, end } = locator;
                    let _ = writeln!(
                        output,
                        "{indent}source {source} {start}..{end}: {}",
                        quote(&text)
                    );
                }
                Err(error) => {
                    *failures += 1;
                    let _ = writeln!(
                        output,
                        "{indent}error[E901] 原文读取失败：{error}{DOCTOR_HINT}"
                    );
                }
            },
            Evidence::Node { id } => {
                let Some(node) = memory.node(&id) else {
                    continue;
                };
                if !seen.insert(node) {
                    let _ = writeln!(output, "{indent}node {id}（上面已列出）");
                    continue;
                }
                let _ = writeln!(output, "{indent}node {id} {}", quote(node.title()));
                let _ = writeln!(output, "{indent}  desc: {}", quote(node.description()));
                pending.extend(
                    node.evidence()
                        .into_iter()
                        .rev()
                        .map(|entry| (entry, depth + 1)),
                );
            }
        }
    }
}

fn render_removed(removed: &RemovedNode, output: &mut String) {
    let _ = writeln!(output, "removed {}", removed.node.id);
    let _ = writeln!(output, "  desc: {}", quote(&removed.node.description));
    for id in &removed.related_by {
        let _ = writeln!(
            output,
            "warning: {id} 少了一条指向 {} 的关联",
            removed.node.id
        );
    }
    for section in &removed.sections {
        let _ = writeln!(output, "warning: 板块 {} 少了一个代表节点", quote(section));
    }
    let _ = writeln!(output, "note: 可以用 revision check 确认是否有节点查不到了");
}

/// 描述变了，依据它的结论可能要跟着变。
fn render_review(node: NodeRef<'_>, changed: &str, output: &mut String) {
    for dependent in node.based_by() {
        let _ = writeln!(
            output,
            "warning: {} 依据 {}，{changed}变了，它可能需要复查",
            dependent.id(),
            node.id()
        );
    }
}

/// 原文的问题：缺失或者比引用的范围短。不校验哈希，否则每次 `check` 都要读完所有原文。
fn damaged_sources(memory: &Memory, sources: &dyn SourceStore) -> BTreeMap<SourceId, Vec<String>> {
    let mut sizes: HashMap<SourceId, Option<u64>> = HashMap::new();
    let mut damaged: BTreeMap<SourceId, Vec<String>> = BTreeMap::new();
    for node in memory.nodes() {
        for evidence in node.evidence() {
            let Evidence::Source { source, locator } = evidence else {
                continue;
            };
            let size = *sizes
                .entry(source.clone())
                .or_insert_with(|| sources.size(&source).ok().flatten());
            let Locator::TextRange { end, .. } = locator;
            if size.is_none_or(|size| end as u64 > size) {
                damaged
                    .entry(source)
                    .or_default()
                    .push(node.id().to_owned());
            }
        }
    }
    damaged
}

fn render_check(
    memory: &Memory,
    sources: &dyn SourceStore,
    manual: Option<&Manual>,
    output: &mut String,
) {
    // 旧版本可能建过这种节点。它们看起来像规则，其实是数据，提醒模型迁走。
    if manual.is_some() {
        for node in memory.nodes() {
            if node.id().starts_with(manual::RESERVED_PREFIX) {
                let _ = writeln!(
                    output,
                    "note: 节点 {} 用了内置说明的前缀，但它是记忆，不是规则。新建节点把它迁走，见 manual-repair-delete",
                    node.id()
                );
            }
        }
    }
    let unreachable = memory.unreachable();
    let empty: Vec<&str> = memory
        .catalog()
        .sections
        .iter()
        .filter(|section| section.nodes.is_empty())
        .map(|section| section.name.as_str())
        .collect();
    let damaged = damaged_sources(memory, sources);
    if unreachable.is_empty() && empty.is_empty() && damaged.is_empty() {
        output.push_str("check: 没有问题\n");
        return;
    }
    for id in &unreachable {
        let _ = writeln!(output, "problem: 从总表到达不了 {id}");
    }
    for name in &empty {
        let _ = writeln!(output, "problem: 板块 {} 没有代表节点", quote(name));
    }
    for (source, nodes) in &damaged {
        let _ = writeln!(
            output,
            "problem: 原文 {source} 丢失或不完整，引用它的节点：{}",
            nodes.join(", ")
        );
    }
    let _ = write!(
        output,
        "check: {} 个节点查不到，{} 个板块没有代表节点",
        unreachable.len(),
        empty.len()
    );
    if damaged.is_empty() {
        output.push('\n');
    } else {
        let _ = writeln!(output, "，{} 份原文有问题", damaged.len());
    }
}

/// 输出停在这条语句中间时，怎么看到剩下的内容。
pub fn continue_hint(statement: &Statement) -> String {
    let (prefix, inner) = match statement {
        Statement::Versioned(version, inner) => (version.keyword(), &**inner),
        Statement::RevisionDisplay(None) => {
            return "revision display 没有显示完，用 revision display 节点 逐个看".to_owned();
        }
        other => ("revision", other),
    };
    match inner {
        Statement::Show(id) => format!(
            "{prefix} show {0} 没有显示完，原文可以用 {prefix} full_source {0} 分段读",
            id.text
        ),
        Statement::Evidence(id) => format!(
            "{prefix} evidence {0} 没有显示完，原文可以用 {prefix} full_source 分段读，依据的节点可以单独 {prefix} show",
            id.text
        ),
        Statement::FullSource { id, .. } => {
            format!(
                "{prefix} full_source {} 没有显示完，用更小的 limit 分段读",
                id.text
            )
        }
        Statement::Links { id, .. } => {
            format!(
                "{prefix} links {} 没有显示完，用更小的 limit 和 offset 分段看",
                id.text
            )
        }
        _ => "这条语句的输出没有显示完".to_owned(),
    }
}

/// `revision display` 的输出。第一行固定是标志，开发者可以拿它做门控，输出被截断也还在。
fn render_revision(memory: &mut Memory, only: Option<&str>, output: &mut String) {
    let Some(revision) = memory.revision() else {
        output.push_str("has_revision: false\n");
        return;
    };
    output.push_str("has_revision: true\n");
    let _ = writeln!(output, "scripts: {}", memory.draft_scripts().len());
    if let Some(id) = only {
        match revision.nodes.iter().find(|change| change.id == id) {
            Some(change) => render_node_change(change, output),
            None => {
                let _ = writeln!(output, "{id} 没有改动");
            }
        }
        return;
    }
    for change in &revision.nodes {
        render_node_change(change, output);
    }
    if let Some(before) = &revision.catalog_before {
        render_catalog_change(before, memory.catalog(), output);
    }
    let events = memory.draft_events();
    if !events.is_empty() {
        output.push_str("修改记录\n");
    }
    for event in events {
        let _ = writeln!(
            output,
            "  {} {}",
            format_time(event.at),
            one_line(&event.text)
        );
    }
}

fn render_node_change(change: &NodeChange, output: &mut String) {
    let (before, after) = match (&change.before, &change.after) {
        (None, Some(after)) => {
            let _ = writeln!(output, "+ {} {}", change.id, quote(&after.title));
            return;
        }
        (Some(before), None) => {
            let _ = writeln!(output, "- {} {}", change.id, quote(&before.title));
            return;
        }
        (Some(before), Some(after)) => (before, after),
        (None, None) => return,
    };
    let _ = writeln!(output, "~ {}", change.id);
    if before.title != after.title {
        let _ = writeln!(
            output,
            "  title: {} -> {}",
            quote(&before.title),
            quote(&after.title)
        );
    }
    if before.description != after.description {
        let _ = writeln!(
            output,
            "  desc: {} -> {}",
            quote(&before.description),
            quote(&after.description)
        );
    }
    if before.related != after.related {
        render_list_change("related", &before.related, &after.related, output);
    }
    if before.evidence != after.evidence {
        let describe = |evidence: &[Evidence]| -> Vec<String> {
            evidence
                .iter()
                .map(|entry| match entry {
                    Evidence::Node { id } => format!("based_on {id}"),
                    Evidence::Source {
                        source,
                        locator: Locator::TextRange { start, end },
                    } => format!("source {source} {start}..{end}"),
                })
                .collect()
        };
        let (old, new) = (describe(&before.evidence), describe(&after.evidence));
        render_list_change("evidence", &old, &new, output);
    }
}

fn render_list_change(name: &str, before: &[String], after: &[String], output: &mut String) {
    let added: Vec<String> = after
        .iter()
        .filter(|item| !before.contains(item))
        .map(|item| format!("+{item}"))
        .collect();
    let removed: Vec<String> = before
        .iter()
        .filter(|item| !after.contains(item))
        .map(|item| format!("-{item}"))
        .collect();
    if added.is_empty() && removed.is_empty() {
        let _ = writeln!(output, "  {name}: 顺序变了");
    } else {
        let _ = writeln!(output, "  {name}: {}", [added, removed].concat().join(" "));
    }
}

fn render_catalog_change(before: &Catalog, after: &Catalog, output: &mut String) {
    for old in &before.sections {
        if !after.sections.iter().any(|new| new.name == old.name) {
            let _ = writeln!(output, "- section {}", quote(&old.name));
        }
    }
    for new in &after.sections {
        let Some(old) = before.sections.iter().find(|old| old.name == new.name) else {
            let _ = writeln!(output, "+ section {}", quote(&new.name));
            for entry in &new.nodes {
                let _ = writeln!(output, "  代表: +{}", entry.id);
            }
            continue;
        };
        if old == new {
            continue;
        }
        let _ = writeln!(output, "~ section {}", quote(&new.name));
        if old.description != new.description {
            let _ = writeln!(
                output,
                "  desc: {} -> {}",
                quote(&old.description),
                quote(&new.description)
            );
        }
        let ids = |section: &crate::knowledge::Section| -> Vec<String> {
            section.nodes.iter().map(|entry| entry.id.clone()).collect()
        };
        let (old_ids, new_ids) = (ids(old), ids(new));
        if old_ids != new_ids {
            render_list_change("代表", &old_ids, &new_ids, output);
        }
    }
    for old in &before.goals {
        match after.goals.iter().find(|new| new.id == old.id) {
            None => {
                let _ = writeln!(output, "- goal {}", old.id);
            }
            Some(new) if new != old => {
                let _ = writeln!(output, "~ goal {} 改之前", old.id);
                render_goal(old, output);
                let _ = writeln!(output, "~ goal {} 改之后", old.id);
                render_goal(new, output);
            }
            Some(_) => {}
        }
    }
    for new in &after.goals {
        if !before.goals.iter().any(|old| old.id == new.id) {
            let _ = writeln!(output, "+ goal {}", new.id);
            render_goal(new, output);
        }
    }
}
