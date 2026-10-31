//! 模型操作记忆用的指令语言。
//!
//! 一段脚本依次经过词法分析、语法分析和语义检查，全部通过才开始执行。写错时返回和编译器一样的
//! 报错，记忆完全不变。语言里没有内存地址和文件路径，所以从语法上就碰不到它们：模型只能用文本
//! 地址引用节点，地址是否存在由 C++ 的 set 回答。

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
catalog                                  查看总表：按领域分类的代表节点
show 节点                                 查看节点的全部内容：标题、描述、所有关系和证据原文
links 节点 [depth N] [limit N]            沿关联往外看，默认 depth 1，limit 50
evidence 节点                             读取证据指向的原文
based_by 节点                             只列出基于这个节点推导的节点，删除节点前用它查依赖
check                                    列出从总表走不到的节点

修改：
add 节点
  title "标题"
  desc """描述，可以多行，可以含引号"""
  source sha256:... 起始..结束             来自原始数据的证据，可以写多条
  based_on 节点, 节点                      由其他节点推出的证据
  related 节点, 节点                       它关联哪些节点
  related_by 节点, 节点                    哪些节点关联它
  section "板块"                           成为这个板块的代表节点
end
remove 节点
link 甲 -> 乙
unlink 甲 -> 乙
set 节点 title "新标题"
set 节点 desc """新描述"""
base 结论 on 依据
unbase 结论 on 依据

规则：
- add 必须有 title、desc，至少一条 source 或 based_on，至少一条 related_by 或 section。
- 节点地址只能包含字母、数字、- 和 _，不能修改。
- 证据不能形成循环。
- 一段脚本要么全部执行，要么完全不执行。出错时会告诉你哪一行错了以及怎么改，改好后重新提交整段脚本。
- 还有结论依据某个节点时不能删除它。先用 base 和 unbase 给这些结论换依据，或者先删除它们，可以写在同一段脚本里。
- 修改描述后，结果里的 warning 会列出依据它的结论，它们是否还成立由你判断。
- 删除节点或减少关联后，可能有节点走不到了，用 check 确认。
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
            Ok(report) if report.read_failures == 0 => Output {
                ok: true,
                text: limit_output(report.text),
            },
            Ok(report) => Output {
                ok: false,
                text: limit_output(format!(
                    "{}有 {} 条读取失败，其余语句都已执行。",
                    report.text, report.read_failures
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

fn failure(errors: &[Diagnostic], script: &str) -> Output {
    Output {
        ok: false,
        text: limit_output(render(errors, script)),
    }
}

fn limit_output(mut text: String) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text;
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(
        "\n（输出超过上限，后面的内容被截断了。语句都已执行，可以用 limit 或分多次查看。）",
    );
    text
}

#[cfg(test)]
mod tests;
