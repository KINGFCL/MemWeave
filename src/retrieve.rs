use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Stage};
use crate::knowledge::{KnowledgeTree, Node, NodeId, TreeNode};
use crate::model::{Model, Prompt, generate_json};
use crate::prompt;
use crate::store::Store;

/// 下一层的一个候选。`sources` 是把它引入这一层的已保留节点，入口节点为空。
struct Pending {
    id: NodeId,
    sources: Vec<NodeId>,
}

/// 已保留的节点及其在树上的位置。
struct Kept {
    node: Node,
    children: Vec<NodeId>,
    references: Vec<NodeId>,
}

/// 从总表出发逐层检索，返回知识树。
///
/// 每个节点在一次查询中只读取、判断一次，被丢弃的节点同样算已访问。
pub fn retrieve(
    store: &impl Store,
    model: &dyn Model,
    query: &str,
) -> Result<KnowledgeTree, Error> {
    let entries = select_entries(store, model, query)?;

    let mut visited: HashSet<NodeId> = entries.iter().cloned().collect();
    let mut pending: Vec<Pending> = entries
        .into_iter()
        .map(|id| Pending {
            id,
            sources: Vec::new(),
        })
        .collect();
    let mut kept: HashMap<NodeId, Kept> = HashMap::new();
    let mut kept_order: Vec<NodeId> = Vec::new();
    let mut roots: Vec<NodeId> = Vec::new();
    let mut rejected: Vec<NodeId> = Vec::new();
    let mut layer = 0;

    while !pending.is_empty() {
        layer += 1;
        let nodes = load_layer(store, &pending)?;
        let chosen = judge_layer(
            model,
            query,
            Stage::Layer(layer),
            &pending,
            &nodes,
            &kept_order,
            &kept,
        )?;

        let first_kept = kept_order.len();
        for (candidate, node) in pending.into_iter().zip(nodes) {
            let id = candidate.id;
            if !chosen.contains(&id) {
                rejected.push(id);
                continue;
            }
            match candidate.sources.split_first() {
                None => roots.push(id.clone()),
                Some((parent, others)) => {
                    kept.get_mut(parent)
                        .expect("来源必已保留")
                        .children
                        .push(id.clone());
                    for other in others {
                        kept.get_mut(other)
                            .expect("来源必已保留")
                            .references
                            .push(id.clone());
                    }
                }
            }
            kept.insert(
                id.clone(),
                Kept {
                    node,
                    children: Vec::new(),
                    references: Vec::new(),
                },
            );
            kept_order.push(id);
        }

        pending = expand(&kept_order[first_kept..], &mut kept, &mut visited);
    }

    let roots = roots.iter().map(|id| build_tree(id, &kept)).collect();
    Ok(KnowledgeTree {
        query: query.to_owned(),
        roots,
        rejected,
    })
}

fn select_entries(
    store: &impl Store,
    model: &dyn Model,
    query: &str,
) -> Result<Vec<NodeId>, Error> {
    let catalog = store.catalog()?;
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
    for id in ask(model, stage, &prompt::select_entries(query, &catalog))? {
        if !known.contains(id.as_str()) {
            return Err(Error::InvalidChoice { stage, id });
        }
        if seen.insert(id.clone()) {
            entries.push(id);
        }
    }
    Ok(entries)
}

fn load_layer(store: &impl Store, pending: &[Pending]) -> Result<Vec<Node>, Error> {
    pending
        .iter()
        .map(|p| {
            store.node(&p.id)?.ok_or_else(|| match p.sources.first() {
                Some(referrer) => Error::BrokenLink {
                    id: p.id.clone(),
                    referrer: referrer.clone(),
                },
                None => Error::MissingEntry(p.id.clone()),
            })
        })
        .collect()
}

fn judge_layer(
    model: &dyn Model,
    query: &str,
    stage: Stage,
    pending: &[Pending],
    nodes: &[Node],
    kept_order: &[NodeId],
    kept: &HashMap<NodeId, Kept>,
) -> Result<HashSet<NodeId>, Error> {
    let kept_nodes: Vec<&Node> = kept_order.iter().map(|id| &kept[id].node).collect();
    let candidates: Vec<prompt::Candidate<'_>> = pending
        .iter()
        .zip(nodes)
        .map(|(p, node)| prompt::Candidate {
            node,
            sources: &p.sources,
        })
        .collect();
    let prompt = prompt::judge_layer(query, &kept_nodes, &candidates);

    let chosen: HashSet<NodeId> = ask(model, stage, &prompt)?.into_iter().collect();
    match chosen
        .iter()
        .find(|id| !pending.iter().any(|p| &p.id == *id))
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
fn expand(
    layer_kept: &[NodeId],
    kept: &mut HashMap<NodeId, Kept>,
    visited: &mut HashSet<NodeId>,
) -> Vec<Pending> {
    let mut next: Vec<Pending> = Vec::new();
    let mut next_index: HashMap<NodeId, usize> = HashMap::new();

    for id in layer_kept {
        for related in kept[id].node.related.clone() {
            if &related == id {
                continue;
            }
            if let Some(&i) = next_index.get(&related) {
                let sources = &mut next[i].sources;
                if !sources.contains(id) {
                    sources.push(id.clone());
                }
            } else if visited.insert(related.clone()) {
                next_index.insert(related.clone(), next.len());
                next.push(Pending {
                    id: related,
                    sources: vec![id.clone()],
                });
            } else if kept.contains_key(&related) {
                let references = &mut kept.get_mut(id).expect("来自已保留节点").references;
                if !references.contains(&related) {
                    references.push(related);
                }
            }
        }
    }
    next
}

fn build_tree(id: &str, kept: &HashMap<NodeId, Kept>) -> TreeNode {
    let entry = &kept[id];
    TreeNode {
        id: entry.node.id.clone(),
        title: entry.node.title.clone(),
        description: entry.node.description.clone(),
        evidence: entry.node.evidence.clone(),
        children: entry
            .children
            .iter()
            .map(|child| build_tree(child, kept))
            .collect(),
        references: entry.references.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;
    use crate::knowledge::{Catalog, CatalogEntry, Section};

    struct MemStore {
        entries: Vec<&'static str>,
        nodes: HashMap<String, Node>,
    }

    impl MemStore {
        /// `graph` 的每一项是（节点，相关节点）。
        fn new(entries: &[&'static str], graph: &[(&str, &[&str])]) -> Self {
            let nodes = graph
                .iter()
                .map(|(id, related)| {
                    let node = Node {
                        id: id.to_string(),
                        title: format!("标题{id}"),
                        description: format!("描述{id}"),
                        evidence: Vec::new(),
                        related: related.iter().map(|r| r.to_string()).collect(),
                    };
                    (id.to_string(), node)
                })
                .collect();
            Self {
                entries: entries.to_vec(),
                nodes,
            }
        }
    }

    impl Store for MemStore {
        fn catalog(&self) -> Result<Catalog, Error> {
            let nodes = self
                .entries
                .iter()
                .map(|id| CatalogEntry {
                    id: id.to_string(),
                    title: format!("标题{id}"),
                })
                .collect();
            Ok(Catalog {
                sections: vec![Section {
                    name: "板块".into(),
                    nodes,
                }],
            })
        }

        fn node(&self, id: &str) -> Result<Option<Node>, Error> {
            Ok(self.nodes.get(id).cloned())
        }
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
        let store = MemStore::new(
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

        let tree = retrieve(&store, &model, "问题").unwrap();

        // 总表一次，三层各一次：每层只调用一次模型。
        assert_eq!(model.calls(), 4);
        assert_eq!(model.candidate_ids(1), ["a", "b"]);
        assert_eq!(model.candidate_ids(2), ["c", "d"]);
        assert_eq!(model.candidate_ids(3), ["e"]);

        assert_eq!(tree.roots.len(), 1);
        let a = &tree.roots[0];
        assert_eq!(a.id, "a");
        assert_eq!(a.description, "描述a");
        assert_eq!(child_ids(a), ["c"]);
        assert_eq!(child_ids(&a.children[0]), ["e"]);
        // d 被丢弃后不再往下爬，f 从未被读取。
        assert_eq!(tree.rejected, ["b", "d"]);
    }

    #[test]
    fn candidates_are_sent_in_full() {
        let store = MemStore::new(&["a"], &[("a", &["b"]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &[]]);

        retrieve(&store, &model, "问题").unwrap();

        let request = model.request(2);
        assert_eq!(request["query"], "问题");
        assert_eq!(request["kept"][0]["id"], "a");
        let candidate = &request["candidates"][0];
        assert_eq!(candidate["description"], "描述b");
        assert_eq!(candidate["sources"][0], "a");
    }

    #[test]
    fn cycle_terminates_with_reference() {
        let store = MemStore::new(&["a"], &[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &["b"], &["c"]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

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
        let store = MemStore::new(&["a", "b"], &[("a", &["c"]), ("b", &[]), ("c", &["b"])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a"], &["c"]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

        assert_eq!(model.calls(), 3);
        assert_eq!(tree.rejected, ["b"]);
        let c = &tree.roots[0].children[0];
        assert!(c.references.is_empty());
    }

    #[test]
    fn shared_candidate_hangs_on_first_source_only() {
        let store = MemStore::new(&["a", "b"], &[("a", &["c"]), ("b", &["c"]), ("c", &[])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a", "b"], &["c"]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

        assert_eq!(model.candidate_ids(2), ["c"]);
        assert_eq!(
            model.request(2)["candidates"][0]["sources"],
            serde_json::json!(["a", "b"])
        );
        assert_eq!(child_ids(&tree.roots[0]), ["c"]);
        assert!(tree.roots[1].children.is_empty());
        assert_eq!(tree.roots[1].references, ["c"]);
    }

    #[test]
    fn same_layer_nodes_reference_each_other() {
        let store = MemStore::new(&["a", "b"], &[("a", &["b"]), ("b", &["a"])]);
        let model = ScriptedModel::new(&[&["a", "b"], &["a", "b"]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

        assert_eq!(model.calls(), 2);
        assert_eq!(tree.roots[0].references, ["b"]);
        assert_eq!(tree.roots[1].references, ["a"]);
    }

    #[test]
    fn self_and_duplicate_links_are_ignored() {
        let store = MemStore::new(&["a"], &[("a", &["a", "b", "b"]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["a"], &["b"]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

        assert_eq!(
            model.request(2)["candidates"][0]["sources"],
            serde_json::json!(["a"])
        );
        assert_eq!(child_ids(&tree.roots[0]), ["b"]);
        assert!(tree.roots[0].references.is_empty());
    }

    #[test]
    fn duplicate_entries_are_deduplicated() {
        let store = MemStore::new(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&["a", "a"], &["a"]]);

        retrieve(&store, &model, "问题").unwrap();

        assert_eq!(model.candidate_ids(1), ["a"]);
    }

    #[test]
    fn no_entries_gives_empty_tree() {
        let store = MemStore::new(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&[]]);

        let tree = retrieve(&store, &model, "问题").unwrap();

        assert!(tree.roots.is_empty());
        assert_eq!(model.calls(), 1);
    }

    #[test]
    fn empty_catalog_skips_the_model() {
        let store = MemStore::new(&[], &[]);
        let model = ScriptedModel::new(&[]);

        assert!(retrieve(&store, &model, "问题").unwrap().roots.is_empty());
        assert_eq!(model.calls(), 0);
    }

    #[test]
    fn overflow_names_the_layer_and_excess() {
        let big = "很长".repeat(1000);
        let mut store = MemStore::new(&["a"], &[("a", &["b"]), ("b", &[])]);
        store.nodes.get_mut("b").unwrap().description = big;
        let mut model = ScriptedModel::new(&[&["a"], &["a"]]);
        model.limit = 1000;

        let err = retrieve(&store, &model, "问题").unwrap_err();

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
        let store = MemStore::new(&["a"], &[("a", &[])]);
        let mut model = ScriptedModel::new(&[]);
        model.limit = 1;

        let err = retrieve(&store, &model, "问题").unwrap_err();

        assert!(matches!(
            err,
            Error::ContextOverflow {
                stage: Stage::Catalog,
                ..
            }
        ));
    }

    #[test]
    fn broken_link_names_the_referrer() {
        let store = MemStore::new(&["a"], &[("a", &["ghost"])]);
        let model = ScriptedModel::new(&[&["a"], &["a"]]);

        let err = retrieve(&store, &model, "问题").unwrap_err();

        assert!(
            matches!(&err, Error::BrokenLink { id, referrer } if id == "ghost" && referrer == "a")
        );
    }

    #[test]
    fn catalog_entry_without_node_is_an_error() {
        let store = MemStore::new(&["a"], &[]);
        let model = ScriptedModel::new(&[&["a"]]);

        let err = retrieve(&store, &model, "问题").unwrap_err();

        assert!(matches!(err, Error::MissingEntry(id) if id == "a"));
    }

    #[test]
    fn choosing_outside_catalog_is_an_error() {
        let store = MemStore::new(&["a"], &[("a", &[]), ("b", &[])]);
        let model = ScriptedModel::new(&[&["b"]]);

        let err = retrieve(&store, &model, "问题").unwrap_err();

        assert!(matches!(err, Error::InvalidChoice { stage: Stage::Catalog, id } if id == "b"));
    }

    #[test]
    fn choosing_outside_candidates_is_an_error() {
        let store = MemStore::new(&["a"], &[("a", &[])]);
        let model = ScriptedModel::new(&[&["a"], &["z"]]);

        let err = retrieve(&store, &model, "问题").unwrap_err();

        assert!(matches!(err, Error::InvalidChoice { stage: Stage::Layer(1), id } if id == "z"));
    }
}
