//! 执行通过检查的语句，并把结果渲染成文本。
//!
//! 记忆里的内容一律用 JSON 字符串输出：有明确的边界，内容里就算写着指令，也只是数据。

use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

use super::ast::{AddNode, Field, Located, Statement};
use crate::error::Error;
use crate::knowledge::{Evidence, Locator, Node};
use crate::memory::{Memory, NodeRef, Placement, RemovedNode};
use crate::source::{LoadedEvidence, SourceStore, source_id};

/// 执行结果。
pub struct Report {
    pub text: String,
    /// 读取失败的语句数。读取失败不影响后面的语句，这样实际效果才和重放日志一致。
    pub read_failures: usize,
}

/// 依次执行语句。`Err` 里的 usize 是已经执行完的语句数，只有内存不足这类意外才会走到那里。
pub fn execute(
    statements: &[Located],
    memory: &mut Memory,
    sources: &dyn SourceStore,
) -> Result<Report, (usize, String, Error)> {
    let mut report = Report {
        text: String::new(),
        read_failures: 0,
    };
    for (done, located) in statements.iter().enumerate() {
        if let Err(error) = statement(&located.statement, memory, sources, &mut report) {
            return Err((done, report.text, error));
        }
    }
    Ok(report)
}

fn statement(
    statement: &Statement,
    memory: &mut Memory,
    sources: &dyn SourceStore,
    report: &mut Report,
) -> Result<(), Error> {
    let output = &mut report.text;
    match statement {
        Statement::Catalog => render_catalog(memory, output),
        Statement::Check => render_check(memory, output),
        Statement::Show(id) => {
            let node = existing(memory, &id.text)?;
            render_node(node, sources, output, &mut report.read_failures);
        }
        Statement::Links { id, depth, limit } => {
            render_links(existing(memory, &id.text)?, *depth, *limit, output);
        }
        Statement::Evidence(id) => match memory.evidence(&id.text, sources) {
            Ok(evidence) => render_evidence(&id.text, evidence, output),
            Err(error) => {
                report.read_failures += 1;
                let _ = writeln!(output, "error[E901] evidence {} 读取失败：{error}", id.text);
            }
        },
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
            render_full_source(
                node,
                *offset,
                *limit,
                sources,
                output,
                &mut report.read_failures,
            );
        }
        Statement::Add(add) | Statement::Source(add) => {
            let (node, placement) = build_node(add);
            memory.add_node(node, placement, sources)?;
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
                "note: 关联减少后可能有节点查不到了，可以用 check 确认"
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
                render_review(node, output);
            }
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

fn existing<'m>(memory: &'m Memory, id: &str) -> Result<NodeRef<'m>, Error> {
    memory
        .node(id)
        .ok_or_else(|| Error::UnknownNode(id.to_owned()))
}

fn build_node(add: &AddNode) -> (Node, Placement) {
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
    let placement = Placement {
        related_by: add
            .related_by
            .iter()
            .map(|name| name.text.clone())
            .collect(),
        section: add.section.clone(),
    };
    (node, placement)
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("字符串总是可以序列化")
}

fn render_catalog(memory: &Memory, output: &mut String) {
    let sections = &memory.catalog().sections;
    if sections.is_empty() {
        output.push_str("catalog: 空\n");
        return;
    }
    for section in sections {
        let _ = writeln!(output, "section {}", quote(&section.name));
        for entry in &section.nodes {
            let _ = writeln!(output, "  {} {}", entry.id, quote(&entry.title));
        }
    }
}

/// 输出节点的全部内容。原文这时才从固态硬盘读取，读不到时其他内容照常输出。
fn render_node(
    node: NodeRef<'_>,
    sources: &dyn SourceStore,
    output: &mut String,
    read_failures: &mut usize,
) {
    let _ = writeln!(output, "node {}", node.id());
    let _ = writeln!(output, "  title: {}", quote(node.title()));
    let _ = writeln!(output, "  desc: {}", quote(node.description()));
    render_list("related", node.related(), output);
    render_list("related_by", node.related_by(), output);
    render_list("based_on", node.based_on(), output);
    render_list("based_by", node.based_by(), output);

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
                    *read_failures += 1;
                    let _ = writeln!(output, "  source: {source} {start}..{end}");
                    let _ = writeln!(output, "error[E901] 原文读取失败：{error}");
                }
            }
        }
    }
}

/// 输出节点指向的每个来源文件的完整内容，一个文件在多条证据里出现时只输出一次。
fn render_full_source(
    node: NodeRef<'_>,
    offset: usize,
    limit: usize,
    sources: &dyn SourceStore,
    output: &mut String,
    read_failures: &mut usize,
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
        let bytes = match sources.read_all(&source) {
            Ok(bytes) => bytes,
            Err(error) => {
                *read_failures += 1;
                let _ = writeln!(output, "  source: {source}");
                let _ = writeln!(output, "error[E901] 原文读取失败：{error}");
                continue;
            }
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            *read_failures += 1;
            let _ = writeln!(output, "  source: {source}");
            let _ = writeln!(output, "error[E901] 这份原文不是文本，无法显示");
            continue;
        };
        let start = floor_char_boundary(text, offset);
        let end = floor_char_boundary(text, start.saturating_add(limit));
        // limit 小于一个字符时也要往前走，否则永远读不完。
        let end = if end == start {
            text[start..]
                .chars()
                .next()
                .map_or(start, |c| start + c.len_utf8())
        } else {
            end
        };
        let total = text.len();
        let _ = writeln!(
            output,
            "  source: {source} 共 {total} 字节，显示 {start}..{end}: {}",
            quote(&text[start..end])
        );
        if end < total {
            let _ = writeln!(
                output,
                "  note: 还没有显示完，下一段用 full_source {} offset {end}",
                node.id()
            );
        }
    }
    if seen.is_empty() {
        let _ = writeln!(
            output,
            "  这个节点没有直接指向原文，用 show 查看 based_on 里的节点"
        );
    }
}

/// 不超过 `index` 的最近的字符边界。
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
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

fn render_links(start: NodeRef<'_>, depth: usize, limit: usize, output: &mut String) {
    let mut seen = HashSet::from([start]);
    let mut pending = VecDeque::from([(start, 0)]);
    let mut total = 0;
    let mut shown = 0;
    while let Some((node, level)) = pending.pop_front() {
        if level >= depth {
            continue;
        }
        for target in node.related() {
            total += 1;
            if shown < limit {
                let _ = writeln!(output, "{} -> {}", node.id(), target.id());
                shown += 1;
            }
            if seen.insert(target) {
                pending.push_back((target, level + 1));
            }
        }
    }
    if total == 0 {
        let _ = writeln!(output, "{} 没有关联", start.id());
    } else if total > shown {
        let _ = writeln!(
            output,
            "（共 {total} 条，只显示前 {shown} 条，可以调大 limit）"
        );
    }
}

fn render_evidence(id: &str, evidence: Vec<LoadedEvidence>, output: &mut String) {
    let _ = writeln!(output, "evidence {id}");
    for entry in evidence {
        match entry {
            LoadedEvidence::Source {
                source,
                locator: Locator::TextRange { start, end },
                text,
            } => {
                let _ = writeln!(output, "  source {source} {start}..{end}: {}", quote(&text));
            }
            LoadedEvidence::Node { id } => {
                let _ = writeln!(output, "  node {id}");
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
    let _ = writeln!(output, "note: 可以用 check 确认是否有节点查不到了");
}

/// 描述变了，依据它的结论可能要跟着变。
fn render_review(node: NodeRef<'_>, output: &mut String) {
    for dependent in node.based_by() {
        let _ = writeln!(
            output,
            "warning: {} 依据 {}，内容变了，它可能需要复查",
            dependent.id(),
            node.id()
        );
    }
}

fn render_check(memory: &Memory, output: &mut String) {
    let unreachable = memory.unreachable();
    if unreachable.is_empty() {
        output.push_str("check: 没有问题\n");
        return;
    }
    for id in &unreachable {
        let _ = writeln!(output, "problem: 从总表到达不了 {id}");
    }
    let _ = writeln!(output, "check: {} 个节点查不到", unreachable.len());
}
