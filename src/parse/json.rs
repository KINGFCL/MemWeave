use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::{ParsedKnowledge, Parser};
use crate::error::Error;
use crate::knowledge::{Catalog, Node};

/// 记录格式版本的文件。没有这个文件的是第 1 版。
pub const FORMAT_FILE: &str = "format.json";
/// 知识库目录的格式版本。不兼容地改格式时要加一。
pub const FORMAT_VERSION: u32 = 1;

/// JSON 目录格式：`catalog.json` 加 `nodes/<节点地址>.json`。来源不会被读取，它们留在固态硬盘上。
///
/// `nodes` 目录只读取第一层的 `.json` 文件，其他文件会被忽略。
pub struct JsonParser;

impl Parser for JsonParser {
    fn parse(&self, source: &Path) -> Result<ParsedKnowledge, Error> {
        check_format(source)?;
        let mut problems = Vec::new();
        let catalog = match read_json::<Catalog>(&source.join("catalog.json")) {
            Ok(catalog) => Some(catalog),
            Err(problem) => {
                problems.push(problem);
                None
            }
        };
        let nodes = read_nodes(&source.join("nodes"), &mut problems);

        match catalog {
            Some(catalog) if problems.is_empty() => Ok(ParsedKnowledge { catalog, nodes }),
            _ => Err(Error::Parse(problems)),
        }
    }
}

/// 尽量读出来：坏掉的文件跳过，并报告问题。修复时用。总表读不出来时返回 `None`。
pub(crate) fn parse_tolerant(source: &Path) -> (Option<Catalog>, Vec<Node>, Vec<String>) {
    let mut problems = Vec::new();
    let catalog = match read_json::<Catalog>(&source.join("catalog.json")) {
        Ok(catalog) => Some(catalog),
        Err(problem) => {
            problems.push(problem);
            None
        }
    };
    let nodes = read_nodes(&source.join("nodes"), &mut problems);
    (catalog, nodes, problems)
}

/// 比这个版本新的知识库不能读：读错了可能损坏数据。
pub(crate) fn check_format(source: &Path) -> Result<(), Error> {
    let path = source.join(FORMAT_FILE);
    if !path.exists() {
        return Ok(());
    }
    let value: serde_json::Value =
        read_json(&path).map_err(|problem| Error::Parse(vec![problem]))?;
    let format = value["format"].as_u64().unwrap_or(u64::MAX);
    if format > u64::from(FORMAT_VERSION) {
        return Err(Error::Parse(vec![format!(
            "{} 是第 {format} 版格式，当前版本只支持到第 {FORMAT_VERSION} 版，请升级 memweave",
            source.display()
        )]));
    }
    Ok(())
}

fn read_nodes(dir: &Path, problems: &mut Vec<String>) -> Vec<Node> {
    let mut nodes = Vec::new();
    for path in node_files(dir, problems) {
        match read_node(&path) {
            Ok(node) => nodes.push(node),
            Err(problem) => problems.push(problem),
        }
    }
    nodes
}

/// 列出节点文件。排序后结果才是确定的，`read_dir` 本身不保证顺序。
fn node_files(dir: &Path, problems: &mut Vec<String>) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            problems.push(format!("{}：{e}", dir.display()));
            return Vec::new();
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                    paths.push(path);
                }
            }
            Err(e) => problems.push(format!("{}：{e}", dir.display())),
        }
    }
    paths.sort();
    paths
}

fn read_node(path: &Path) -> Result<Node, String> {
    let node: Node = read_json(path)?;
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) if stem == node.id => Ok(node),
        _ => Err(format!(
            "{}：文件里的 id 是 {:?}，与文件名不一致",
            path.display(),
            node.id
        )),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}：{e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}：{e}", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::knowledge::{Evidence, Locator};
    use crate::memory::Memory;
    use crate::source::file::FileSources;
    use crate::source::{LoadedEvidence, SourceStore, source_id};

    const CATALOG: &str = r#"{"sections":[{"name":"板块","nodes":[{"id":"a","title":"A"}]}]}"#;

    fn node_json(id: &str) -> String {
        format!(r#"{{"id":"{id}","title":"T","description":"D","evidence":[]}}"#)
    }

    fn demo_path() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo-kb"))
    }

    fn load_demo() -> Memory {
        Memory::load(&JsonParser, &FileSources::new(demo_path()), demo_path()).unwrap()
    }

    /// 创建知识库目录。`nodes` 的每一项是（文件名，内容）。
    fn knowledge_base(catalog: &str, nodes: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("catalog.json"), catalog).unwrap();
        fs::create_dir(dir.path().join("nodes")).unwrap();
        for (name, content) in nodes {
            fs::write(dir.path().join("nodes").join(name), content).unwrap();
        }
        dir
    }

    fn problems(result: Result<ParsedKnowledge, Error>) -> Vec<String> {
        match result {
            Err(Error::Parse(problems)) => problems,
            other => panic!("应为解析错误：{other:?}"),
        }
    }

    #[test]
    fn demo_knowledge_base_loads_into_memory() {
        let memory = load_demo();

        // drop 不在总表里，只能沿关联到达。
        assert_eq!(memory.node_count(), 4);
    }

    #[test]
    fn demo_evidence_can_be_traced_to_its_source() {
        let memory = load_demo();
        let sources = FileSources::new(demo_path());

        for node in memory.nodes() {
            let evidence = memory.evidence(node.id(), &sources).unwrap();
            assert!(!evidence.is_empty(), "{}", node.id());
        }
        let LoadedEvidence::Source { text, .. } = &memory.evidence("drop", &sources).unwrap()[0]
        else {
            panic!("drop 的第一条证据应该来自来源");
        };
        assert!(text.contains("Drop"), "{text}");
    }

    #[test]
    fn nodes_are_sorted_and_other_files_ignored() {
        let (a, b) = (node_json("a"), node_json("b"));
        let dir = knowledge_base(
            CATALOG,
            &[("b.json", &b), ("a.json", &a), ("说明.txt", "不是节点")],
        );
        fs::create_dir(dir.path().join("nodes/子目录.json")).unwrap();

        let parsed = JsonParser.parse(dir.path()).unwrap();

        let ids: Vec<&str> = parsed.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(parsed.catalog.sections[0].name, "板块");
    }

    #[test]
    fn an_empty_knowledge_base_loads() {
        let dir = knowledge_base(r#"{"sections":[]}"#, &[]);

        let memory = Memory::load(&JsonParser, &FileSources::new(dir.path()), dir.path()).unwrap();

        assert_eq!(memory.node_count(), 0);
        assert!(memory.catalog().sections.is_empty());
    }

    #[test]
    fn section_descriptions_are_read_and_old_catalogs_still_parse() {
        let a = node_json("a");
        let old = knowledge_base(CATALOG, &[("a.json", &a)]);
        let new = knowledge_base(
            r#"{"sections":[{"name":"板块","description":"描述","nodes":[]}]}"#,
            &[],
        );

        let old = JsonParser.parse(old.path()).unwrap();
        let new = JsonParser.parse(new.path()).unwrap();

        assert_eq!(old.catalog.sections[0].description, "");
        assert_eq!(new.catalog.sections[0].description, "描述");
    }

    #[test]
    fn newer_formats_are_refused() {
        let dir = knowledge_base(r#"{"sections":[]}"#, &[]);
        fs::write(dir.path().join(FORMAT_FILE), r#"{"format":999}"#).unwrap();

        let problems = problems(JsonParser.parse(dir.path()));

        assert!(problems[0].contains("请升级"), "{problems:?}");

        fs::write(dir.path().join(FORMAT_FILE), r#"{"format":1}"#).unwrap();
        assert!(JsonParser.parse(dir.path()).is_ok());
    }

    #[test]
    fn related_defaults_to_empty() {
        let a = node_json("a");
        let dir = knowledge_base(CATALOG, &[("a.json", &a)]);

        let parsed = JsonParser.parse(dir.path()).unwrap();

        assert!(parsed.nodes[0].related.is_empty());
    }

    #[test]
    fn reports_every_bad_file_at_once() {
        let other = node_json("别的地址");
        let dir = knowledge_base("不是 JSON", &[("bad.json", "{"), ("mismatch.json", &other)]);

        let problems = problems(JsonParser.parse(dir.path()));

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("catalog.json"));
        assert!(problems[1].contains("bad.json"));
        assert!(problems[2].contains("mismatch.json") && problems[2].contains("别的地址"));
    }

    #[test]
    fn missing_files_are_reported() {
        let dir = TempDir::new().unwrap();

        let problems = problems(JsonParser.parse(dir.path()));

        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("catalog.json"));
        assert!(problems[1].contains("nodes"));
    }

    #[test]
    fn parsing_does_not_need_the_sources() {
        let text = "一段原文";
        let node = format!(
            r#"{{"id":"a","title":"T","description":"D","evidence":[{{"type":"source","source":"{}","locator":{{"type":"text_range","start":0,"end":3}}}}]}}"#,
            source_id(text.as_bytes())
        );
        let dir = knowledge_base(CATALOG, &[("a.json", &node)]);

        // 只有解析时来源文件不存在也没关系，校验时才报告。
        let parsed = JsonParser.parse(dir.path()).unwrap();
        assert_eq!(
            parsed.nodes[0].evidence,
            [Evidence::Source {
                source: source_id(text.as_bytes()),
                locator: Locator::TextRange { start: 0, end: 3 },
            }]
        );
        let sources = FileSources::new(dir.path());
        assert!(sources.size(&source_id(text.as_bytes())).unwrap().is_none());
        assert!(matches!(
            Memory::from_parsed(parsed, &sources),
            Err(Error::InvalidMemory(_))
        ));
    }
}
