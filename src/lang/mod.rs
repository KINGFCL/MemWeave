//! 模型操作记忆用的指令语言。
//!
//! 一段脚本依次经过词法分析、语法分析和语义检查，全部通过才开始执行。写错时返回和编译器一样的
//! 报错，记忆完全不变。语言里没有内存地址，所以从语法上就碰不到它：模型只能用文本地址引用节点，
//! 地址是否存在由 C++ 的 set 回答。只有 `export` 会写文件路径，它只会新建目录，不会覆盖已有内容。

pub mod ast;
pub mod check;
pub mod diagnostic;
pub mod exec;
pub mod journal;
pub mod lexer;
pub mod parser;

use crate::memory::Memory;
use crate::source::SourceStore;
use diagnostic::{Diagnostic, render};
use journal::Journal;

const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_STATEMENTS: usize = 1000;
/// 输出的上限，避免一条脚本撑爆模型的上下文。
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// 放进提示词里的语言说明。
pub const SPEC: &str = r####"用下面的语言操作记忆。每条语句占一行，# 开头是注释。节点用文本地址引用。

读取：
catalog show                             查看总表：每个领域的名字、描述和代表节点
show 节点                                 查看节点的全部内容：标题、描述、所有关系和证据原文
links 节点 [depth N] [limit N]            沿关联往外看，默认 depth 1，limit 50
evidence 节点                             读取证据指向的原文
based_by 节点                             只列出基于这个节点推导的节点，删除节点前用它查依赖
related_by 节点                           只列出关联了这个节点的节点，可以用来反向探索
full_source 节点 [offset N] [limit N]     查看节点指向的完整原始文件，不只是它引用的那一段。文件很大时分段读
check                                    列出从总表走不到的节点和没有代表的板块

修改：
add 节点                                  新增由别的节点推出的知识
  title "标题"
  desc """描述，可以多行，可以含引号"""
  based_on 节点, 节点                      必填：它依据哪些节点
  text """原文"""                          可选：同时存入一段新原文
  source sha256:... 起始..结束             可选：同时引用已有的原文，可以写多条
  related 节点, 节点                       它关联哪些节点
  related_by 节点, 节点                    哪些节点关联它
end
source 节点                               新增直接来自原文的知识，不能写 based_on
  title "标题"
  desc """描述"""
  text """原文"""                          把这段文字存成不可变的原文
  source sha256:... 起始..结束             或者引用已有的原文。这两项至少写一个
  related 节点, 节点
  related_by 节点, 节点
end
remove 节点
catalog create "板块" desc """这个领域包含什么"""
catalog describe "板块" """新的描述"""
catalog rename "板块" to "新名字"
catalog delete "板块"                      删除板块，节点本身保留
catalog add 节点 to "板块"                 让节点成为这个领域的代表
catalog remove 节点 from "板块"            撤下代表，节点本身保留
link 甲 -> 乙
unlink 甲 -> 乙
set 节点 title "新标题"
set 节点 desc """新描述"""
base 结论 on 依据
unbase 结论 on 依据
export "路径"                             把当前记忆导出成新的知识库目录，只带节点引用到的原文

规则：
- add 必须有 title、desc、based_on。
- source 必须有 title、desc，至少一段原文。要记下对话或工具结果里的新内容，就用它。
- 新节点必须能被找到：在同一段脚本里写 related_by、link 某节点 -> 它，或者 catalog add 它 to "板块"。
- 总表由你维护。它是目录，不是索引：每个领域只放几个代表节点，其他节点靠 related 到达。
- 节点地址只能包含字母、数字、- 和 _，不能修改。
- 证据不能形成循环。
- 一段脚本要么全部执行，要么完全不执行。出错时会告诉你哪一行错了以及怎么改，改好后重新提交整段脚本。
- 还有结论依据某个节点时不能删除它。先用 base 和 unbase 给这些结论换依据，或者先删除它们，可以写在同一段脚本里。
- 修改描述后，结果里的 warning 会列出依据它的结论，它们是否还成立由你判断。
- 删除节点或减少关联后，可能有节点走不到了，用 check 确认。
- export 的目标必须不存在或是空目录，不会覆盖。有节点走不到时会拒绝导出并列出它们，接入或删除后再导出。
"####;

/// 会话的权限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ReadOnly,
    ReadWrite,
}

/// 一段脚本的执行结果。无论对错，都是可以直接交给模型的文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub ok: bool,
    pub text: String,
}

/// 模型操作记忆的唯一入口。
pub struct Session<'a> {
    memory: &'a mut Memory,
    sources: &'a dyn SourceStore,
    mode: Mode,
    journal: Option<&'a Journal>,
}

impl<'a> Session<'a> {
    pub fn new(memory: &'a mut Memory, sources: &'a dyn SourceStore, mode: Mode) -> Self {
        Self {
            memory,
            sources,
            mode,
            journal: None,
        }
    }

    /// 修改记忆的脚本会先记到这个日志里。
    pub fn with_journal(mut self, journal: &'a Journal) -> Self {
        self.journal = Some(journal);
        self
    }

    /// 执行一段脚本。对任何输入都只会返回文本，不会崩溃。
    pub fn run(&mut self, script: &str) -> Output {
        if script.len() > MAX_SCRIPT_BYTES {
            return failure(&[check::too_large("大小", MAX_SCRIPT_BYTES)], "");
        }

        let (tokens, mut errors) = lexer::tokenize(script);
        let (statements, parse_errors) = parser::parse(tokens);
        errors.extend(parse_errors);
        if statements.len() > MAX_STATEMENTS {
            errors.push(check::too_large("语句数", MAX_STATEMENTS));
        }
        if !errors.is_empty() {
            errors.sort_by_key(|error| (error.span.line, error.span.column));
            return failure(&errors, script);
        }

        let errors = check::check(&statements, self.memory, self.sources, self.mode);
        if !errors.is_empty() {
            return failure(&errors, script);
        }
        if statements.is_empty() {
            return Output {
                ok: true,
                text: "脚本是空的，没有执行任何语句。".to_owned(),
            };
        }

        // 新原文要最先写：它们不可变，也不影响图，中途失败最多留下没人引用的文件。
        if let Err(error) = self.store_new_sources(&statements) {
            return Output {
                ok: false,
                text: format!("写入原文失败，没有执行任何语句：{error}"),
            };
        }

        // 先落盘再执行：执行到一半崩溃时，重启后重放能恢复。
        if let Some(journal) = self.journal
            && statements.iter().any(|s| s.statement.writes())
            && let Err(error) = journal.append(script)
        {
            return Output {
                ok: false,
                text: format!("写入日志失败，没有执行任何语句：{error}"),
            };
        }

        match exec::execute(&statements, self.memory, self.sources) {
            Ok(report) if report.failures == 0 => Output {
                ok: true,
                text: limit_output(report.text),
            },
            Ok(report) => Output {
                ok: false,
                text: limit_output(format!(
                    "{}有 {} 条语句失败，其余语句都已执行。",
                    report.text, report.failures
                )),
            },
            // 检查已经通过，只有内存不足才会走到这里。
            Err((done, text, error)) => Output {
                ok: false,
                text: limit_output(format!(
                    "{text}error[E900] 第 {} 条语句执行失败：{error}\n前 {done} 条语句已经生效，后面的没有执行。",
                    done + 1
                )),
            },
        }
    }
}

impl Session<'_> {
    /// 重放日志里的一段脚本，只执行其中的写语句。
    ///
    /// 读语句对恢复记忆没有作用，还可能因为来源损坏而失败，不能让它们妨碍知识库加载。
    pub fn replay(&mut self, script: &str) -> Output {
        let (tokens, mut errors) = lexer::tokenize(script);
        let (mut statements, parse_errors) = parser::parse(tokens);
        errors.extend(parse_errors);
        statements.retain(|located| located.statement.writes());
        errors.extend(check::check(
            &statements,
            self.memory,
            self.sources,
            Mode::ReadWrite,
        ));
        if !errors.is_empty() {
            return failure(&errors, script);
        }
        // 日志里有原文，文件丢了也能恢复。
        if let Err(error) = self.store_new_sources(&statements) {
            return Output {
                ok: false,
                text: error.to_string(),
            };
        }
        match exec::execute(&statements, self.memory, self.sources) {
            Ok(report) => Output {
                ok: true,
                text: report.text,
            },
            Err((_, _, error)) => Output {
                ok: false,
                text: error.to_string(),
            },
        }
    }
}

impl Session<'_> {
    fn store_new_sources(&self, statements: &[ast::Located]) -> Result<(), crate::error::Error> {
        for located in statements {
            if let ast::Statement::Add(add) | ast::Statement::Source(add) = &located.statement
                && let Some(text) = &add.text
            {
                self.sources.put(text.as_bytes())?;
            }
        }
        Ok(())
    }
}

fn failure(errors: &[Diagnostic], script: &str) -> Output {
    Output {
        ok: false,
        text: truncate(
            render(errors, script),
            "\n（输出超过上限，后面的内容被截断了。脚本没有执行。）",
        ),
    }
}

fn limit_output(text: String) -> String {
    truncate(
        text,
        "\n（输出超过上限，后面的内容被截断了。语句都已执行，可以用 limit 或分多次查看。）",
    )
}

fn truncate(mut text: String, note: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text;
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(note);
    text
}

#[cfg(test)]
mod tests;
