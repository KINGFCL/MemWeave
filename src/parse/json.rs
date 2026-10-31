use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use super::{ParsedKnowledge, Parser};
use crate::error::Error;
use crate::knowledge::{Catalog, Node};

/// JSON 目录格式：`catalog.json` 加 `nodes/<节点地址>.json`。
///
/// `nodes` 目录只读取第一层的 `.json` 文件，其他文件会被忽略。
pub struct JsonParser;

impl Parser for JsonParser {
    fn parse(&self, source: &Path) -> Result<ParsedKnowledge, Error> {
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
    use crate::memory::Memory;

    const CATALOG: &str = r#"{"sections":[{"name":"板块","nodes":[{"id":"a","title":"A"}]}]}"#;

    fn node_json(id: &str) -> String {
        format!(r#"{{"id":"{id}","title":"T","raw":"R","understanding":"U"}}"#)
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
        let source = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo-kb");

        let memory = Memory::load(&JsonParser, Path::new(source)).unwrap();

        // drop 不在总表里，只能沿关联到达。
        assert_eq!(memory.node_count(), 4);
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
}
