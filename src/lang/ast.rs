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
    /// 要存入的新原文。
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Description,
}

/// 脚本里写的一段文字，记着位置，报错时用。板块的名字就是这种。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub span: Span,
}

/// `cite` 引用的原文：新存一段，或者已有原文的一段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Citation {
    Text(Label),
    Source(SourceRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    CatalogShow,
    CatalogCreate {
        name: Label,
        description: Label,
    },
    CatalogDescribe {
        name: Label,
        description: Label,
    },
    CatalogRename {
        name: Label,
        new_name: Label,
    },
    CatalogDelete(Label),
    CatalogAdd {
        id: Name,
        section: Label,
    },
    CatalogRemove {
        id: Name,
        section: Label,
    },
    Check,
    Export(Label),
    Cite {
        id: Name,
        citation: Citation,
    },
    Uncite {
        id: Name,
        source: SourceRef,
    },
    Show(Name),
    Links {
        id: Name,
        depth: usize,
        limit: usize,
        offset: usize,
    },
    Evidence(Name),
    BasedBy(Name),
    RelatedBy(Name),
    FullSource {
        id: Name,
        offset: usize,
        limit: usize,
    },
    Add(Box<AddNode>),
    Source(Box<AddNode>),
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
            Statement::CatalogShow
                | Statement::Check
                | Statement::Show(_)
                | Statement::Links { .. }
                | Statement::Evidence(_)
                | Statement::BasedBy(_)
                | Statement::RelatedBy(_)
                | Statement::FullSource { .. }
                | Statement::Export(_)
        )
    }

    /// 只读会话能不能执行。`export` 不改图，不写日志，但会写磁盘，所以也要写权限。
    pub fn needs_write_session(&self) -> bool {
        self.writes() || matches!(self, Statement::Export(_))
    }
}

/// 一条语句和它开始的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub statement: Statement,
    pub span: Span,
}
