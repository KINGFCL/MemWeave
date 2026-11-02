//! 模型操作记忆用的指令语言。
//!
//! 一段脚本依次经过词法分析、语法分析和语义检查，全部通过才开始执行。写错时返回和编译器一样的
//! 报错，记忆完全不变。通过之后修改先进草稿，`save` 才写进日志。语言里没有内存地址，所以从语法上就碰不到它：模型只能用文本地址引用节点，
//! 地址是否存在由 C++ 的 set 回答。只有 `export` 会写文件路径，它只会新建目录，不会覆盖已有内容。

pub mod ast;
pub mod check;
pub mod diagnostic;
pub mod exec;
pub mod journal;
pub mod lexer;
pub mod parser;

use crate::manual::Manual;
use crate::memory::{DraftScript, Memory};
use crate::source::{SourceStore, Trusted};
use crate::timeline;
use diagnostic::{Diagnostic, render};
use journal::Journal;

const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_STATEMENTS: usize = 1000;
/// 输出的上限，避免一条脚本撑爆模型的上下文。
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// 放进提示词里的语言说明。
pub const SPEC: &str = r####"用下面的语言操作记忆。每条语句占一行，# 开头是注释。节点用文本地址引用。

所有修改都先进草稿，能读到，但 save 之后才算保存。

读取都要写明版本：revision 读当前草稿，origin 读已保存的版本。没有未保存的修改时两者一样。下面只写 revision，把它换成 origin 就是读已保存的版本。
revision catalog show                     查看工作板、最近行为和总表：每个领域的名字、描述和代表节点
revision show 节点, 节点                  查看节点的全部内容：标题、描述、所有关系和证据原文
revision links 节点 [depth N] [limit N] [offset N] 沿关联往外看，默认 depth 1，limit 50。没看完时用 offset 接着看
revision evidence 节点, 节点              沿依据一路追到原文：依据节点的标题和描述，以及所有原文
revision based_by 节点, 节点              只列出基于这个节点推导的节点，删除节点前用它查依赖
revision related_by 节点, 节点            只列出关联了这个节点的节点，可以用来反向探索
revision full_source 节点 [offset N] [limit N] 查看节点指向的完整原始文件，不只是它引用的那一段。文件很大时分段读
revision check                            列出从总表走不到的节点、没有代表的板块和丢失的原文
revision timeline [limit N] [offset N] [node 节点] 时间线，从新到旧。node 只看这个节点的历史
revision export "路径"                    把这个版本的记忆导出成新的知识库目录，只带节点引用到的原文

草稿：
revision display [节点]                   第一行 has_revision 说明有没有未保存的修改，后面列出改了哪些，不带内容
revision display-detail [节点]            看具体改了什么，新节点显示全部内容
comparison display [节点]                 已保存的版本和草稿逐项对照，只列出变了的字段
save                                      保存所有修改。可以单独执行，也可以写在脚本最后一行
discard                                   放弃所有未保存的修改，要单独执行

修改：
add 节点                                  新增由别的节点推出的知识
  title "标题"
  desc """描述，可以多行，可以含引号，首尾空白会去掉"""
  based_on 节点, 节点                      必填：它依据哪些节点
  text """原文"""                          可选：同时存入一段新原文
  source sha256:... 起始..结束             可选：同时引用已有的原文，可以写多条
  related 节点, 节点                       它关联哪些节点
  related_by 节点, 节点                    哪些节点关联它
end
source 节点                               新增直接来自原文的知识，不能写 based_on
  title "标题"
  desc """描述"""
  text """原文"""                          把这段文字逐字存成不可变的原文，含 """ 时用 """" 包住
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
cite 节点 text """原文"""                  存一段新原文，并让节点引用它
cite 节点 source sha256:... 起始..结束     让节点引用已有原文的一段
uncite 节点 source sha256:... 起始..结束   去掉节点对这段原文的引用，地址和范围抄 revision show 的输出

工作板：
goal create 目标 "标题"                    新建目标，一次写完步骤，第一步自动开始
  desc """为什么要做、做到什么算完成"""
  step "步骤"
  step "步骤"
end
step done 目标 编号                        完成这一步，下一步自动开始
step start 目标 编号                       切到这一步，原来进行中的回到未开始
step block 目标 编号 """卡在哪里"""
step add 目标 "步骤" [after 编号]
step remove 目标 编号
goal done 目标 """结果"""                  结束目标，所有步骤都要完成
goal drop 目标 """为什么放弃"""
log """做了什么"""                         记下记忆之外的行为，比如跑了测试

规则：
- add 必须有 title、desc、based_on。
- source 必须有 title、desc，至少一段原文。要记下对话或工具结果里的新内容，就用它。
- 新节点必须能被找到：save 之前写 related_by、link 某节点 -> 它，或者 catalog add 它 to "板块"。可以分几段脚本写完，没接入时不能保存。
- 总表由你维护。它是目录，不是索引：每个领域只放几个代表节点，其他节点靠 related 到达。
- 节点地址只能包含字母、数字、- 和 _，不能修改。manual- 开头的地址留给内置说明，只能读。
- 证据不能形成循环。
- 一段脚本要么全部执行，要么完全不执行。出错时会告诉你哪一行错了以及怎么改，改好后重新提交整段脚本。
- 没 save 的修改不算数。结果里出现 revision: N 段脚本未保存 时，做完就 save，不要了就 discard。
- 还有结论依据某个节点时不能删除它。先用 base 和 unbase 给这些结论换依据，或者先删除它们，可以写在同一段脚本里。
- 修改描述或原文引用后，结果里的 warning 会列出依据它的结论，它们是否还成立由你判断。
- 原文说错或过时了：写一段脚本，用 cite 引用新原文，uncite 去掉旧的，需要时用 set 改描述。原文本身不可变，节点至少要留一条证据。
- 删除节点或减少关联后，可能有节点走不到了，用 revision check 确认。
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
    manual: Option<&'a Manual>,
    output_limit: usize,
    /// 固定的时间，测试用。没有时取当前时间。
    fixed_time: Option<Option<u64>>,
}

impl<'a> Session<'a> {
    pub fn new(memory: &'a mut Memory, sources: &'a dyn SourceStore, mode: Mode) -> Self {
        Self {
            memory,
            sources,
            mode,
            journal: None,
            manual: None,
            output_limit: MAX_OUTPUT_BYTES,
            fixed_time: None,
        }
    }

    /// 保存时写进这个日志，没保存的脚本记在它旁边的草稿文件里。
    ///
    /// 打开知识库时要先调用 [`Journal::restore_draft`] 接上草稿，否则内存里没有文件里的那些脚本，
    /// 保存时它们就丢了。
    pub fn with_journal(mut self, journal: &'a Journal) -> Self {
        self.journal = Some(journal);
        self
    }

    /// 让模型能读内置说明，并保护它不被改动。
    pub fn with_manual(mut self, manual: &'a Manual) -> Self {
        self.manual = Some(manual);
        self
    }

    /// 设置输出的上限，默认是 256 KiB。工具调用的结果比命令行的输出更容易超过客户端的限制。
    pub fn with_output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }

    /// 用固定的时间代替当前时间。
    pub fn with_time(mut self, at: Option<u64>) -> Self {
        self.fixed_time = Some(at);
        self
    }

    /// 执行一段脚本。对任何输入都只会返回文本，不会崩溃。
    pub fn run(&mut self, script: &str) -> Output {
        if script.len() > MAX_SCRIPT_BYTES {
            return failure(
                &[check::too_large("大小", MAX_SCRIPT_BYTES)],
                "",
                self.output_limit,
            );
        }

        let (tokens, mut errors) = lexer::tokenize(script);
        let (statements, parse_errors) = parser::parse(tokens);
        errors.extend(parse_errors);
        if statements.len() > MAX_STATEMENTS {
            errors.push(check::too_large("语句数", MAX_STATEMENTS));
        }
        if !errors.is_empty() {
            errors.sort_by_key(|error| (error.span.line, error.span.column));
            return failure(&errors, script, self.output_limit);
        }

        let errors = check::check(
            &statements,
            self.memory,
            self.sources,
            self.mode,
            self.manual,
        );
        if !errors.is_empty() {
            return failure(&errors, script, self.output_limit);
        }
        if statements.is_empty() {
            return Output {
                ok: true,
                text: "脚本是空的，没有执行任何语句。".to_owned(),
            };
        }

        if statements[0].statement == ast::Statement::Discard {
            return self.discard();
        }
        let writes = statements.iter().any(|s| s.statement.writes());
        let saving = statements.last().map(|s| &s.statement) == Some(&ast::Statement::Save);
        let body = if saving {
            &statements[..statements.len() - 1]
        } else {
            &statements[..]
        };

        // 新原文要最先写：它们不可变，也不影响图，中途失败最多留下没人引用的文件。不用等落盘，
        // 保存时日志里有这段原文，文件丢了能从日志恢复。
        if let Err(error) = self.store_new_sources(&statements, false) {
            return Output {
                ok: false,
                text: format!("写入原文失败，没有执行任何语句：{error}"),
            };
        }

        let at = self.fixed_time.unwrap_or_else(timeline::now);

        if writes {
            // 这段脚本马上就保存的话，不用先记一遍。
            if !saving
                && let Some(journal) = self.journal
                && let Err(error) = journal.append_draft(script, at)
            {
                return Output {
                    ok: false,
                    text: format!("写入草稿失败，没有执行任何语句：{error}"),
                };
            }
            self.memory.begin_draft();
        }

        let report = exec::execute(body, self.memory, self.sources, self.manual, at);
        let report = match report {
            Ok(report) => report,
            // 检查已经通过，只有内存不足才会走到这里。
            Err((done, text, error)) => {
                let shown = truncate(text, self.output_limit, "\n（输出超过上限，被截断了。）\n");
                let number = done + 1;
                if !writes {
                    return Output {
                        ok: false,
                        text: format!(
                            "{shown}error[E900] 第 {number} 条语句执行失败：{error}\n后面的语句没有执行。"
                        ),
                    };
                }
                let scripts = self.memory.draft_scripts().len();
                self.memory.abort_draft();
                if let Some(journal) = self.journal {
                    let _ = journal.clear_draft();
                }
                return Output {
                    ok: false,
                    text: format!(
                        "{shown}error[E900] 第 {number} 条语句执行失败：{error}\n这段脚本和草稿里之前的 {scripts} 段脚本都已经撤销，记忆回到上次保存时的样子。"
                    ),
                };
            }
        };
        if writes {
            self.memory.note_draft_script(DraftScript {
                script: script.to_owned(),
                at,
            });
        }

        let mut text = limit_report(&report, body, self.output_limit);
        let mut ok = report.failures == 0;
        if !ok {
            // 截断之后再写结论，否则结论可能被截掉。
            text.push_str(&format!(
                "有 {} 条语句失败，其余语句都已执行。",
                report.failures
            ));
        }
        if saving {
            match self.save() {
                Ok(message) => text.push_str(&message),
                Err(error) => {
                    ok = false;
                    text.push_str(&format!(
                        "error[E903] 保存失败，修改还在草稿里：{error}\n可以再次 save，或者 discard 放弃。\n"
                    ));
                    // 这段脚本还没记进草稿文件，马上保存才不用记。
                    if writes
                        && let Some(journal) = self.journal
                        && journal.append_draft(script, at).is_err()
                    {
                        text.push_str(
                            "warning: 这段脚本也没能写进草稿文件，进程退出后它的修改会丢失。\n",
                        );
                    }
                }
            }
        }
        if writes && let Some(note) = exec::revision_note(self.memory) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&note);
            text.push('\n');
        }
        Output { ok, text }
    }

    /// 把草稿整个写进日志，一行里放下所有脚本，所以崩溃时要么全部生效，要么都不生效。
    fn save(&mut self) -> Result<String, crate::error::Error> {
        if !self.memory.has_revision() {
            return Ok("没有要保存的修改\n".to_owned());
        }
        let scripts = self.memory.draft_scripts().len();
        if let Some(journal) = self.journal {
            journal.append_scripts(self.memory.draft_scripts())?;
        }
        self.memory.commit_draft();
        let mut message = format!("saved {scripts} 段脚本\n");
        // 删不掉也没关系：重启时发现日志变长了，就知道这份草稿已经保存过。
        if let Some(journal) = self.journal
            && let Err(error) = journal.clear_draft()
        {
            message.push_str(&format!("warning: 没能删除草稿文件：{error}\n"));
        }
        Ok(message)
    }

    fn discard(&mut self) -> Output {
        if !self.memory.has_revision() {
            return Output {
                ok: true,
                text: "没有要放弃的修改\n".to_owned(),
            };
        }
        if let Some(journal) = self.journal
            && let Err(error) = journal.clear_draft()
        {
            return Output {
                ok: false,
                text: format!("放弃失败，修改还在草稿里：{error}\n"),
            };
        }
        let scripts = self.memory.draft_scripts().len();
        self.memory.abort_draft();
        Output {
            ok: true,
            text: format!("discarded {scripts} 段脚本\n"),
        }
    }
}

impl Session<'_> {
    /// 重放日志里的一段脚本，只执行其中的写语句。
    ///
    /// 读语句对恢复记忆没有作用，还可能因为来源损坏而失败，不能让它们妨碍知识库加载。
    pub fn replay(&mut self, script: &str, at: Option<u64>) -> Output {
        let (tokens, mut errors) = lexer::tokenize(script);
        let (mut statements, parse_errors) = parser::parse(tokens);
        errors.extend(parse_errors);
        statements.retain(|located| located.statement.writes());
        // 这些脚本当时已经检查过了。原文后来丢了，不能让整份记忆都打不开。
        let trusted = Trusted(self.sources);
        errors.extend(check::check(
            &statements,
            self.memory,
            &trusted,
            Mode::ReadWrite,
            None,
        ));
        if !errors.is_empty() {
            return failure(&errors, script, MAX_OUTPUT_BYTES);
        }
        // 日志里有原文，文件丢了也能恢复。
        if let Err(error) = self.store_new_sources(&statements, true) {
            return Output {
                ok: false,
                text: error.to_string(),
            };
        }
        match exec::execute(&statements, self.memory, &trusted, None, at) {
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
    fn store_new_sources(
        &self,
        statements: &[ast::Located],
        durable: bool,
    ) -> Result<(), crate::error::Error> {
        for located in statements {
            let text = match &located.statement {
                ast::Statement::Add(add) | ast::Statement::Source(add) => add.text.as_deref(),
                ast::Statement::Cite {
                    citation: ast::Citation::Text(text),
                    ..
                } => Some(text.text.as_str()),
                _ => None,
            };
            if let Some(text) = text {
                if durable {
                    self.sources.put(text.as_bytes())?;
                } else {
                    self.sources.put_unsynced(text.as_bytes())?;
                }
            }
        }
        Ok(())
    }
}

fn failure(errors: &[Diagnostic], script: &str, limit: usize) -> Output {
    Output {
        ok: false,
        text: truncate(
            render(errors, script),
            limit,
            "\n（输出超过上限，后面的内容被截断了。脚本没有执行。）",
        ),
    }
}

/// 截断时说清楚停在哪条语句，否则模型不知道少了什么，也不知道怎么接着看。
fn limit_report(report: &exec::Report, statements: &[ast::Located], limit: usize) -> String {
    if report.text.len() <= limit {
        return report.text.clone();
    }
    let mut note = String::from("\n（输出超过上限，被截断了。语句都已执行，写语句不要重复提交。");
    let cut = report
        .ends
        .iter()
        .position(|end| *end > limit)
        .unwrap_or(statements.len().saturating_sub(1));
    let located = &statements[cut];
    note.push_str(&format!(
        "第 {} 行的 {}。",
        located.span.line,
        exec::continue_hint(&located.statement)
    ));
    let hidden = statements.len() - cut - 1;
    if hidden > 0 {
        note.push_str(&format!(
            "它后面还有 {hidden} 条语句的输出没有显示，从第 {} 行开始。",
            statements[cut + 1].span.line
        ));
    }
    note.push_str("）\n");
    truncate(report.text.clone(), limit, &note)
}

fn truncate(mut text: String, limit: usize, note: &str) -> String {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(note);
    text
}

#[cfg(test)]
mod tests;
