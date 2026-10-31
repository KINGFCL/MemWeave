//! 报错：和编译器一样指出位置、原因和改法。这些文本是写给模型看的，它要靠它们改对脚本。

use std::fmt::Write;

/// 脚本中的一个位置。行和列都从 1 开始，列按字符计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub column: usize,
    pub length: usize,
}

/// 一条报错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn new(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            span,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 把报错渲染成文本，并引用出错的那一行。
pub fn render(diagnostics: &[Diagnostic], script: &str) -> String {
    let lines: Vec<&str> = script.lines().collect();
    let mut output = String::new();
    for diagnostic in diagnostics {
        let span = diagnostic.span;
        let _ = writeln!(
            output,
            "error[{}] 第 {} 行第 {} 列：{}",
            diagnostic.code, span.line, span.column, diagnostic.message
        );
        if let Some(line) = span.line.checked_sub(1).and_then(|index| lines.get(index)) {
            let number = span.line.to_string();
            let _ = writeln!(output, "  {number} | {line}");
            let gutter = " ".repeat(number.len());
            let padding = " ".repeat(span.column.saturating_sub(1));
            let carets = "^".repeat(span.length.max(1));
            let _ = writeln!(output, "  {gutter} | {padding}{carets}");
        }
        if let Some(hint) = &diagnostic.hint {
            let _ = writeln!(output, "  提示：{hint}");
        }
        output.push('\n');
    }
    let _ = write!(
        output,
        "共 {} 个错误，没有执行任何语句。",
        diagnostics.len()
    );
    output
}

/// 在候选词里找最接近的，用于“你是不是想写”的提示。太不像就不提示。
pub fn closest<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).max(2);
    candidates
        .map(|candidate| (edit_distance(word, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, candidate)| (*distance, *candidate))
        .map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, right) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(left != right);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_position_and_hint() {
        let span = Span {
            line: 2,
            column: 6,
            length: 3,
        };
        let diagnostic = Diagnostic::new("E201", span, "节点 abc 不存在").with_hint("换一个");

        let text = render(&[diagnostic], "catalog\nshow abc\n");

        assert_eq!(
            text,
            "error[E201] 第 2 行第 6 列：节点 abc 不存在\n  2 | show abc\n    |      ^^^\n  提示：换一个\n\n共 1 个错误，没有执行任何语句。"
        );
    }

    #[test]
    fn finds_close_words_only() {
        let words = ["ownership", "borrowing"];

        assert_eq!(closest("ownershp", words.into_iter()), Some("ownership"));
        assert_eq!(closest("zzzzzz", words.into_iter()), None);
    }
}
