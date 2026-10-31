use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Stage};
use crate::knowledge::{KnowledgeTree, NodeId, TreeNode};
use crate::memory::{Memory, NodeRef};
use crate::model::{Model, Prompt, generate_json};
use crate::prompt;

/// 下一层的一个候选。`sources` 是把它引入这一层的已保留节点，入口节点为空。
struct Pending<'m> {
    node: NodeRef<'m>,
    sources: Vec<NodeRef<'m>>,
}

/// 已保留的节点在树上的位置。
#[derive(Default)]
struct Kept<'m> {
    children: Vec<NodeRef<'m>>,
    references: Vec<NodeRef<'m>>,
}

/// 从总表出发逐层检索，返回知识树。
///
/// 遍历时节点之间直接用地址相连。只有模型的回答是文本地址，要在总表和这一层的候选里对应回节点。
/// 每个节点在一次查询中只读取、判断一次，被丢弃的节点同样算已访问。
pub fn retrieve(memory: &Memory, model: &dyn Model, query: &str) -> Result<KnowledgeTree, Error> {
    let entries = select_entries(memory, model, query)?;

    let mut visited: HashSet<NodeRef<'_>> = entries.iter().copied().collect();
    let mut pending: Vec<Pending<'_>> = entries
        .into_iter()
        .map(|node| Pending {
            node,
            sources: Vec::new(),
        })
        .collect();
    let mut kept: HashMap<NodeRef<'_>, Kept<'_>> = HashMap::new();
    let mut kept_order: Vec<NodeRef<'_>> = Vec::new();
    let mut roots: Vec<NodeRef<'_>> = Vec::new();
    let mut rejected: Vec<NodeId> = Vec::new();
    let mut layer = 0;

    while !pending.is_empty() {
        layer += 1;
        let chosen = judge_layer(model, query, Stage::Layer(layer), &pending, &kept_order)?;

        let first_kept = kept_order.len();
        for candidate in pending {
            let node = candidate.node;
            if !chosen.contains(node.id()) {
                rejected.push(node.id().to_owned());
                continue;
            }
            match candidate.sources.split_first() {
                None => roots.push(node),
                Some((parent, others)) => {
                    kept.get_mut(parent)
                        .expect("来源必已保留")
                        .children
                        .push(node);
                    for other in others {
                        kept.get_mut(other)
                            .expect("来源必已保留")
                            .references
                            .push(node);
                    }
                }
            }
            kept.insert(node, Kept::default());
            kept_order.push(node);
        }

        pending = expand(&kept_order[first_kept..], &mut kept, &mut visited);
    }

    let roots = roots.iter().map(|node| build_tree(*node, &kept)).collect();
    Ok(KnowledgeTree {
        query: query.to_owned(),
        roots,
        rejected,
    })
}

fn select_entries<'m>(
    memory: &'m Memory,
    model: &dyn Model,
    query: &str,
) -> Result<Vec<NodeRef<'m>>, Error> {
    let catalog = memory.catalog();
    let known: HashSet<&str> = catalog
        .sections
        .iter()
        .flat_map(|s| s.nodes.iter().map(|e| e.id.as_str()))
        .collect();
    if known.is_empty() {
        return Ok(Vec::new());
    }

    let stage = Stage::Catalog;
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for id in ask(model, stage, &prompt::select_entries(query, catalog))? {
        if !known.contains(id.as_str()) {
            return Err(Error::InvalidChoice { stage, id });
        }
        if seen.insert(id.clone()) {
            // 校验过的知识库里，总表的代表节点一定存在。
            entries.push(memory.node(&id).expect("总表的代表节点必定存在"));
        }
    }
    Ok(entries)
}

fn judge_layer(
    model: &dyn Model,
    query: &str,
    stage: Stage,
    pending: &[Pending<'_>],
    kept_order: &[NodeRef<'_>],
) -> Result<HashSet<NodeId>, Error> {
    let candidates: Vec<prompt::Candidate<'_>> = pending
        .iter()
        .map(|p| prompt::Candidate {
            node: p.node,
            sources: &p.sources,
        })
        .collect();
    let prompt = prompt::judge_layer(query, kept_order, &candidates);

    let chosen: HashSet<NodeId> = ask(model, stage, &prompt)?.into_iter().collect();
    let candidate_ids: HashSet<&str> = pending.iter().map(|p| p.node.id()).collect();
    match chosen
        .iter()
        .find(|id| !candidate_ids.contains(id.as_str()))
    {
        Some(id) => Err(Error::InvalidChoice {
            stage,
            id: id.clone(),
        }),
        None => Ok(chosen),
    }
}

/// 让模型选择节点。
fn ask(model: &dyn Model, stage: Stage, prompt: &Prompt) -> Result<Vec<NodeId>, Error> {
    #[derive(Deserialize)]
    struct Choice {
        ids: Vec<NodeId>,
    }

    let choice: Choice = generate_json(model, stage, prompt, &choice_schema())?;
    Ok(choice.ids)
}

fn choice_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "ids": { "type": "array", "items": { "type": "string" } } },
        "required": ["ids"],
        "additionalProperties": false,
    })
}

/// 收集这一层保留节点的关联，得到下一层候选。
/// 遇到已保留的节点只记引用，遇到已丢弃的节点直接忽略。
fn expand<'m>(
    layer_kept: &[NodeRef<'m>],
    kept: &mut HashMap<NodeRef<'m>, Kept<'m>>,
    visited: &mut HashSet<NodeRef<'m>>,
) -> Vec<Pending<'m>> {
    let mut next: Vec<Pending<'m>> = Vec::new();
    let mut next_index: HashMap<NodeRef<'m>, usize> = HashMap::new();

    for &node in layer_kept {
        for related in node.related() {
            if related == node {
                continue;
            }
            if let Some(&i) = next_index.get(&related) {
                let sources = &mut next[i].sources;
                if !sources.contains(&node) {
                    sources.push(node);
                }
            } else if visited.insert(related) {
                next_index.insert(related, next.len());
                next.push(Pending {
                    node: related,
                    sources: vec![node],
                });
            } else if kept.contains_key(&related) {
                let references = &mut kept.get_mut(&node).expect("来自已保留节点").references;
                if !references.contains(&related) {
                    references.push(related);
                }
            }
        }
    }
    next
}

fn build_tree(node: NodeRef<'_>, kept: &HashMap<NodeRef<'_>, Kept<'_>>) -> TreeNode {
    let entry = &kept[&node];
    TreeNode {
        id: node.id().to_owned(),
        title: node.title().to_owned(),
        description: node.description().to_owned(),
        evidence: node.evidence(),
        children: entry
            .children
            .iter()
            .map(|child| build_tree(*child, kept))
            .collect(),
        references: entry
            .references
            .iter()
            .map(|reference| reference.id().to_owned())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::knowledge::{Catalog, CatalogEntry, Evidence, Locator, Node, Section};
    use crate::parse::ParsedKnowledge;
    use crate::source::in_memory::InMemorySources;

    /// `graph` 的每一项是（节点，相关节点），`entries` 是总表的代表节点。
    fn memory(entries: &[&str], graph: &[(&str, &[&str])]) -> Memory {
        memory_with(entries, graph, |_| {})
    }

    fn memory_with(
        entries: &[&str],
        graph: &[(&str, &[&str])],
        change: impl Fn(&mut Node),
    ) -> Memory {
        let mut sources = InMemorySources::new();
        let source = sources.insert(b"text".to_vec());
        let nodes = graph
            .iter()
            .map(|(id, related)| {
                let mut node = Node {
                    id: id.to_string(),
                    title: format!("标题{id}"),
                    description: format!("描述{id}"),
                    evidence: vec![Evidence::Source {
                        source: source.clone(),
                        locator: Locator::TextRange { start: 0, end: 4 },
                    }],
                    related: related.iter().map(|r| r.to_string()).collect(),
                };
                change(&mut node);
                node
            })
            .collect();
        let entries = entries
            .iter()
            .map(|id| CatalogEntry {
                id: id.to_string(),
                title: format!("标题{id}"),
            })
            .collect();
        let catalog = Catalog {
            sections: vec![Section {
                name: "板块".into(),
                description: String::new(),
                nodes: entries,
            }],
        };
        Memory::from_parsed(ParsedKnowledge { catalog, nodes }, &sources).unwrap()
    }

    /// 依次返回预设答案，并记录收到的每个请求。
    struct ScriptedModel {
        answers: RefCell<VecDeque<Vec<&'static str>>>,
        prompts: RefCell<Vec<Prompt>>,
        limit: usize,
    }

    impl ScriptedModel {
        fn new(answers: &[&[&'static str]]) -> Self {
            Self {
                answers: RefCell::new(answers.iter().map(|a| a.to_vec()).collect()),
                prompts: RefCell::new(Vec::new()),
                limit: usize::MAX,
            }
        }

        /// 第 `call` 次调用（从 0 开始）的用户消息。
        fn request(&self, call: usize) -> Value {
            serde_json::from_str(&self.prompts.borrow()[call].user).unwrap()
        }

        fn candidate_ids(&self, call: usize) -> Vec<String> {
            self.request(call)["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["id"].as_str().unwrap().to_owned())
                .collect()
        }

        fn calls(&self) -> usize {
            self.prompts.borrow().len()
        }
    }

    impl Model for ScriptedModel {
        fn context_limit(&self) -> usize {
            self.limit
        }

        fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error> {
            Ok(prompt.user.chars().count())
        }

        fn generate(&self, prompt: &Prompt, _schema: &Value) -> Result<String, Error> {
            self.prompts.borrow_mut().push(prompt.clone());
            let answer = self
                .answers
                .borrow_mut()
                .pop_front()
                .expect("模型调用次数超出预期");
            Ok(json!({ "ids": answer }).to_string())
        }
    }

    fn child_ids(node: &TreeNode) -> Vec<&str> {
        node.children.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn crawls_layer_by_layer_and_builds_tree() {
        let memory = memory(
            &["a", "b"],
            &[
                ("a", &["c", "d"]),
                ("b", &[]),
                ("c", &["e"]),
                ("d", &["f"]),
                ("e", &[]),
                ("f", &[]),
            ],
        );
        let model = ScriptedModel::new(&[&["a", "b"], &["a"], &["c"], &["e"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        // 总表一次，三层各一次：每层只调用一次模型。
        assert_eq!(model.calls(), 4);
        assert_eq!(model.candidate_ids(1), ["a", "b"]);
        assert_eq!(model.candidate_ids(2), ["c", "d"]);
        assert_eq!(model.candidate_ids(3), ["e"]);

        assert_eq!(tree.roots.len(), 1);
        let a = &tree.roots[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.description, "描述a");
        assert_eq!(a.evidence.len(), 1);
        assert_eq!(child_ids(a), ["c"]);
        assert_eq!(child_ids(&a.children[0]), ["e"]);
        // d 被丢弃后不再往下爬，f 从未被读取。
        assert_eq!(tree.rejected, ["b", "d"]);
    }

    #[test]
    fn candidates_are_sent_in_full() {
        let memory = memory(&["a"], &[("a", &["b"]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &[]]);

        retrieve(&memory, &model, "问题").unwrap();

        let request = model.request(2);
        assert_eq!(request["query"], "问题");
        assert_eq!(request["kept"][0]["id"], "a");
        let candidate = &request["candidates"][0];
        assert_eq!(candidate["description"], "描述b");
        assert_eq!(candidate["sources"][0], "a");
        assert_eq!(model.request(1)["candidates"][0]["related"], json!(["b"]));
    }

    #[test]
    fn cycle_terminates_with_reference() {
        let memory = memory(&["a"], &[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &["b"], &["c"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.calls(), 4);
        let a = &tree.roots[0];
        let c = &a.children[0].children[0];
        assert_eq!(c.id, "c");
        assert!(c.children.is_empty());
        assert_eq!(c.references, ["a"]);
    }

    #[test]
    fn rejected_node_is_not_judged_again() {
        // b 在第一层被丢弃；第二层的 c 又指向 b。
        let memory = memory(&["a", "b"], &[("a", &["c"]), ("b", &[]), ("c", &["b"])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a"], &["c"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.calls(), 3);
        assert_eq!(tree.rejected, ["b"]);
        let c = &tree.roots[0].children[0];
        assert!(c.references.is_empty());
    }

    #[test]
    fn shared_candidate_hangs_on_first_source_only() {
        let memory = memory(&["a", "b"], &[("a", &["c"]), ("b", &["c"]), ("c", &[])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a", "b"], &["c"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.candidate_ids(2), ["c"]);
        assert_eq!(
            model.request(2)["candidates"][0]["sources"],
            json!(["a", "b"])
        );
        assert_eq!(child_ids(&tree.roots[0]), ["c"]);
        assert!(tree.roots[1].children.is_empty());
        assert_eq!(tree.roots[1].references, ["c"]);
    }

    #[test]
    fn same_layer_nodes_reference_each_other() {
        let memory = memory(&["a", "b"], &[("a", &["b"]), ("b", &["a"])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a", "b"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.calls(), 2);
        assert_eq!(tree.roots[0].references, ["b"]);
        assert_eq!(tree.roots[1].references, ["a"]);
    }

    #[test]
    fn self_and_duplicate_links_are_ignored() {
        let memory = memory(&["a"], &[("a", &["a", "b", "b"]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &["b"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.request(2)["candidates"][0]["sources"], json!(["a"]));
        assert_eq!(child_ids(&tree.roots[0]), ["b"]);
        assert!(tree.roots[0].references.is_empty());
    }

    #[test]
    fn duplicate_entries_are_deduplicated() {
        let memory = memory(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&["a", "a"], &["a"]]);

        retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.candidate_ids(1), ["a"]);
    }

    #[test]
    fn no_entries_gives_empty_tree() {
        let memory = memory(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&[]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert!(tree.roots.is_empty());
        assert_eq!(model.calls(), 1);
    }

    #[test]
    fn empty_catalog_skips_the_model() {
        let memory = memory(&[], &[]);
        let model = ScriptedModel::new(&[]);

        assert!(retrieve(&memory, &model, "问题").unwrap().roots.is_empty());
        assert_eq!(model.calls(), 0);
    }

    #[test]
    fn overflow_names_the_layer_and_excess() {
        let big = "很长".repeat(1000);
        let memory = memory_with(&["a"], &[("a", &["b"]), ("b", &[])], |node| {
            if node.id == "b" {
                node.description = big.clone();
            }
        });
        let mut model = ScriptedModel::new(&[&["a"], &["a"]]);
        model.limit = 1000;

        let err = retrieve(&memory, &model, "问题").unwrap_err();

        let Error::ContextOverflow {
            stage,
            tokens,
            limit,
        } = err
        else {
            panic!("应为上下文超限：{err}");
        };
        assert_eq!(stage, Stage::Layer(2));
        assert_eq!(limit, 1000);
        assert!(tokens > limit);
        assert_eq!(model.calls(), 2);
        assert!(
            Error::ContextOverflow {
                stage,
                tokens: 1500,
                limit
            }
            .to_string()
            .contains("超出 500")
        );
    }

    #[test]
    fn overflow_on_catalog() {
        let memory = memory(&["a"], &[("a", &[])]);
        let mut model = ScriptedModel::new(&[]);
        model.limit = 1;

        let err = retrieve(&memory, &model, "问题").unwrap_err();

        assert!(matches!(
            err,
            Error::ContextOverflow {
                stage: Stage::Catalog,
                ..
            }
        ));
    }

    #[test]
    fn removed_nodes_are_not_visited() {
        let mut memory = memory(&["a"], &[("a", &["b", "c"]), ("b", &[]), ("c", &[])]);
        memory.remove_node("b").unwrap();
        let model = ScriptedModel::new(&[&["a"], &["a"], &["c"]]);

        let tree = retrieve(&memory, &model, "问题").unwrap();

        assert_eq!(model.candidate_ids(2), ["c"]);
        assert_eq!(child_ids(&tree.roots[0]), ["c"]);
    }

    /// b 存在，但不在总表里：入口只能从总表选。
    #[test]
    fn choosing_outside_catalog_is_an_error() {
        let memory = memory(&["a"], &[("a", &["b"]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["b"]]);

        let err = retrieve(&memory, &model, "问题").unwrap_err();

        assert!(matches!(
            err,
            Error::InvalidChoice { stage: Stage::Catalog, id } if id == "b"
        ));
    }

    #[test]
    fn choosing_outside_candidates_is_an_error() {
        let memory = memory(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["z"]]);

        let err = retrieve(&memory, &model, "问题").unwrap_err();

        assert!(matches!(
            err,
            Error::InvalidChoice { stage: Stage::Layer(1), id } if id == "z"
        ));
    }
}
