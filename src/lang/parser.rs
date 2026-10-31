//! 语法分析：把词组成语句。出错后跳到下一行继续，这样一次能报告所有语法错误。

use crate::board::BoardOp;

use super::ast::{AddNode, Citation, Field, Label, Located, Name, SourceRef, Statement};
use super::diagnostic::{Diagnostic, Span, closest};
use super::lexer::{Token, TokenKind};

pub(crate) const KEYWORDS: [&str; 23] = [
    "catalog",
    "check",
    "export",
    "show",
    "links",
    "evidence",
    "based_by",
    "related_by",
    "full_source",
    "add",
    "source",
    "remove",
    "link",
    "unlink",
    "set",
    "base",
    "unbase",
    "cite",
    "uncite",
    "goal",
    "step",
    "log",
    "timeline",
];
const ADD_CLAUSES: [&str; 8] = [
    "title",
    "desc",
    "based_on",
    "text",
    "source",
    "related",
    "related_by",
    "end",
];
const SOURCE_CLAUSES: [&str; 7] = [
    "title",
    "desc",
    "text",
    "source",
    "related",
    "related_by",
    "end",
];
/// 写这些的人想搜索，但这个系统靠走图回忆。
const SEARCH_WORDS: [&str; 7] = ["search", "find", "grep", "list", "ls", "query", "lookup"];
const CATALOG_COMMANDS: [&str; 7] = [
    "show", "create", "describe", "rename", "delete", "add", "remove",
];
/// `full_source` 每个文件默认和最多显示多少字节。最大值小于输出上限，所以总能看到下一段的位置。
const DEFAULT_SOURCE_LIMIT: usize = 64 * 1024;
const MAX_SOURCE_LIMIT: usize = 128 * 1024;
const DEFAULT_LINK_LIMIT: usize = 50;
const MAX_LINK_LIMIT: usize = 1000;
const MAX_LINK_DEPTH: usize = 5;
const DEFAULT_TIMELINE_LIMIT: usize = 50;
const MAX_TIMELINE_LIMIT: usize = 1000;
const GOAL_COMMANDS: [&str; 3] = ["create", "done", "drop"];
const STEP_COMMANDS: [&str; 5] = ["add", "start", "done", "block", "remove"];

pub fn parse(tokens: Vec<Token>) -> (Vec<Located>, Vec<Diagnostic>) {
    let mut parser = Parser {
        tokens,
        position: 0,
        errors: Vec::new(),
        extra: Vec::new(),
    };
    let statements = parser.script();
    (statements, parser.errors)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    errors: Vec<Diagnostic>,
    /// `show a, b` 的第二条及之后的语句，等这一行确认无误后才算数。
    extra: Vec<Located>,
}

/// 这一行已经报过错，跳过它。
struct Skip;

impl Parser {
    fn script(&mut self) -> Vec<Located> {
        let mut statements = Vec::new();
        loop {
            self.skip_newlines();
            let Some(token) = self.peek().cloned() else {
                return statements;
            };
            match self.statement(&token) {
                Ok(statement) => {
                    statements.push(Located {
                        statement,
                        span: token.span,
                    });
                    statements.append(&mut self.extra);
                }
                Err(Skip) => {
                    self.extra.clear();
                    self.skip_line();
                }
            }
        }
    }

    fn statement(&mut self, token: &Token) -> Result<Statement, Skip> {
        let TokenKind::Word(keyword) = &token.kind else {
            return self.fail(Diagnostic::new(
                "E101",
                token.span,
                "每条语句都要以指令开头",
            ));
        };
        let keyword = keyword.clone();
        self.position += 1;
        let statement = match keyword.as_str() {
            "catalog" => self.catalog(token)?,
            "export" => Statement::Export(self.label("导出路径")?),
            "check" => Statement::Check,
            "show" => self.for_each_name(Statement::Show)?,
            "evidence" => self.for_each_name(Statement::Evidence)?,
            "based_by" => self.for_each_name(Statement::BasedBy)?,
            "related_by" => self.for_each_name(Statement::RelatedBy)?,
            "remove" => Statement::Remove(self.name("节点地址")?),
            "links" => self.links()?,
            "link" => {
                let (from, to) = self.arrow()?;
                Statement::Link { from, to }
            }
            "unlink" => {
                let (from, to) = self.arrow()?;
                Statement::Unlink { from, to }
            }
            "set" => self.set()?,
            "base" => {
                let (id, on) = self.on()?;
                Statement::Base { id, on }
            }
            "unbase" => {
                let (id, on) = self.on()?;
                Statement::Unbase { id, on }
            }
            "full_source" => self.full_source()?,
            "cite" => self.cite()?,
            "goal" => self.goal(token)?,
            "step" => self.step(token)?,
            "log" => Statement::Log(self.label("要记下的内容")?),
            "timeline" => self.timeline()?,
            "uncite" => {
                let id = self.name("节点地址")?;
                self.expect_word("source", "写法：uncite 节点 source sha256:... 起始..结束")?;
                Statement::Uncite {
                    id,
                    source: self.source_ref()?,
                }
            }
            "add" => return self.add("add"),
            "source" => return self.add("source"),
            other => {
                let mut error =
                    Diagnostic::new("E101", token.span, format!("没有 {other} 这条指令"));
                error = if SEARCH_WORDS.contains(&other) {
                    error.with_hint(
                        "这里没有搜索，也不能列出所有节点。想查一个词，就想它属于哪个领域，从 catalog show 开始，沿关联一层层走",
                    )
                } else {
                    match closest(other, KEYWORDS.into_iter()) {
                        Some(suggestion) => error.with_hint(format!("你是不是想写 {suggestion}？")),
                        None => error.with_hint(format!("可用的指令：{}", KEYWORDS.join("、"))),
                    }
                };
                return self.fail(error);
            }
        };
        self.end_of_line()?;
        Ok(statement)
    }

    /// 取出 `keyword` 后面的操作名。
    fn command(&mut self, keyword: &Token, name: &str, commands: &[&str]) -> Result<String, Skip> {
        let Some(token) = self.peek_word() else {
            return self.fail(
                Diagnostic::new("E102", keyword.span, format!("{name} 后面要写操作"))
                    .with_hint(format!("可用的操作：{}", commands.join("、"))),
            );
        };
        let command = word_of(&token);
        if !commands.contains(&command.as_str()) {
            let hint = match closest(&command, commands.iter().copied()) {
                Some(suggestion) => format!("你是不是想写 {name} {suggestion}？"),
                None => format!("可用的操作：{}", commands.join("、")),
            };
            return self.fail(
                Diagnostic::new(
                    "E101",
                    token.span,
                    format!("{name} 没有 {command} 这个操作"),
                )
                .with_hint(hint),
            );
        }
        self.position += 1;
        Ok(command)
    }

    fn goal(&mut self, keyword: &Token) -> Result<Statement, Skip> {
        let command = self.command(keyword, "goal", &GOAL_COMMANDS)?;
        let id = self.name("目标地址")?.text;
        Ok(Statement::Board(match command.as_str() {
            "create" => return self.goal_create(id),
            "done" => BoardOp::GoalDone {
                id,
                result: self.text()?,
            },
            _ => BoardOp::GoalDrop {
                id,
                reason: self.text()?,
            },
        }))
    }

    fn goal_create(&mut self, id: String) -> Result<Statement, Skip> {
        let title = self.text()?;
        self.end_of_line()?;
        let mut description = None;
        let mut steps = Vec::new();
        loop {
            self.skip_newlines();
            let Some(token) = self.peek_word() else {
                return self.fail(
                    Diagnostic::new("E103", self.current_span(), "goal create 没有写 end")
                        .with_hint(
                            "写法：goal create 地址 \"标题\"，下面写 desc 和 step，最后写 end",
                        ),
                );
            };
            self.position += 1;
            match word_of(&token).as_str() {
                "end" => {
                    return Ok(Statement::Board(BoardOp::GoalCreate {
                        id,
                        title,
                        description: description.unwrap_or_default(),
                        steps,
                    }));
                }
                "desc" => {
                    let value = self.text()?;
                    self.once(&mut description, value, token.span, "desc")?;
                }
                "step" => steps.push(self.text()?),
                other => {
                    return self.fail(
                        Diagnostic::new(
                            "E104",
                            token.span,
                            format!("goal create 没有 {other} 这个子句"),
                        )
                        .with_hint("可以写 desc、step、end"),
                    );
                }
            }
            self.end_of_line()?;
        }
    }

    fn step(&mut self, keyword: &Token) -> Result<Statement, Skip> {
        let command = self.command(keyword, "step", &STEP_COMMANDS)?;
        let goal = self.name("目标地址")?.text;
        Ok(Statement::Board(match command.as_str() {
            "add" => {
                let text = self.text()?;
                let after = match self.peek_word() {
                    Some(token) if word_of(&token) == "after" => {
                        self.position += 1;
                        Some(self.number("步骤号")?)
                    }
                    _ => None,
                };
                BoardOp::StepAdd { goal, text, after }
            }
            "start" => BoardOp::StepStart {
                goal,
                number: self.number("步骤号")?,
            },
            "done" => BoardOp::StepDone {
                goal,
                number: self.number("步骤号")?,
            },
            "block" => BoardOp::StepBlock {
                goal,
                number: self.number("步骤号")?,
                reason: self.text()?,
            },
            _ => BoardOp::StepRemove {
                goal,
                number: self.number("步骤号")?,
            },
        }))
    }

    fn timeline(&mut self) -> Result<Statement, Skip> {
        let mut limit = DEFAULT_TIMELINE_LIMIT;
        let mut offset = 0;
        let mut node = None;
        while let Some(token) = self.peek_word() {
            let span = token.span;
            match word_of(&token).as_str() {
                "limit" => {
                    self.position += 1;
                    limit = self.number("数量")?;
                    if limit == 0 || limit > MAX_TIMELINE_LIMIT {
                        return self.fail(Diagnostic::new(
                            "E109",
                            span,
                            format!("limit 必须在 1 到 {MAX_TIMELINE_LIMIT} 之间"),
                        ));
                    }
                }
                "offset" => {
                    self.position += 1;
                    offset = self.number("起始位置")?;
                }
                "node" => {
                    self.position += 1;
                    node = Some(self.name("节点地址")?);
                }
                _ => break,
            }
        }
        Ok(Statement::Timeline {
            limit,
            offset,
            node,
        })
    }

    fn cite(&mut self) -> Result<Statement, Skip> {
        let id = self.name("节点地址")?;
        let kind = self.peek_word().map(|token| word_of(&token));
        let citation = match kind.as_deref() {
            Some("text") => {
                self.position += 1;
                Citation::Text(self.source_text()?)
            }
            Some("source") => {
                self.position += 1;
                Citation::Source(self.source_ref()?)
            }
            _ => {
                return self.fail(
                    Diagnostic::new("E102", self.current_span(), "这里应该写 text 或 source")
                        .with_hint(
                            "写法：cite 节点 text \"\"\"原文\"\"\"，或 cite 节点 source sha256:... 起始..结束",
                        ),
                );
            }
        };
        Ok(Statement::Cite { id, citation })
    }

    fn source_ref(&mut self) -> Result<SourceRef, Skip> {
        let source = self.name("来源地址")?;
        let start = self.number("起始位置")?;
        match self.peek() {
            Some(Token {
                kind: TokenKind::DotDot,
                ..
            }) => self.position += 1,
            _ => {
                return self.fail(
                    Diagnostic::new("E102", self.current_span(), "这里应该写 ..")
                        .with_hint("写法：source sha256:... 起始..结束"),
                );
            }
        }
        let end = self.number("结束位置")?;
        Ok(SourceRef { source, start, end })
    }

    /// 每个地址一条语句：`show a, b` 和分两行写完全相同。
    fn for_each_name(&mut self, statement: fn(Name) -> Statement) -> Result<Statement, Skip> {
        let mut names = self.names()?.into_iter();
        let first = names.next().expect("names 至少返回一个地址");
        self.extra.extend(names.map(|name| Located {
            span: name.span,
            statement: statement(name),
        }));
        Ok(statement(first))
    }

    fn catalog(&mut self, keyword: &Token) -> Result<Statement, Skip> {
        let Some(token) = self.peek_word() else {
            return self.fail(
                Diagnostic::new("E102", keyword.span, "catalog 后面要写操作")
                    .with_hint(format!("可用的操作：{}", CATALOG_COMMANDS.join("、"))),
            );
        };
        let command = word_of(&token);
        if !CATALOG_COMMANDS.contains(&command.as_str()) {
            let hint = match closest(&command, CATALOG_COMMANDS.into_iter()) {
                Some(suggestion) => format!("你是不是想写 catalog {suggestion}？"),
                None => format!("可用的操作：{}", CATALOG_COMMANDS.join("、")),
            };
            return self.fail(
                Diagnostic::new(
                    "E101",
                    token.span,
                    format!("catalog 没有 {command} 这个操作"),
                )
                .with_hint(hint),
            );
        }
        self.position += 1;
        Ok(match command.as_str() {
            "show" => Statement::CatalogShow,
            "create" => {
                let name = self.label("板块的名字")?;
                self.expect_word(
                    "desc",
                    "写法：catalog create \"板块\" desc \"\"\"描述\"\"\"",
                )?;
                Statement::CatalogCreate {
                    name,
                    description: self.label("板块的描述")?,
                }
            }
            "describe" => Statement::CatalogDescribe {
                name: self.label("板块的名字")?,
                description: self.label("板块的描述")?,
            },
            "rename" => {
                let name = self.label("板块的名字")?;
                self.expect_word("to", "写法：catalog rename \"旧名字\" to \"新名字\"")?;
                Statement::CatalogRename {
                    name,
                    new_name: self.label("新名字")?,
                }
            }
            "delete" => Statement::CatalogDelete(self.label("板块的名字")?),
            "add" => {
                let id = self.name("节点地址")?;
                self.expect_word("to", "写法：catalog add 节点 to \"板块\"")?;
                Statement::CatalogAdd {
                    id,
                    section: self.label("板块的名字")?,
                }
            }
            _ => {
                let id = self.name("节点地址")?;
                self.expect_word("from", "写法：catalog remove 节点 from \"板块\"")?;
                Statement::CatalogRemove {
                    id,
                    section: self.label("板块的名字")?,
                }
            }
        })
    }

    fn expect_word(&mut self, word: &str, hint: &str) -> Result<(), Skip> {
        match self.peek_word() {
            Some(token) if word_of(&token) == word => {
                self.position += 1;
                Ok(())
            }
            _ => self.fail(
                Diagnostic::new("E102", self.current_span(), format!("这里应该写 {word}"))
                    .with_hint(hint),
            ),
        }
    }

    /// 原文必须逐字保存，所以不去掉任何空白。
    fn source_text(&mut self) -> Result<Label, Skip> {
        match self.peek().cloned() {
            Some(Token {
                kind: TokenKind::Text(text) | TokenKind::Block(text),
                span,
            }) => {
                self.position += 1;
                Ok(Label { text, span })
            }
            _ => self.fail(
                Diagnostic::new("E102", self.current_span(), "这里应该写原文")
                    .with_hint("写在引号里。内容有三个连续的引号时，用四个引号包住"),
            ),
        }
    }

    fn label(&mut self, what: &str) -> Result<Label, Skip> {
        match self.peek().cloned() {
            Some(Token {
                kind: TokenKind::Text(text),
                span,
            }) => {
                self.position += 1;
                Ok(Label { text, span })
            }
            Some(Token {
                kind: TokenKind::Block(text),
                span,
            }) => {
                self.position += 1;
                Ok(Label {
                    text: text.trim().to_owned(),
                    span,
                })
            }
            _ => self.fail(
                Diagnostic::new("E102", self.current_span(), format!("这里应该写{what}"))
                    .with_hint("要写在引号里"),
            ),
        }
    }

    fn links(&mut self) -> Result<Statement, Skip> {
        let id = self.name("节点地址")?;
        let mut depth = 1;
        let mut limit = DEFAULT_LINK_LIMIT;
        let mut offset = 0;
        while let Some(token) = self.peek_word() {
            let span = token.span;
            let option = word_of(&token);
            match option.as_str() {
                "depth" => {
                    self.position += 1;
                    depth = self.number("深度")?;
                    if depth == 0 || depth > MAX_LINK_DEPTH {
                        return self.fail(Diagnostic::new(
                            "E109",
                            span,
                            format!("depth 必须在 1 到 {MAX_LINK_DEPTH} 之间"),
                        ));
                    }
                }
                "limit" => {
                    self.position += 1;
                    limit = self.number("数量")?;
                    if limit == 0 || limit > MAX_LINK_LIMIT {
                        return self.fail(Diagnostic::new(
                            "E109",
                            span,
                            format!("limit 必须在 1 到 {MAX_LINK_LIMIT} 之间"),
                        ));
                    }
                }
                "offset" => {
                    self.position += 1;
                    offset = self.number("起始位置")?;
                }
                _ => break,
            }
        }
        Ok(Statement::Links {
            id,
            depth,
            limit,
            offset,
        })
    }

    fn arrow(&mut self) -> Result<(Name, Name), Skip> {
        let from = self.name("起点的节点地址")?;
        match self.peek() {
            Some(Token {
                kind: TokenKind::Arrow,
                ..
            }) => self.position += 1,
            _ => {
                return self.fail(
                    Diagnostic::new("E102", self.current_span(), "这里应该写 ->")
                        .with_hint("写法：link 起点 -> 终点"),
                );
            }
        }
        Ok((from, self.name("终点的节点地址")?))
    }

    fn on(&mut self) -> Result<(Name, Name), Skip> {
        let id = self.name("节点地址")?;
        match self.peek_word() {
            Some(token) if word_of(&token) == "on" => self.position += 1,
            _ => {
                return self.fail(
                    Diagnostic::new("E102", self.current_span(), "这里应该写 on")
                        .with_hint("写法：base 结论 on 依据"),
                );
            }
        }
        Ok((id, self.name("依据的节点地址")?))
    }

    fn set(&mut self) -> Result<Statement, Skip> {
        let id = self.name("节点地址")?;
        let span = self.current_span();
        let field = match self.peek_word().map(|token| word_of(&token)).as_deref() {
            Some("title") => Field::Title,
            Some("desc") => Field::Description,
            _ => {
                return self.fail(
                    Diagnostic::new("E102", span, "这里应该写 title 或 desc")
                        .with_hint("节点地址不能修改；写法：set 节点 desc \"\"\"新的描述\"\"\""),
                );
            }
        };
        self.position += 1;
        let value_span = self.current_span();
        let value = self.text()?;
        Ok(Statement::Set {
            id,
            field,
            value,
            value_span,
        })
    }

    fn full_source(&mut self) -> Result<Statement, Skip> {
        let id = self.name("节点地址")?;
        let mut offset = 0;
        let mut limit = DEFAULT_SOURCE_LIMIT;
        while let Some(token) = self.peek_word() {
            let span = token.span;
            match word_of(&token).as_str() {
                "offset" => {
                    self.position += 1;
                    offset = self.number("起始位置")?;
                }
                "limit" => {
                    self.position += 1;
                    limit = self.number("字节数")?;
                    if limit == 0 || limit > MAX_SOURCE_LIMIT {
                        return self.fail(Diagnostic::new(
                            "E109",
                            span,
                            format!("limit 必须在 1 到 {MAX_SOURCE_LIMIT} 之间"),
                        ));
                    }
                }
                _ => break,
            }
        }
        Ok(Statement::FullSource { id, offset, limit })
    }

    /// `add` 和 `source` 写法相同：`add` 必须有 `based_on`，`source` 必须有原文且不能有 `based_on`。
    fn add(&mut self, keyword: &str) -> Result<Statement, Skip> {
        let id = self.name("新节点的地址")?;
        self.end_of_line()?;
        let is_source = keyword == "source";
        let mut add = AddNode {
            id,
            title: None,
            description: None,
            sources: Vec::new(),
            based_on: Vec::new(),
            related: Vec::new(),
            related_by: Vec::new(),
            text: None,
        };
        let finish = |add: AddNode| {
            if is_source {
                Statement::Source(Box::new(add))
            } else {
                Statement::Add(Box::new(add))
            }
        };
        loop {
            self.skip_newlines();
            let Some(token) = self.peek().cloned() else {
                self.errors.push(
                    Diagnostic::new(
                        "E103",
                        add.id.span,
                        format!("{keyword} {} 没有写 end", add.id.text),
                    )
                    .with_hint(format!("{keyword} 的最后一行要写 end")),
                );
                return Ok(finish(add));
            };
            let clause = word_of(&token);
            if clause == "end" {
                self.position += 1;
                if self.end_of_line().is_err() {
                    self.skip_line();
                }
                return Ok(finish(add));
            }
            if self.clause(&mut add, keyword, &clause, &token).is_err() {
                self.skip_line();
            }
        }
    }

    fn clause(
        &mut self,
        add: &mut AddNode,
        keyword: &str,
        clause: &str,
        token: &Token,
    ) -> Result<(), Skip> {
        let allowed: &[&'static str] = if keyword == "source" {
            &SOURCE_CLAUSES
        } else {
            &ADD_CLAUSES
        };
        if !allowed.contains(&clause) {
            let mut error = Diagnostic::new(
                "E104",
                token.span,
                format!("{keyword} 里不能写 {}", describe(token)),
            );
            error = if clause == "based_on" {
                error.with_hint("source 只用于直接来自原文的知识；由别的节点推出的知识请用 add")
            } else if clause == "section" {
                error.with_hint(format!(
                    "板块用单独的指令管理：{keyword} 之后写 catalog add {} to \"板块\"",
                    add.id.text
                ))
            } else {
                match closest(clause, allowed.iter().copied()) {
                    Some(suggestion) => error.with_hint(format!("你是不是想写 {suggestion}？")),
                    None => error.with_hint(format!(
                        "{keyword} 里可以写：{}。是不是忘了写 end？",
                        allowed.join("、")
                    )),
                }
            };
            return self.fail(error);
        }
        self.position += 1;
        match clause {
            "title" => {
                let value = self.text()?;
                self.once(&mut add.title, value, token.span, "title")?;
            }
            "desc" => {
                let value = self.text()?;
                self.once(&mut add.description, value, token.span, "desc")?;
            }
            "text" => {
                let value = self.source_text()?.text;
                self.once(&mut add.text, value, token.span, "text")?;
            }
            "source" => {
                let source = self.source_ref()?;
                add.sources.push(source);
            }
            "based_on" => add.based_on.extend(self.names()?),
            "related" => add.related.extend(self.names()?),
            "related_by" => add.related_by.extend(self.names()?),
            _ => unreachable!("上面已经检查过子句"),
        }
        self.end_of_line()
    }

    fn once(
        &mut self,
        slot: &mut Option<String>,
        value: String,
        span: Span,
        clause: &str,
    ) -> Result<(), Skip> {
        if slot.is_some() {
            return self.fail(Diagnostic::new(
                "E105",
                span,
                format!("{clause} 写了不止一次"),
            ));
        }
        *slot = Some(value);
        Ok(())
    }

    fn names(&mut self) -> Result<Vec<Name>, Skip> {
        let mut names = vec![self.name("节点地址")?];
        while matches!(
            self.peek(),
            Some(Token {
                kind: TokenKind::Comma,
                ..
            })
        ) {
            self.position += 1;
            names.push(self.name("节点地址")?);
        }
        Ok(names)
    }

    fn name(&mut self, what: &str) -> Result<Name, Skip> {
        match self.peek().cloned() {
            Some(Token {
                kind: TokenKind::Word(text),
                span,
            }) => {
                self.position += 1;
                Ok(Name { text, span })
            }
            _ => self.fail(Diagnostic::new(
                "E102",
                self.current_span(),
                format!("这里应该写{what}"),
            )),
        }
    }

    fn text(&mut self) -> Result<String, Skip> {
        match self.peek().cloned() {
            Some(Token {
                kind: TokenKind::Text(text),
                ..
            }) => {
                self.position += 1;
                Ok(text)
            }
            Some(Token {
                kind: TokenKind::Block(text),
                ..
            }) => {
                self.position += 1;
                Ok(text.trim().to_owned())
            }
            _ => self.fail(
                Diagnostic::new("E102", self.current_span(), "这里应该写带引号的文字")
                    .with_hint("短文字用 \"...\"，多行或含引号的内容用 \"\"\"...\"\"\""),
            ),
        }
    }

    fn number(&mut self, what: &str) -> Result<usize, Skip> {
        let span = self.current_span();
        match self.peek().cloned() {
            Some(Token {
                kind: TokenKind::Word(word),
                ..
            }) => match word.parse() {
                Ok(number) => {
                    self.position += 1;
                    Ok(number)
                }
                Err(_) => self.fail(Diagnostic::new(
                    "E102",
                    span,
                    format!("{what}应该是非负整数，这里写的是 {word}"),
                )),
            },
            _ => self.fail(Diagnostic::new(
                "E102",
                span,
                format!("这里应该写{what}（非负整数）"),
            )),
        }
    }

    fn end_of_line(&mut self) -> Result<(), Skip> {
        let after_block =
            self.position > 0 && matches!(self.tokens[self.position - 1].kind, TokenKind::Block(_));
        match self.peek() {
            None
            | Some(Token {
                kind: TokenKind::Newline,
                ..
            }) => Ok(()),
            Some(token) => {
                let error = Diagnostic::new(
                    "E106",
                    token.span,
                    format!("这一行多写了 {}", describe(token)),
                )
                .with_hint(if after_block {
                    "如果这是文字内容的一部分：内容有三个连续的引号时，用四个引号包住整段"
                } else {
                    "每条语句单独占一行"
                });
                self.fail(error)
            }
        }
    }

    fn fail<T>(&mut self, diagnostic: Diagnostic) -> Result<T, Skip> {
        self.errors.push(diagnostic);
        Err(Skip)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn peek_word(&self) -> Option<Token> {
        self.peek()
            .filter(|token| matches!(token.kind, TokenKind::Word(_)))
            .cloned()
    }

    /// 当前词的位置；已经到行尾或末尾时，指向上一个词的后面。
    fn current_span(&self) -> Span {
        match self.peek() {
            Some(token) if token.kind != TokenKind::Newline => token.span,
            _ => match self
                .position
                .checked_sub(1)
                .and_then(|i| self.tokens.get(i))
            {
                Some(previous) => Span {
                    line: previous.span.line,
                    column: previous.span.column + previous.span.length,
                    length: 1,
                },
                None => Span {
                    line: 1,
                    column: 1,
                    length: 1,
                },
            },
        }
    }

    fn skip_newlines(&mut self) {
        while matches!(
            self.peek(),
            Some(Token {
                kind: TokenKind::Newline,
                ..
            })
        ) {
            self.position += 1;
        }
    }

    fn skip_line(&mut self) {
        while let Some(token) = self.peek() {
            if token.kind == TokenKind::Newline {
                return;
            }
            self.position += 1;
        }
    }
}

fn word_of(token: &Token) -> String {
    match &token.kind {
        TokenKind::Word(word) => word.clone(),
        _ => String::new(),
    }
}

fn describe(token: &Token) -> String {
    match &token.kind {
        TokenKind::Word(word) => word.clone(),
        TokenKind::Text(_) | TokenKind::Block(_) => "一段文字".to_owned(),
        TokenKind::Arrow => "->".to_owned(),
        TokenKind::DotDot => "..".to_owned(),
        TokenKind::Comma => "逗号".to_owned(),
        TokenKind::Newline => "换行".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::lexer::tokenize;
    use super::*;

    fn statements(script: &str) -> Vec<Statement> {
        let (tokens, lex_errors) = tokenize(script);
        assert!(lex_errors.is_empty(), "{lex_errors:?}");
        let (statements, errors) = parse(tokens);
        assert!(errors.is_empty(), "{errors:?}");
        statements.into_iter().map(|s| s.statement).collect()
    }

    fn errors(script: &str) -> Vec<Diagnostic> {
        let (tokens, _) = tokenize(script);
        parse(tokens).1
    }

    #[test]
    fn parses_simple_statements() {
        let parsed = statements("catalog show\n\n# 注释\nshow a\nlink a -> b\nbase a on b\n");

        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0], Statement::CatalogShow);
        assert!(matches!(&parsed[1], Statement::Show(name) if name.text == "a"));
        assert!(
            matches!(&parsed[2], Statement::Link { from, to } if from.text == "a" && to.text == "b")
        );
        assert!(
            matches!(&parsed[3], Statement::Base { id, on } if id.text == "a" && on.text == "b")
        );
    }

    #[test]
    fn parses_cite_and_uncite() {
        let parsed = statements(
            "cite a text \"\"\"原文\"\"\"\ncite a source sha256:ab 1..9\nuncite a source sha256:ab 1..9",
        );

        assert!(matches!(
            &parsed[0],
            Statement::Cite { id, citation: Citation::Text(text) }
                if id.text == "a" && text.text == "原文"
        ));
        let Statement::Cite {
            citation: Citation::Source(source),
            ..
        } = &parsed[1]
        else {
            panic!("应为引用已有原文");
        };
        assert_eq!(source.source.text, "sha256:ab");
        assert_eq!((source.start, source.end), (1, 9));
        assert!(matches!(
            &parsed[2],
            Statement::Uncite { source, .. } if (source.start, source.end) == (1, 9)
        ));

        let found = errors(
            "cite a\ncite a \"原文\"\ncite a text\ncite a source sha256:ab 1\nuncite a\nuncite a text \"x\"\nuncite a source sha256:ab\ncite a text \"x\" y",
        );
        let codes: Vec<&str> = found.iter().map(|error| error.code).collect();
        assert_eq!(
            codes,
            [
                "E102", "E102", "E102", "E102", "E102", "E102", "E102", "E106"
            ]
        );
    }

    #[test]
    fn links_has_defaults_and_options() {
        let parsed = statements("links a\nlinks a limit 3 depth 2\nlinks a offset 7 limit 3");

        assert!(matches!(
            &parsed[0],
            Statement::Links {
                depth: 1,
                limit: 50,
                offset: 0,
                ..
            }
        ));
        assert!(matches!(
            &parsed[1],
            Statement::Links {
                depth: 2,
                limit: 3,
                offset: 0,
                ..
            }
        ));
        assert!(matches!(
            &parsed[2],
            Statement::Links {
                depth: 1,
                limit: 3,
                offset: 7,
                ..
            }
        ));
    }

    #[test]
    fn parses_add() {
        let parsed = statements(
            "add n\n  title \"标题\"\n  desc \"\"\"描述\n两行\"\"\"\n  source sha256:ab 1..9\n  based_on x, y\n  related z\n  related_by w\nend\nshow n",
        );

        let Statement::Add(add) = &parsed[0] else {
            panic!("应为 add");
        };
        assert_eq!(add.title.as_deref(), Some("标题"));
        assert_eq!(add.description.as_deref(), Some("描述\n两行"));
        assert_eq!(add.sources[0].source.text, "sha256:ab");
        assert_eq!((add.sources[0].start, add.sources[0].end), (1, 9));
        assert_eq!(add.based_on.len(), 2);
        assert!(matches!(parsed[1], Statement::Show(_)));
    }

    #[test]
    fn reports_every_error_and_suggests_fixes() {
        let found = errors("shwo a\nshow\nlink a b\nlinks a depth 9\nset a id \"x\"\nshow a b");

        let codes: Vec<&str> = found.iter().map(|error| error.code).collect();
        assert_eq!(codes, ["E101", "E102", "E102", "E109", "E102", "E106"]);
        assert_eq!(found[0].hint.as_deref(), Some("你是不是想写 show？"));
        assert_eq!(found[0].span.line, 1);
        assert_eq!(found[5].span.line, 6);
    }

    #[test]
    fn add_errors_are_precise() {
        let found =
            errors("add n\n  titel \"x\"\n  title \"a\"\n  title \"b\"\n  source sha256:ab 1\n");

        let codes: Vec<&str> = found.iter().map(|error| error.code).collect();
        assert_eq!(codes, ["E104", "E105", "E102", "E103"]);
        assert_eq!(found[0].hint.as_deref(), Some("你是不是想写 title？"));
    }

    #[test]
    fn forgetting_end_points_at_the_next_statement() {
        let found = errors("add n\n  title \"a\"\nshow x\nend\n");

        assert_eq!(found[0].code, "E104");
        assert!(found[0].message.contains("show"));
        assert!(found[0].hint.as_deref().unwrap().contains("忘了写 end"));
    }
}
