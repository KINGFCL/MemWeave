use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{Exported, Exporter, check_target};
use crate::error::Error;
use crate::knowledge::Evidence;
use crate::memory::{Memory, Problem};
use crate::parse::Parser;
use crate::parse::json::JsonParser;
use crate::source::SourceStore;
use crate::source::file::FileSources;
use crate::timeline;

/// 写成 [`JsonParser`] 读取的 JSON 目录格式。
///
/// 先写到旁边的临时目录，校验通过后才改名，所以目标目录要么不存在，要么是完整的知识库。
/// 反向列表不写，加载时重新生成，所以它们的顺序可能和导出前不同。
pub struct JsonExporter;

impl Exporter for JsonExporter {
    fn export(
        &self,
        memory: &Memory,
        sources: &dyn SourceStore,
        target: &Path,
    ) -> Result<Exported, Error> {
        check_target(target)?;
        let lost = memory.unreachable();
        if !lost.is_empty() {
            let problems = lost.into_iter().map(|id| Problem::Unreachable { id });
            return Err(Error::InvalidMemory(problems.collect()));
        }

        let temporary = temporary_dir(target)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| failed(parent, &e))?;
        }
        // 临时目录已经存在时这里会报错，不会动别人的东西。
        fs::create_dir(&temporary).map_err(|e| failed(&temporary, &e))?;

        let result = write_knowledge_base(memory, sources, &temporary)
            .and_then(|exported| move_into_place(&temporary, target).map(|()| exported));
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}

fn temporary_dir(target: &Path) -> Result<PathBuf, Error> {
    let name = target
        .file_name()
        .ok_or_else(|| Error::Save(format!("{} 不是可以新建的目录名", target.display())))?;
    let mut temporary = OsString::from(name);
    temporary.push(format!(".export-{}", std::process::id()));
    Ok(target.with_file_name(temporary))
}

fn write_knowledge_base(
    memory: &Memory,
    sources: &dyn SourceStore,
    dir: &Path,
) -> Result<Exported, Error> {
    let nodes_dir = dir.join("nodes");
    fs::create_dir(&nodes_dir).map_err(|e| failed(&nodes_dir, &e))?;

    let mut cited = BTreeSet::new();
    for node in memory.nodes() {
        let node = node.to_node();
        for evidence in &node.evidence {
            if let Evidence::Source { source, .. } = evidence {
                cited.insert(source.clone());
            }
        }
        write_json(&nodes_dir.join(format!("{}.json", node.id)), &node)?;
    }

    let exported_sources = FileSources::new(dir);
    for id in &cited {
        exported_sources.put(&sources.read_all(id)?)?;
    }
    write_json(&dir.join("catalog.json"), memory.catalog())?;
    timeline::write(memory.timeline(), dir)?;

    let parsed = JsonParser.parse(dir)?;
    let problems = Memory::validate(&parsed, &exported_sources);
    if !problems.is_empty() {
        return Err(Error::InvalidMemory(problems));
    }
    Ok(Exported {
        nodes: parsed.nodes.len(),
        sections: parsed.catalog.sections.len(),
        sources: cited.len(),
    })
}

fn move_into_place(temporary: &Path, target: &Path) -> Result<(), Error> {
    // 目标可能是空目录。remove_dir 只能删除空目录，不会丢内容。
    if target.exists() {
        fs::remove_dir(target).map_err(|e| failed(target, &e))?;
    }
    fs::rename(temporary, target).map_err(|e| failed(target, &e))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Save(format!("{}：{e}", path.display())))?;
    text.push('\n');
    fs::write(path, text).map_err(|e| failed(path, &e))
}

fn failed(path: &Path, error: &std::io::Error) -> Error {
    Error::Save(format!("{}：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::knowledge::Catalog;
    use crate::parse::ParsedKnowledge;
    use crate::source::in_memory::InMemorySources;

    fn demo_path() -> &'static Path {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo-kb"))
    }

    fn load_demo() -> Memory {
        Memory::load(&JsonParser, &FileSources::new(demo_path()), demo_path()).unwrap()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn exported_memory_loads_back_unchanged() {
        let original = load_demo();
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("新目录/知识库");
        let demo = FileSources::new(demo_path());

        let exported = JsonExporter.export(&original, &demo, &target).unwrap();

        assert_eq!(
            exported,
            Exported {
                nodes: 4,
                sections: 2,
                sources: 1
            }
        );
        let target_sources = FileSources::new(&target);
        let loaded = Memory::load(&JsonParser, &target_sources, &target).unwrap();
        assert_eq!(loaded.catalog(), original.catalog());
        assert_eq!(loaded.node_count(), original.node_count());
        for node in original.nodes() {
            assert_eq!(loaded.node(node.id()).unwrap().to_node(), node.to_node());
            assert_eq!(
                loaded.evidence(node.id(), &target_sources).unwrap(),
                original.evidence(node.id(), &demo).unwrap()
            );
        }
        // 没有留下临时目录。
        assert_eq!(entries(&dir.path().join("新目录")), ["知识库"]);
    }

    #[test]
    fn empty_memory_can_be_exported() {
        let parsed = ParsedKnowledge {
            catalog: Catalog {
                goals: Vec::new(),
                sections: Vec::new(),
            },
            nodes: Vec::new(),
        };
        let sources = InMemorySources::new();
        let memory = Memory::from_parsed(parsed, &sources).unwrap();
        let dir = TempDir::new().unwrap();

        JsonExporter.export(&memory, &sources, dir.path()).unwrap();

        let target_sources = FileSources::new(dir.path());
        let loaded = Memory::load(&JsonParser, &target_sources, dir.path()).unwrap();
        assert_eq!(loaded.node_count(), 0);
    }

    #[test]
    fn refuses_to_overwrite() {
        let memory = load_demo();
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("已有文件"), "内容").unwrap();

        let result = JsonExporter.export(&memory, &FileSources::new(demo_path()), dir.path());

        assert!(matches!(result, Err(Error::Save(_))));
        assert_eq!(entries(dir.path()), ["已有文件"]);
    }

    #[test]
    fn a_path_without_a_directory_name_is_rejected() {
        let memory = load_demo();
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("不存在/..");

        let result = JsonExporter.export(&memory, &FileSources::new(demo_path()), &target);

        assert!(matches!(result, Err(Error::Save(_))));
        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn refuses_nodes_that_could_not_be_loaded_back() {
        let mut memory = load_demo();
        memory.unlink("ownership", "drop").unwrap();
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("知识库");

        let result = JsonExporter.export(&memory, &FileSources::new(demo_path()), &target);

        let Err(Error::InvalidMemory(problems)) = result else {
            panic!("应该拒绝导出");
        };
        assert_eq!(problems, [Problem::Unreachable { id: "drop".into() }]);
        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_source_leaves_nothing_behind() {
        let memory = load_demo();
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("知识库");

        let result = JsonExporter.export(&memory, &InMemorySources::new(), &target);

        assert!(matches!(result, Err(Error::Source(_))));
        assert!(entries(dir.path()).is_empty());
    }
}
