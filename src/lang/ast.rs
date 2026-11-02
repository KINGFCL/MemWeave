//! 语法树。

use super::diagnostic::Span;
use crate::board::BoardOp;

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

/// 读语句读哪个版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// 当前草稿。没有草稿时就是已保存的版本。
    Revision,
    /// 已保存的版本。
    Origin,
}

impl Version {
    pub fn keyword(self) -> &'static str {
        match self {
            Version::Revision => "revision",
            Version::Origin => "origin",
        }
    }
}

/// 用什么方式看未保存的修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionView {
    /// `revision display`：有没有未保存的修改、改了哪些，不带内容。
    Brief,
    /// `revision display-detail`：改了什么，新节点显示全部内容。
    Detail,
    /// `comparison display`：已保存的版本和草稿逐项对照。
    Comparison,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    /// `revision show a` 或 `origin show a`。里面是一条读语句或 `export`。
    Versioned(Version, Box<Statement>),
    /// 看未保存的修改。可以只看一个节点。
    RevisionDisplay {
        view: RevisionView,
        node: Option<Name>,
    },
    /// 把草稿写进日志。
    Save,
    /// 放弃草稿。
    Discard,
    // 下面这些读语句必须写在 `Versioned` 里。旧日志里还有不带前缀的写法，所以语法上接受，
    // 重放时读语句本来就跳过；新脚本里出现时检查会报错。
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
    Board(BoardOp),
    Log(Label),
    Timeline {
        limit: usize,
        offset: usize,
        node: Option<Name>,
    },
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
            Statement::Versioned(..)
                | Statement::RevisionDisplay { .. }
                | Statement::Save
                | Statement::Discard
                | Statement::CatalogShow
                | Statement::Check
                | Statement::Show(_)
                | Statement::Links { .. }
                | Statement::Evidence(_)
                | Statement::BasedBy(_)
                | Statement::RelatedBy(_)
                | Statement::FullSource { .. }
                | Statement::Export(_)
                | Statement::Timeline { .. }
        )
    }

    /// 写语句提到的节点。`add` 和 `source` 要新建的地址也算在内。
    pub fn written_nodes(&self) -> Vec<&Name> {
        match self {
            Statement::Add(add) | Statement::Source(add) => std::iter::once(&add.id)
                .chain(&add.based_on)
                .chain(&add.related)
                .chain(&add.related_by)
                .collect(),
            Statement::CatalogAdd { id, .. }
            | Statement::CatalogRemove { id, .. }
            | Statement::Cite { id, .. }
            | Statement::Uncite { id, .. }
            | Statement::Remove(id)
            | Statement::Set { id, .. } => vec![id],
            Statement::Link { from, to } | Statement::Unlink { from, to } => vec![from, to],
            Statement::Base { id, on } | Statement::Unbase { id, on } => vec![id, on],
            _ => Vec::new(),
        }
    }

    /// 写语句提到的板块，包括要新建和改成的名字。
    pub fn written_sections(&self) -> Vec<&Label> {
        match self {
            Statement::CatalogCreate { name, .. }
            | Statement::CatalogDescribe { name, .. }
            | Statement::CatalogDelete(name) => vec![name],
            Statement::CatalogRename { name, new_name } => vec![name, new_name],
            Statement::CatalogAdd { section, .. } | Statement::CatalogRemove { section, .. } => {
                vec![section]
            }
            _ => Vec::new(),
        }
    }

    /// 只读会话能不能执行。`export` 不改图，不写日志，但会写磁盘，所以也要写权限。
    pub fn needs_write_session(&self) -> bool {
        match self {
            Statement::Versioned(_, inner) => inner.needs_write_session(),
            Statement::Export(_) | Statement::Save | Statement::Discard => true,
            _ => self.writes(),
        }
    }

    /// 不带版本前缀的读语句。新脚本不能这样写。
    pub fn is_bare_read(&self) -> bool {
        matches!(
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
                | Statement::Timeline { .. }
        )
    }
}

/// 一条语句和它开始的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub statement: Statement,
    pub span: Span,
}
