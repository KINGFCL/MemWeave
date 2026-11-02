//! 维护知识库：压缩、清除和修复。都要独占写入权，所以服务运行时不能做。
//!
//! 三者都是生成一份新的快照再换上去：换的过程中崩溃，下次打开时 [`recover_interrupted_swap`] 会
//! 把它做完。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{self, Path, PathBuf};

use crate::durable;
use crate::error::Error;
use crate::export::Exporter;
use crate::export::json::JsonExporter;
use crate::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
use crate::lang::journal::{Journal, WriteLock, complete_lines, load_current, parse_entry};
use crate::lang::{Mode, Session};
use crate::memory::{Memory, Problem, is_valid_id};
use crate::parse::ParsedKnowledge;
use crate::parse::json::{JsonParser, check_format, parse_tolerant};
use crate::source::file::FileSources;
use crate::source::{SourceStore, Trusted, is_valid_source_id, source_id};
use crate::timeline::{self, Action, Event};

/// 原文丢失后补上的证据：节点至少要有一条证据，但不能假装依据还在。
const LOST_SOURCE: &str = "（这条记忆原来的依据已经丢失，由 memweave doctor 恢复时补上。）";
const RECOVERED_SECTION: &str = "恢复的节点";

fn io_failed(path: &Path, error: &std::io::Error) -> Error {
    Error::Save(format!("{}：{error}", path.display()))
}

fn sibling(knowledge_base: &Path, suffix: &str) -> Result<PathBuf, Error> {
    let absolute = path::absolute(knowledge_base).map_err(|e| io_failed(knowledge_base, &e))?;
    let name = absolute
        .file_name()
        .ok_or_else(|| Error::Save(format!("{} 不是知识库目录", knowledge_base.display())))?;
    let mut file_name = name.to_os_string();
    file_name.push(suffix);
    Ok(absolute.with_file_name(file_name))
}

/// 完成上次没换完的快照。打开知识库之前调用。
///
/// 换的步骤是：写好 `.swap-new`，把知识库改名成 `.swap-old`，把 `.swap-new` 改名成知识库，删除
/// `.swap-old`。无论停在哪一步，这里都能得到一份完整的知识库。
pub fn recover_interrupted_swap(knowledge_base: &Path) -> Result<(), Error> {
    let new = sibling(knowledge_base, ".swap-new")?;
    let old = sibling(knowledge_base, ".swap-old")?;
    if !knowledge_base.exists() {
        for candidate in [&new, &old] {
            if candidate.join("catalog.json").exists() {
                fs::rename(candidate, knowledge_base).map_err(|e| io_failed(candidate, &e))?;
                eprintln!("完成了上次中断的快照替换：{}", knowledge_base.display());
                break;
            }
        }
    }
    if !knowledge_base.exists() || (!new.exists() && !old.exists()) {
        return Ok(());
    }
    // 拿不到写入权说明别的进程正在替换，不能动它的文件。
    let Ok(_lock) = WriteLock::acquire(knowledge_base) else {
        return Ok(());
    };
    for leftover in [&new, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).map_err(|e| io_failed(leftover, &e))?;
        }
    }
    Ok(())
}

/// 把 `memory` 写成新快照，换掉旧的知识库。旧的知识库删除，日志和没人引用的原文也就没了。
fn replace_with_snapshot(
    knowledge_base: &Path,
    memory: &Memory,
    sources: &dyn SourceStore,
) -> Result<(), Error> {
    let new = sibling(knowledge_base, ".swap-new")?;
    let old = sibling(knowledge_base, ".swap-old")?;
    JsonExporter.export(memory, sources, &new)?;
    fs::rename(knowledge_base, &old).map_err(|e| io_failed(knowledge_base, &e))?;
    fs::rename(&new, knowledge_base).map_err(|e| io_failed(&new, &e))?;
    durable::sync_dir(knowledge_base.parent().unwrap_or(Path::new("")))
        .map_err(|e| io_failed(knowledge_base, &e))?;
    fs::remove_dir_all(&old).map_err(|e| io_failed(&old, &e))
}

fn directory_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => directory_size(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

/// 把日志合并进快照，去掉没人引用的原文。有节点走不到时和导出一样拒绝，并列出它们。
pub fn compact(knowledge_base: &Path) -> Result<String, Error> {
    recover_interrupted_swap(knowledge_base)?;
    let lock = WriteLock::acquire(knowledge_base)?;
    let sources = FileSources::new(knowledge_base);
    let memory = load_current(&JsonParser, &sources, knowledge_base)?;
    let before = directory_size(knowledge_base);
    replace_with_snapshot(knowledge_base, &memory, &sources)?;
    drop(lock);
    Ok(format!(
        "压缩完成：{} 字节 → {} 字节，{} 个节点",
        before,
        directory_size(knowledge_base),
        memory.node_count()
    ))
}

/// 彻底清除一段文字：从时间线上抹掉，日志和没人引用的原文随压缩一起消失。
///
/// 记忆里还在用这段文字时拒绝并列出来：先 `remove` 或改掉它们。清除不了已经导出的副本和备份。
pub fn purge(knowledge_base: &Path, secret: &str) -> Result<String, Error> {
    if secret.trim().is_empty() {
        return Err(Error::Save("要清除的文字不能为空".to_owned()));
    }
    recover_interrupted_swap(knowledge_base)?;
    let lock = WriteLock::acquire(knowledge_base)?;
    let sources = FileSources::new(knowledge_base);
    let mut memory = load_current(&JsonParser, &sources, knowledge_base)?;
    let in_use = uses_of(&memory, &sources, secret)?;
    if !in_use.is_empty() {
        return Err(Error::Save(format!(
            "记忆里还在用这段文字，先用 remove、set 或 uncite 处理：{}",
            in_use.join("、")
        )));
    }
    let scrubbed = memory.scrub_timeline(secret);
    replace_with_snapshot(knowledge_base, &memory, &sources)?;
    drop(lock);
    Ok(format!(
        "清除完成：抹掉了 {scrubbed} 条时间线记录，日志和没人引用的原文已删除。已经导出的副本和 .damaged 备份不受影响"
    ))
}

fn uses_of(memory: &Memory, sources: &dyn SourceStore, secret: &str) -> Result<Vec<String>, Error> {
    let mut uses = Vec::new();
    let mut checked: HashMap<String, bool> = HashMap::new();
    let needle = secret.as_bytes();
    for node in memory.nodes() {
        if node.id().contains(secret)
            || node.title().contains(secret)
            || node.description().contains(secret)
        {
            uses.push(format!("节点 {}", node.id()));
        }
        for evidence in node.evidence() {
            if let Evidence::Source { source, .. } = evidence {
                let contains = match checked.get(&source) {
                    Some(contains) => *contains,
                    None => {
                        let bytes = sources.read_all(&source)?;
                        let contains = bytes.windows(needle.len()).any(|part| part == needle);
                        checked.insert(source.clone(), contains);
                        contains
                    }
                };
                if contains {
                    uses.push(format!("节点 {} 引用的原文 {source}", node.id()));
                }
            }
        }
    }
    let catalog = memory.catalog();
    for section in &catalog.sections {
        if section.name.contains(secret) || section.description.contains(secret) {
            uses.push(format!("板块 {}", section.name));
        }
    }
    for goal in &catalog.goals {
        let in_steps = goal.steps.iter().any(|step| {
            step.text.contains(secret)
                || step
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains(secret))
        });
        if goal.id.contains(secret)
            || goal.title.contains(secret)
            || goal.description.contains(secret)
            || in_steps
        {
            uses.push(format!("目标 {}", goal.id));
        }
    }
    Ok(uses)
}

/// 只检查时用：写入新来源时只放在内存里，不碰磁盘。
struct DryRun<'a> {
    disk: &'a dyn SourceStore,
    written: std::cell::RefCell<HashMap<String, Vec<u8>>>,
}

impl SourceStore for DryRun<'_> {
    fn size(&self, id: &str) -> Result<Option<u64>, Error> {
        match self.written.borrow().get(id) {
            Some(bytes) => Ok(Some(bytes.len() as u64)),
            None => self.disk.size(id),
        }
    }

    fn read(&self, id: &str, locator: &Locator) -> Result<String, Error> {
        match self.written.borrow().get(id) {
            Some(bytes) => {
                let Locator::TextRange { start, end } = *locator;
                bytes
                    .get(start..end)
                    .and_then(|part| std::str::from_utf8(part).ok())
                    .map(str::to_owned)
                    .ok_or_else(|| Error::Source(format!("来源 {id} 的范围无效")))
            }
            None => self.disk.read(id, locator),
        }
    }

    fn read_all(&self, id: &str) -> Result<Vec<u8>, Error> {
        match self.written.borrow().get(id) {
            Some(bytes) => Ok(bytes.clone()),
            None => self.disk.read_all(id),
        }
    }

    fn put(&self, bytes: &[u8]) -> Result<String, Error> {
        let id = source_id(bytes);
        self.written.borrow_mut().insert(id.clone(), bytes.to_vec());
        Ok(id)
    }
}

/// 尽量恢复出来的记忆。
struct Salvaged {
    memory: Memory,
    problems: Vec<String>,
    failed_scripts: Vec<String>,
}

/// 去掉坏掉的引用，让剩下的内容能建成图。这里改掉的每一处都要写进 `problems`：加载时这些都会被
/// 拒绝，`doctor` 不报告的话，用户就走进了死路。
fn clean_parsed(
    catalog: Option<Catalog>,
    mut nodes: Vec<Node>,
    store: &dyn SourceStore,
    problems: &mut Vec<String>,
) -> Result<ParsedKnowledge, Error> {
    let mut catalog = catalog.unwrap_or_default();
    let mut seen = HashSet::new();
    nodes.retain(|node| {
        let keep = is_valid_id(&node.id) && seen.insert(node.id.clone());
        if !keep {
            problems.push(format!("节点 {} 地址不合法或重复，已丢弃", node.id));
        }
        keep
    });
    let ids: HashSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
    for node in &mut nodes {
        let Node {
            id: owner,
            related,
            evidence,
            ..
        } = node;
        related.retain(|id| {
            let keep = ids.contains(id);
            if !keep {
                problems.push(format!(
                    "节点 {owner} 关联的节点 {id} 不存在，已去掉这条关联"
                ));
            }
            keep
        });
        evidence.retain(|item| {
            let keep = match item {
                Evidence::Node { id } => ids.contains(id),
                Evidence::Source { source, locator } => {
                    let Locator::TextRange { start, end } = *locator;
                    is_valid_source_id(source) && start < end
                }
            };
            if !keep {
                problems.push(match item {
                    Evidence::Node { id } => {
                        format!("节点 {owner} 依据的节点 {id} 不存在，已去掉这条依据")
                    }
                    Evidence::Source { source, locator } => {
                        let Locator::TextRange { start, end } = *locator;
                        format!("节点 {owner} 的原文引用 {source} {start}..{end} 无效，已去掉")
                    }
                });
            }
            keep
        });
    }
    let mut names = HashSet::new();
    catalog.sections.retain(|section| {
        let keep = names.insert(section.name.clone());
        if !keep {
            problems.push(format!("板块“{}”重复，只保留第一个", section.name));
        }
        keep
    });
    for section in &mut catalog.sections {
        let Section {
            name,
            nodes: entries,
            ..
        } = section;
        entries.retain(|entry| {
            let keep = ids.contains(&entry.id);
            if !keep {
                problems.push(format!(
                    "板块“{name}”的代表节点 {} 不存在，已去掉",
                    entry.id
                ));
            }
            keep
        });
    }
    let mut goals = HashSet::new();
    catalog.goals.retain(|goal| {
        let keep = goals.insert(goal.id.clone());
        if !keep {
            problems.push(format!("工作板上的目标 {} 重复，只保留第一个", goal.id));
        }
        keep
    });

    let lost = store.put(LOST_SOURCE.as_bytes())?;
    let lost_evidence = Evidence::Source {
        source: lost,
        locator: Locator::TextRange {
            start: 0,
            end: LOST_SOURCE.len(),
        },
    };
    let mut parsed = ParsedKnowledge { catalog, nodes };
    // 证据循环只有手工改坏才会出现。去掉循环里的依据，最多几轮就能得到合法的图。
    for _ in 0..=parsed.nodes.len() {
        for node in &mut parsed.nodes {
            if node.evidence.is_empty() {
                problems.push(format!("节点 {} 没有证据了，补上了依据丢失的说明", node.id));
                node.evidence.push(lost_evidence.clone());
            }
        }
        for id in attach_unreachable(&mut parsed) {
            problems.push(format!(
                "从总表走不到节点 {id}，已把它放进“{RECOVERED_SECTION}”板块"
            ));
        }
        let found = Memory::validate(&parsed, &Trusted(store));
        if found.is_empty() {
            return Ok(parsed);
        }
        let mut progressed = false;
        for problem in &found {
            if let Problem::EvidenceCycle { ids } = problem {
                problems.push(format!("证据循环：{}，已去掉循环里的依据", ids.join("、")));
                for node in &mut parsed.nodes {
                    if ids.contains(&node.id) {
                        node.evidence.retain(|evidence| {
                            !matches!(evidence, Evidence::Node { id } if ids.contains(id))
                        });
                    }
                }
                progressed = true;
            }
        }
        if !progressed {
            return Err(Error::InvalidMemory(found));
        }
    }
    Err(Error::Save("无法修复证据循环".to_owned()))
}

/// 走不到的节点放进“恢复的节点”板块，这样不会丢，模型也能看到。返回放进去的入口。
fn attach_unreachable(parsed: &mut ParsedKnowledge) -> Vec<String> {
    let targets: HashSet<&str> = parsed
        .nodes
        .iter()
        .flat_map(|node| node.related.iter().map(String::as_str))
        .collect();
    let by_id: HashMap<&str, &Node> = parsed.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut reachable: HashSet<&str> = HashSet::new();
    let mut pending: Vec<&str> = parsed
        .catalog
        .sections
        .iter()
        .flat_map(|section| section.nodes.iter().map(|entry| entry.id.as_str()))
        .collect();
    while let Some(id) = pending.pop() {
        if reachable.insert(id)
            && let Some(node) = by_id.get(id)
        {
            pending.extend(node.related.iter().map(String::as_str));
        }
    }
    // 先选没有入边的节点当入口；只剩环时再选环上的。
    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut covered = reachable.clone();
    let order: Vec<&Node> = parsed
        .nodes
        .iter()
        .filter(|node| !targets.contains(node.id.as_str()))
        .chain(
            parsed
                .nodes
                .iter()
                .filter(|node| targets.contains(node.id.as_str())),
        )
        .collect();
    for node in order {
        if covered.contains(node.id.as_str()) {
            continue;
        }
        entries.push(CatalogEntry {
            id: node.id.clone(),
            title: node.title.clone(),
        });
        let mut pending = vec![node.id.as_str()];
        while let Some(id) = pending.pop() {
            if covered.insert(id)
                && let Some(next) = by_id.get(id)
            {
                pending.extend(next.related.iter().map(String::as_str));
            }
        }
    }
    let attached = entries.iter().map(|entry| entry.id.clone()).collect();
    if entries.is_empty() {
        return attached;
    }
    if let Some(section) = parsed
        .catalog
        .sections
        .iter_mut()
        .find(|section| section.name == RECOVERED_SECTION)
    {
        section.nodes.extend(entries);
    } else {
        parsed.catalog.sections.push(Section {
            name: RECOVERED_SECTION.to_owned(),
            description: "修复时找回的节点，原来的入口丢了。请把它们接回合适的领域。".to_owned(),
            nodes: entries,
        });
    }
    attached
}

fn salvage(knowledge_base: &Path, store: &dyn SourceStore) -> Result<Salvaged, Error> {
    let (catalog, nodes, mut problems) = parse_tolerant(knowledge_base);
    let parsed = clean_parsed(catalog, nodes, store, &mut problems)?;
    let mut memory = Memory::from_parsed(parsed, &Trusted(store))?;

    let events = timeline::read(knowledge_base);
    if events.skipped > 0 {
        problems.push(format!("时间线有 {} 行损坏，已跳过", events.skipped));
    }
    for event in events.events {
        memory.record(event);
    }

    let journal = Journal::new(knowledge_base);
    let bytes = fs::read(journal.path()).unwrap_or_default();
    let mut failed_scripts = Vec::new();
    for (index, line) in complete_lines(&bytes).enumerate() {
        match parse_entry(line) {
            Ok(entry) => {
                let output = Session::new(&mut memory, store, Mode::ReadWrite)
                    .replay(&entry.script, entry.at);
                if !output.ok {
                    problems.push(format!("日志第 {} 行无法重放，已跳过", index + 1));
                    failed_scripts.push(entry.script);
                }
            }
            Err(error) => {
                problems.push(format!("日志第 {} 行损坏，已跳过：{error}", index + 1));
                failed_scripts.push(String::from_utf8_lossy(line).into_owned());
            }
        }
    }
    if bytes.last().is_some_and(|byte| *byte != b'\n') {
        problems.push("日志最后一行没有写完，这段脚本当时没有生效，已丢弃".to_owned());
    }

    replace_damaged_citations(&mut memory, store, &mut problems)?;

    // 重放可能让节点走不到，而快照要求每个节点都能到达。
    let lost = memory.unreachable();
    if !lost.is_empty() {
        let name = RECOVERED_SECTION;
        if !memory.catalog().sections.iter().any(|s| s.name == name) {
            memory.catalog_create(
                name,
                "修复时找回的节点，原来的入口丢了。请把它们接回合适的领域。",
            )?;
        }
        for id in &lost {
            if memory.unreachable().contains(id) {
                problems.push(format!("节点 {id} 走不到，放进了“{name}”板块"));
                memory.catalog_add(id, name)?;
            }
        }
    }
    // 兜底：没发现问题，真实加载却失败时，也要报告。否则用户又走进死路：加载被拒绝，doctor 却说没事。
    if problems.is_empty()
        && let Err(error) = load_current(&JsonParser, store, knowledge_base)
    {
        problems.push(format!("知识库无法加载：{error}"));
    }

    Ok(Salvaged {
        memory,
        problems,
        failed_scripts,
    })
}

/// 读不出来的原文不能留着：导出会失败。节点没有别的证据时，补上依据丢失的说明。
fn replace_damaged_citations(
    memory: &mut Memory,
    store: &dyn SourceStore,
    problems: &mut Vec<String>,
) -> Result<(), Error> {
    let lost = source_id(LOST_SOURCE.as_bytes());
    let lost_range = Locator::TextRange {
        start: 0,
        end: LOST_SOURCE.len(),
    };
    let mut damaged = Vec::new();
    for node in memory.nodes() {
        for evidence in node.evidence() {
            if let Evidence::Source { source, locator } = evidence
                && store.read(&source, &locator).is_err()
            {
                damaged.push((node.id().to_owned(), source, locator));
            }
        }
    }
    if damaged.is_empty() {
        return Ok(());
    }
    store.put(LOST_SOURCE.as_bytes())?;
    for (id, source, locator) in damaged {
        problems.push(format!(
            "节点 {id} 引用的原文 {source} 丢失或损坏，已去掉这条引用"
        ));
        let citation = Evidence::Source {
            source: source.clone(),
            locator: locator.clone(),
        };
        let others = memory
            .node(&id)
            .ok_or_else(|| Error::UnknownNode(id.clone()))?
            .evidence()
            .iter()
            .any(|evidence| *evidence != citation);
        if !others {
            memory.cite(&id, &lost, &lost_range, &Trusted(store))?;
        }
        memory.uncite(&id, &source, &locator)?;
    }
    Ok(())
}

/// 检查知识库，把发现的问题都列出来，不改任何东西。还会校验所有原文的哈希。
pub fn doctor(knowledge_base: &Path) -> Result<String, Error> {
    recover_interrupted_swap(knowledge_base)?;
    // 比这个版本新的知识库不是坏了，是要升级程序。
    check_format(knowledge_base)?;
    let disk = FileSources::new(knowledge_base);
    let dry = DryRun {
        disk: &disk,
        written: std::cell::RefCell::new(HashMap::new()),
    };
    let salvaged = salvage(knowledge_base, &dry)?;
    let mut report = String::new();
    for problem in &salvaged.problems {
        report.push_str(&format!("problem: {problem}\n"));
    }
    report.push_str(&maintenance_notes(knowledge_base, &salvaged.memory));
    if salvaged.problems.is_empty() {
        report.push_str("doctor: 没有问题");
    } else {
        report.push_str(&format!(
            "doctor: {} 个问题。运行 memweave doctor <知识库> --fix 尽量恢复，原来的目录会保留",
            salvaged.problems.len()
        ));
    }
    Ok(report)
}

fn maintenance_notes(knowledge_base: &Path, memory: &Memory) -> String {
    let mut cited = HashSet::new();
    for node in memory.nodes() {
        for evidence in node.evidence() {
            if let Evidence::Source { source, .. } = evidence {
                cited.insert(source.trim_start_matches("sha256:").to_owned());
            }
        }
    }
    let orphans = fs::read_dir(knowledge_base.join("sources"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| !cited.contains(entry.file_name().to_string_lossy().as_ref()))
                .count()
        })
        .unwrap_or(0);
    let journal = fs::metadata(knowledge_base.join("journal.jsonl")).map_or(0, |m| m.len());
    format!(
        "note: {} 个节点，日志 {journal} 字节，{orphans} 份原文没有节点引用。memweave compact 可以合并日志并删除它们\n",
        memory.node_count()
    )
}

/// 尽量恢复。恢复不了的内容如实报告，原来的目录改名保留，无法重放的脚本也存在里面。
pub fn doctor_fix(knowledge_base: &Path) -> Result<String, Error> {
    recover_interrupted_swap(knowledge_base)?;
    check_format(knowledge_base)?;
    let lock = WriteLock::acquire(knowledge_base)?;
    let sources = FileSources::new(knowledge_base);
    let mut salvaged = salvage(knowledge_base, &sources)?;
    if salvaged.problems.is_empty() {
        return Ok("doctor: 没有问题，不用修复".to_owned());
    }
    salvaged.memory.record(Event {
        at: timeline::now(),
        text: format!("doctor 修复：{} 个问题", salvaged.problems.len()),
        nodes: Vec::new(),
        action: Action::Change,
    });
    let at = timeline::now().unwrap_or(0);
    let backup = sibling(knowledge_base, &format!(".damaged-{at}"))?;
    let new = sibling(knowledge_base, ".swap-new")?;
    JsonExporter.export(&salvaged.memory, &sources, &new)?;
    fs::rename(knowledge_base, &backup).map_err(|e| io_failed(knowledge_base, &e))?;
    fs::rename(&new, knowledge_base).map_err(|e| io_failed(&new, &e))?;
    durable::sync_dir(knowledge_base.parent().unwrap_or(Path::new("")))
        .map_err(|e| io_failed(knowledge_base, &e))?;
    drop(lock);
    if !salvaged.failed_scripts.is_empty() {
        let path = backup.join("failed-scripts.txt");
        durable::write_private(&path, salvaged.failed_scripts.join("\n---\n").as_bytes())
            .map_err(|e| io_failed(&path, &e))?;
    }
    let mut report = String::new();
    for problem in &salvaged.problems {
        report.push_str(&format!("fixed: {problem}\n"));
    }
    report.push_str(&format!(
        "doctor: 修复完成，{} 个节点。原来的目录保留在 {}",
        salvaged.memory.node_count(),
        backup.display()
    ));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn demo(dir: &TempDir) -> PathBuf {
        let kb = dir.path().join("kb");
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/demo-kb"),
            &kb,
        );
        kb
    }

    fn write(kb: &Path, script: &str) -> String {
        let sources = FileSources::new(kb);
        let journal = Journal::new(kb);
        let mut memory = load_current(&JsonParser, &sources, kb).unwrap();
        let output = Session::new(&mut memory, &sources, Mode::ReadWrite)
            .with_journal(&journal)
            .run(script);
        assert!(output.ok, "{}", output.text);
        output.text
    }

    fn read(kb: &Path, script: &str) -> String {
        let sources = FileSources::new(kb);
        let mut memory = load_current(&JsonParser, &sources, kb).unwrap();
        Session::new(&mut memory, &sources, Mode::ReadOnly)
            .run(script)
            .text
    }

    const NOTE: &str = "source note\n title \"t\"\n desc \"d\"\n text \"\"\"要保留的原文\"\"\"\n related_by ownership\nend";
    const THROWAWAY: &str = "source gone\n title \"t\"\n desc \"d\"\n text \"\"\"没人引用的原文\"\"\"\n related_by ownership\nend\nremove gone";

    #[test]
    fn compact_merges_the_journal_and_drops_orphan_sources() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, NOTE);
        write(&kb, THROWAWAY);
        write(
            &kb,
            "goal create g \"目标\"\n desc \"d\"\n step \"一\"\nend\nlog \"做过\"",
        );
        let before = read(&kb, "show note\ncatalog show\ntimeline");
        assert_eq!(fs::read_dir(kb.join("sources")).unwrap().count(), 3);

        let report = compact(&kb).unwrap();

        assert!(report.contains("压缩完成"), "{report}");
        assert!(!kb.join("journal.jsonl").exists());
        assert_eq!(fs::read_dir(kb.join("sources")).unwrap().count(), 2);
        assert_eq!(read(&kb, "show note\ncatalog show\ntimeline"), before);
        assert!(!sibling(&kb, ".swap-old").unwrap().exists());
        assert!(!sibling(&kb, ".swap-new").unwrap().exists());
        // 压缩后可以接着写。
        write(&kb, "log \"压缩之后\"");
        assert!(read(&kb, "timeline limit 1").contains("压缩之后"));
    }

    #[test]
    fn compact_refuses_unreachable_nodes_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, "unlink ownership -> drop");

        let error = compact(&kb).unwrap_err().to_string();

        assert!(error.contains("drop"), "{error}");
        assert!(kb.join("journal.jsonl").exists());
        assert!(!sibling(&kb, ".swap-new").unwrap().exists());
    }

    #[test]
    fn an_interrupted_swap_is_finished_at_any_step() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, NOTE);
        let new = sibling(&kb, ".swap-new").unwrap();
        let old = sibling(&kb, ".swap-old").unwrap();
        let expected = read(&kb, "show note");

        // 新快照写了一半：丢掉它，旧的知识库不变。
        fs::create_dir(&new).unwrap();
        fs::write(new.join("半个文件"), "x").unwrap();
        recover_interrupted_swap(&kb).unwrap();
        assert!(!new.exists());
        assert_eq!(read(&kb, "show note"), expected);

        // 旧的已经改名，新的还没换上。
        let sources = FileSources::new(&kb);
        let memory = load_current(&JsonParser, &sources, &kb).unwrap();
        JsonExporter.export(&memory, &sources, &new).unwrap();
        fs::rename(&kb, &old).unwrap();
        recover_interrupted_swap(&kb).unwrap();
        assert_eq!(read(&kb, "show note"), expected);
        assert!(!new.exists() && !old.exists());

        // 只差删除旧的。
        copy_dir(&kb, &old);
        recover_interrupted_swap(&kb).unwrap();
        assert!(!old.exists());
        assert_eq!(read(&kb, "show note"), expected);
    }

    fn grep(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(root).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(grep(&path, needle));
            } else if fs::read(&path)
                .unwrap()
                .windows(needle.len())
                .any(|part| part == needle)
            {
                found.push(path);
            }
        }
        found
    }

    #[test]
    fn purge_removes_every_trace() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        let secret = "SECRET-123456";
        write(
            &kb,
            &format!(
                "source leak-{secret}\n title \"含{secret}\"\n desc \"\"\"密钥 {secret}\"\"\"\n text \"\"\"用户说：{secret}\"\"\"\n related_by ownership\nend\nlog \"\"\"用了 {secret}\"\"\""
            ),
        );
        write(&kb, NOTE);

        // 还在用时拒绝。
        let error = purge(&kb, secret).unwrap_err().to_string();
        assert!(error.contains("还在用"), "{error}");
        assert!(error.contains(&format!("节点 leak-{secret}")), "{error}");

        write(&kb, &format!("remove leak-{secret}"));
        assert!(!grep(&kb, secret.as_bytes()).is_empty());

        let report = purge(&kb, secret).unwrap();

        assert!(report.contains("清除完成"), "{report}");
        assert_eq!(grep(&kb, secret.as_bytes()), Vec::<PathBuf>::new());
        assert!(
            !grep(dir.path(), secret.as_bytes())
                .iter()
                .any(|p| p.starts_with(&kb))
        );
        assert_eq!(grep(dir.path(), secret.as_bytes()), Vec::<PathBuf>::new());
        // 别的记忆和时间线都还在。
        assert!(read(&kb, "show note").contains("要保留的原文"));
        assert!(read(&kb, "timeline").contains("（已清除）"));
        assert!(purge(&kb, "  ").is_err());
    }

    #[test]
    fn a_healthy_knowledge_base_needs_no_fix() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, NOTE);

        let report = doctor(&kb).unwrap();
        assert!(report.ends_with("doctor: 没有问题"), "{report}");
        assert!(doctor_fix(&kb).unwrap().contains("不用修复"));
        // 检查不改任何东西。
        assert!(!kb.join("catalog.json.tmp").exists());
        assert!(kb.join("journal.jsonl").exists());
    }

    fn damaged_and_fixed(damage: impl Fn(&Path), expect: &str) -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, NOTE);
        write(&kb, "log \"修复前的历史\"");
        damage(&kb);

        let report = doctor(&kb).unwrap();
        assert!(report.contains(expect), "{report}");
        assert!(report.contains("--fix"), "{report}");
        let fixed = doctor_fix(&kb).unwrap();
        assert!(fixed.contains("修复完成"), "{fixed}");
        assert!(fixed.contains(".damaged-"), "{fixed}");
        // 修复后能正常加载、检查和写入，备份还在。
        let check = read(&kb, "check");
        assert_eq!(check.trim(), "check: 没有问题", "{check}");
        write(&kb, "log \"修复之后\"");
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains(".damaged-"))
        );
        assert!(doctor(&kb).unwrap().ends_with("doctor: 没有问题"));
        (dir, kb, fixed)
    }

    fn edit_json(path: &Path, change: impl Fn(&mut serde_json::Value)) {
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        change(&mut value);
        fs::write(path, value.to_string()).unwrap();
    }

    /// 真实加载拒绝这种损坏，doctor 就必须报告，修完要能加载。
    fn refused_then_fixed(damage: impl Fn(&Path), expect: &str) -> (TempDir, PathBuf, String) {
        damaged_and_fixed(
            |kb| {
                damage(kb);
                let sources = FileSources::new(kb);
                assert!(
                    load_current(&JsonParser, &sources, kb).is_err(),
                    "这种损坏应该被加载拒绝，不然这个测试没有意义"
                );
            },
            expect,
        )
    }

    #[test]
    fn doctor_reports_a_node_nothing_leads_to() {
        let (_dir, kb, fixed) = refused_then_fixed(
            |kb| {
                let mut island: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(kb.join("nodes/drop.json")).unwrap())
                        .unwrap();
                island["id"] = "island".into();
                island["related"] = serde_json::json!([]);
                fs::write(kb.join("nodes/island.json"), island.to_string()).unwrap();
            },
            "从总表走不到节点 island",
        );
        assert!(fixed.contains("fixed: 从总表走不到节点 island"), "{fixed}");
        assert!(read(&kb, "catalog show").contains("恢复的节点"));
        assert!(read(&kb, "show island").contains("node island"));
    }

    #[test]
    fn doctor_reports_a_dangling_link() {
        let (_dir, kb, _) = refused_then_fixed(
            |kb| {
                edit_json(&kb.join("nodes/drop.json"), |node| {
                    node["related"]
                        .as_array_mut()
                        .unwrap()
                        .push("不存在".into());
                });
            },
            "节点 drop 关联的节点 不存在 不存在",
        );
        assert!(!read(&kb, "show drop").contains("不存在"));
    }

    #[test]
    fn doctor_reports_a_catalog_entry_with_no_node() {
        let (_dir, kb, _) = refused_then_fixed(
            |kb| {
                edit_json(&kb.join("catalog.json"), |catalog| {
                    catalog["sections"][0]["nodes"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({ "id": "不存在", "title": "x" }));
                });
            },
            "的代表节点 不存在 不存在",
        );
        assert!(!read(&kb, "catalog show").contains("不存在"));
    }

    #[test]
    fn doctor_reports_a_duplicate_section() {
        let (_dir, kb, _) = refused_then_fixed(
            |kb| {
                edit_json(&kb.join("catalog.json"), |catalog| {
                    let sections = catalog["sections"].as_array_mut().unwrap();
                    let copy = sections[0].clone();
                    sections.push(copy);
                });
            },
            "重复，只保留第一个",
        );
        let listed = read(&kb, "catalog show");
        assert_eq!(
            listed.matches("section \"Rust 内存管理\"").count(),
            1,
            "{listed}"
        );
    }

    #[test]
    fn doctor_reports_evidence_on_a_missing_node() {
        refused_then_fixed(
            |kb| {
                edit_json(&kb.join("nodes/drop.json"), |node| {
                    node["evidence"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({ "type": "node", "id": "不存在" }));
                });
            },
            "节点 drop 依据的节点 不存在 不存在",
        );
    }

    #[test]
    fn doctor_reports_an_invalid_source_reference() {
        refused_then_fixed(
            |kb| {
                edit_json(&kb.join("nodes/drop.json"), |node| {
                    node["evidence"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "type": "source",
                            "source": "不是地址",
                            "locator": { "type": "text_range", "start": 0, "end": 4 },
                        }));
                });
            },
            "节点 drop 的原文引用 不是地址 0..4 无效",
        );
    }

    #[test]
    fn doctor_reports_a_duplicate_goal() {
        refused_then_fixed(
            |kb| {
                write(
                    kb,
                    "goal create 重复 \"目标\"\n  desc \"\"\"描述\"\"\"\n  step \"一\"\nend",
                );
                compact(kb).unwrap();
                edit_json(&kb.join("catalog.json"), |catalog| {
                    let goals = catalog["goals"].as_array_mut().unwrap();
                    let copy = goals[0].clone();
                    goals.push(copy);
                });
            },
            "工作板上的目标 重复 重复",
        );
    }

    /// 比这个版本新的知识库不是坏了，不能当成坏的去“修”。
    #[test]
    fn doctor_leaves_a_newer_format_alone() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        // 同时有损坏时，旧版本的程序也不能重写它。
        edit_json(&kb.join("nodes/drop.json"), |node| {
            node["related"]
                .as_array_mut()
                .unwrap()
                .push("不存在".into());
        });
        fs::write(kb.join("format.json"), r#"{"format":999}"#).unwrap();
        let before = fs::read_to_string(kb.join("nodes/drop.json")).unwrap();

        assert!(doctor(&kb).unwrap_err().to_string().contains("999"));
        assert!(doctor_fix(&kb).unwrap_err().to_string().contains("999"));

        assert_eq!(
            fs::read_to_string(kb.join("nodes/drop.json")).unwrap(),
            before
        );
        assert_eq!(
            fs::read_to_string(kb.join("format.json")).unwrap(),
            r#"{"format":999}"#
        );
    }

    #[test]
    fn doctor_fixes_a_damaged_node_file() {
        let (_dir, kb, _) = damaged_and_fixed(
            |kb| fs::write(kb.join("nodes/drop.json"), "{").unwrap(),
            "drop.json",
        );
        // 别的节点和日志里的修改都在，指向它的关联去掉了。
        assert!(read(&kb, "show note").contains("要保留的原文"));
        assert!(read(&kb, "show ownership").contains("note"));
        assert!(read(&kb, "show drop").contains("E201"));
        assert!(read(&kb, "timeline").contains("修复前的历史"));
    }

    #[test]
    fn doctor_fixes_a_damaged_catalog() {
        let (_dir, kb, _) = damaged_and_fixed(
            |kb| fs::write(kb.join("catalog.json"), "{").unwrap(),
            "catalog.json",
        );
        let catalog = read(&kb, "catalog show");
        assert!(catalog.contains(RECOVERED_SECTION), "{catalog}");
        assert!(read(&kb, "show note").contains("要保留的原文"));
    }

    #[test]
    fn doctor_fixes_a_lost_source() {
        let (_dir, kb, fixed) = damaged_and_fixed(
            |kb| {
                for entry in fs::read_dir(kb.join("sources")).unwrap().flatten() {
                    if entry.file_name().to_string_lossy().starts_with("95b2") {
                        fs::remove_file(entry.path()).unwrap();
                    }
                }
            },
            "丢失或损坏",
        );
        assert!(fixed.contains("已去掉这条引用"), "{fixed}");
        // 节点还在，并且如实说明依据丢了。
        assert!(read(&kb, "show drop").contains("依据已经丢失"));
        // 日志里写过的原文不受影响。
        assert!(read(&kb, "show note").contains("要保留的原文"));
    }

    #[test]
    fn doctor_skips_a_damaged_journal_line_and_keeps_it_in_the_backup() {
        let (dir, kb, fixed) = damaged_and_fixed(
            |kb| {
                let journal = kb.join("journal.jsonl");
                let text = fs::read_to_string(&journal).unwrap();
                let mut lines: Vec<&str> = text.lines().collect();
                lines.insert(1, "坏掉的一行");
                fs::write(&journal, lines.join("\n") + "\n").unwrap();
            },
            "日志第 2 行损坏",
        );
        // 坏行前后的修改都恢复了。
        assert!(read(&kb, "show note").contains("要保留的原文"));
        assert!(read(&kb, "timeline").contains("修复前的历史"));
        assert!(fixed.contains("已跳过"), "{fixed}");
        let backup = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().contains(".damaged-"))
            .unwrap()
            .path();
        assert!(
            fs::read_to_string(backup.join("failed-scripts.txt"))
                .unwrap()
                .contains("坏掉的一行")
        );
    }

    #[test]
    fn doctor_keeps_the_rest_when_old_syntax_cannot_be_replayed() {
        let (_dir, kb, _) = damaged_and_fixed(
            |kb| {
                fs::write(
                    kb.join("journal.jsonl"),
                    "{\"script\":\"catalog\"}\n{\"script\":\"log \\\"还能重放\\\"\"}\n",
                )
                .unwrap();
            },
            "无法重放",
        );
        assert!(read(&kb, "timeline").contains("还能重放"));
    }

    #[test]
    fn doctor_reports_a_torn_last_line_and_a_changed_source() {
        let dir = TempDir::new().unwrap();
        let kb = demo(&dir);
        write(&kb, NOTE);
        let journal = kb.join("journal.jsonl");
        let mut bytes = fs::read(&journal).unwrap();
        bytes.extend_from_slice(b"{\"script\":\"\xE4\xB8");
        fs::write(&journal, bytes).unwrap();
        let source = fs::read_dir(kb.join("sources"))
            .unwrap()
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().starts_with("95b2"))
            .unwrap()
            .path();
        let original = fs::read(&source).unwrap();
        fs::write(&source, vec![b'x'; original.len()]).unwrap();

        let report = doctor(&kb).unwrap();

        assert!(report.contains("没有写完"), "{report}");
        assert!(report.contains("丢失或损坏"), "{report}");
        // 检查不修改：文件还是坏的。
        assert_eq!(fs::read(&source).unwrap(), vec![b'x'; original.len()]);
    }
}
