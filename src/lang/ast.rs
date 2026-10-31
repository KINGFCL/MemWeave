//! 语法树。

use super::diagnostic::Span;

/// 脚本里写的一个节点地址，记着位置，报错时用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

/// `add` 里的一条来源证据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    pub source: Name,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddNode {
    pub id: Name,
    pub title: Option<String>,
    pub description: Option<String>,
    pub sources: Vec<SourceRef>,
    pub based_on: Vec<Name>,
    pub related: Vec<Name>,
    pub related_by: Vec<Name>,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Description,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Catalog,
    Check,
    Show(Name),
    Links {
        id: Name,
        depth: usize,
        limit: usize,
    },
    Evidence(Name),
    BasedBy(Name),
    Add(Box<AddNode>),
    Remove(Name),
    Link {
        from: Name,
        to: Name,
    },
    Unlink {
        from: Name,
        to: Name,
    },
    Set {
        id: Name,
        field: Field,
        value: String,
        value_span: Span,
    },
    Base {
        id: Name,
        on: Name,
    },
    Unbase {
        id: Name,
        on: Name,
    },
}

impl Statement {
    pub fn writes(&self) -> bool {
        !matches!(
            self,
            Statement::Catalog
                | Statement::Check
                | Statement::Show(_)
                | Statement::Links { .. }
                | Statement::Evidence(_)
                | Statement::BasedBy(_)
        )
    }
}

/// 一条语句和它开始的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub statement: Statement,
    pub span: Span,
}
