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

/// 在候选词里找最接近的，用于“你是不是想写”的提示。太不像就不提示。距离相同时取字典序最小的。
///
/// 节点地址写错时要和图里每个节点比较，所以这里不能慢：已经找到的最好结果就是后面候选的上限，
/// 超过上限的候选不用算完。
pub fn closest<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let word: Vec<char> = word.chars().collect();
    let mut limit = (word.len() / 3).max(2);
    let mut best: Option<(usize, &'a str)> = None;
    let mut other = Vec::new();
    let mut row = Vec::new();
    for candidate in candidates {
        // 一个字符占 1 到 4 个字节，不用解码就能排除长度差太多的候选。
        if candidate.len() + limit < word.len() || candidate.len() > (word.len() + limit) * 4 {
            continue;
        }
        other.clear();
        other.extend(candidate.chars());
        let Some(distance) = distance_within(&word, &other, limit, &mut row) else {
            continue;
        };
        if best.is_none_or(|found| (distance, candidate) < found) {
            best = Some((distance, candidate));
            limit = distance;
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// 编辑距离。超过 `limit` 时返回 `None`，这时不用算完。
fn distance_within(a: &[char], b: &[char], limit: usize, row: &mut Vec<usize>) -> Option<usize> {
    // 相同的头尾不影响距离。
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if long.len() - short.len() > limit {
        return None;
    }
    if short.is_empty() {
        return Some(long.len());
    }

    // 距离不超过上限时，路径离对角线不会超过上限，所以每行只算这一段。
    let over = limit + 1;
    row.clear();
    row.extend((0..=short.len()).map(|j| j.min(over)));
    for (i, right) in long.iter().enumerate() {
        let i = i + 1;
        let low = i.saturating_sub(limit).max(1);
        let high = (i + limit).min(short.len());
        let mut diagonal = row[low - 1];
        row[low - 1] = if low == 1 { i.min(over) } else { over };
        let mut smallest = row[low - 1];
        for j in low..=high {
            let above = row[j];
            let value = (diagonal + usize::from(short[j - 1] != *right))
                .min(above + 1)
                .min(row[j - 1] + 1)
                .min(over);
            diagonal = above;
            row[j] = value;
            smallest = smallest.min(value);
        }
        if high < short.len() {
            row[high + 1] = over;
        }
        if smallest > limit {
            return None;
        }
    }
    let distance = row[short.len()];
    (distance <= limit).then_some(distance)
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

    /// 优化前的写法，用来核对结果没有变。
    fn slow_distance(a: &str, b: &str) -> usize {
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

    fn slow_closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
        let limit = (word.chars().count() / 3).max(2);
        candidates
            .iter()
            .map(|candidate| (slow_distance(word, candidate), *candidate))
            .filter(|(distance, _)| *distance <= limit)
            .min_by_key(|(distance, candidate)| (*distance, *candidate))
            .map(|(_, candidate)| candidate)
    }

    /// 字母少，汉字和字母混用，这样会有大量相近的词和距离相同的情况。
    fn random_word(state: &mut u64, longest: u64) -> String {
        let mut next = || {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        };
        let length = next() % (longest + 1);
        (0..length)
            .map(|_| ['a', 'b', 'c', '-', '记', '忆'][(next() % 6) as usize])
            .collect()
    }

    #[test]
    fn gives_the_same_answer_as_the_full_calculation() {
        let mut state = 0x9e37_79b9_7f4a_7c15;
        for longest in [3, 8, 30] {
            for _ in 0..3000 {
                let word = random_word(&mut state, longest);
                let words: Vec<String> =
                    (0..12).map(|_| random_word(&mut state, longest)).collect();
                let refs: Vec<&str> = words.iter().map(String::as_str).collect();
                assert_eq!(
                    closest(&word, refs.iter().copied()),
                    slow_closest(&word, &refs),
                    "{word:?} {refs:?}"
                );
            }
        }
    }

    #[test]
    fn distance_within_stops_at_the_limit_and_is_exact_below_it() {
        let mut state = 0x1234_5678_9abc_def1;
        let mut row = Vec::new();
        for _ in 0..20_000 {
            let left = random_word(&mut state, 12);
            let right = random_word(&mut state, 12);
            let a: Vec<char> = left.chars().collect();
            let b: Vec<char> = right.chars().collect();
            let exact = slow_distance(&left, &right);
            for limit in 0..=8 {
                let expected = (exact <= limit).then_some(exact);
                assert_eq!(
                    distance_within(&a, &b, limit, &mut row),
                    expected,
                    "{left:?} {right:?} 上限 {limit}"
                );
            }
        }
    }
}
