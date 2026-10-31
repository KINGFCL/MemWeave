//! 词法分析：把脚本切成词。任何输入都只会产生词或报错，不会崩溃。

use super::diagnostic::{Diagnostic, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// 关键字、节点地址、来源地址或数字，具体含义由语法分析决定。
    Word(String),
    /// `"..."`，内容原样。
    Text(String),
    /// 三个或更多引号包住的文字，内容原样。只去掉紧贴开头和结尾的那个换行，要不要去掉别的空白由
    /// 语法分析决定：原文必须逐字保存，描述不必。
    Block(String),
    Arrow,
    DotDot,
    Comma,
    Newline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// 这么多个引号开始的是可以多行的文字。
const BLOCK_QUOTES: usize = 3;

/// 去掉紧贴开头和结尾的那个换行，这样内容可以从下一行写起。其他空白都是内容。
fn strip_edge_newlines(text: &str) -> String {
    let text = text
        .strip_prefix("\r\n")
        .or_else(|| text.strip_prefix('\n'))
        .unwrap_or(text);
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    text.to_owned()
}

pub fn tokenize(script: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        chars: script.chars().collect(),
        position: 0,
        line: 1,
        column: 1,
        tokens: Vec::new(),
        errors: Vec::new(),
    };
    lexer.run();
    (lexer.tokens, lexer.errors)
}

struct Lexer {
    chars: Vec<char>,
    position: usize,
    line: usize,
    column: usize,
    tokens: Vec<Token>,
    errors: Vec<Diagnostic>,
}

impl Lexer {
    fn run(&mut self) {
        while let Some(c) = self.peek(0) {
            let start = self.here(1);
            match c {
                '\n' => {
                    self.advance();
                    self.push(TokenKind::Newline, start);
                }
                c if c.is_whitespace() => {
                    self.advance();
                }
                '#' => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.advance();
                    }
                }
                ',' => {
                    self.advance();
                    self.push(TokenKind::Comma, start);
                }
                '"' => self.text(start),
                '-' if self.peek(1) == Some('>') => {
                    self.advance();
                    self.advance();
                    self.push(TokenKind::Arrow, Span { length: 2, ..start });
                }
                '.' if self.peek(1) == Some('.') => {
                    self.advance();
                    self.advance();
                    self.push(TokenKind::DotDot, Span { length: 2, ..start });
                }
                c if is_word_char(c) => self.word(start),
                c => {
                    self.advance();
                    self.errors.push(
                        Diagnostic::new("E001", start, format!("无法识别的字符 {c:?}"))
                            .with_hint("文字内容要放在引号里；地址只能包含字母、数字、- 和 _"),
                    );
                }
            }
        }
    }

    fn word(&mut self, start: Span) {
        let mut word = String::new();
        while let Some(c) = self.peek(0) {
            // a->b 不写空格时，箭头不能被算进地址里。
            if !is_word_char(c) || (c == '-' && self.peek(1) == Some('>')) {
                break;
            }
            word.push(c);
            self.advance();
        }
        let length = word.chars().count();
        self.push(TokenKind::Word(word), Span { length, ..start });
    }

    fn text(&mut self, start: Span) {
        let quotes = self.quotes_at(0);
        if quotes >= BLOCK_QUOTES {
            self.block_text(start, quotes);
        } else {
            self.line_text(start);
        }
    }

    fn line_text(&mut self, start: Span) {
        self.advance();
        let mut text = String::new();
        loop {
            match self.peek(0) {
                Some('"') => {
                    self.advance();
                    let length = text.chars().count() + 2;
                    self.push(TokenKind::Text(text), Span { length, ..start });
                    return;
                }
                Some('\n') | None => {
                    self.errors
                        .push(Diagnostic::new("E002", start, "字符串没有结束").with_hint(
                            "\"...\" 必须在同一行结束；多行或含引号的内容请用 \"\"\"...\"\"\"",
                        ));
                    return;
                }
                Some(c) => {
                    text.push(c);
                    self.advance();
                }
            }
        }
    }

    /// 从当前位置往后数，连续有多少个引号。
    fn quotes_at(&self, offset: usize) -> usize {
        self.chars[self.position + offset..]
            .iter()
            .take_while(|c| **c == '"')
            .count()
    }

    /// 开头有几个引号，结尾就要有几个，所以内容里可以有更少的连续引号。
    /// 结尾的引号比开头多时，多出来的算内容：`"""他说"好""""` 的内容是 `他说"好"`。
    fn block_text(&mut self, start: Span, quotes: usize) {
        for _ in 0..quotes {
            self.advance();
        }
        let span = Span {
            length: quotes,
            ..start
        };
        let mut text = String::new();
        while let Some(c) = self.peek(0) {
            if c == '"' {
                let run = self.quotes_at(0);
                for _ in 0..run {
                    self.advance();
                }
                if run >= quotes {
                    text.extend(std::iter::repeat_n('"', run - quotes));
                    self.push(TokenKind::Block(strip_edge_newlines(&text)), span);
                    return;
                }
                text.extend(std::iter::repeat_n('"', run));
                continue;
            }
            text.push(c);
            self.advance();
        }
        let delimiter = "\"".repeat(quotes);
        self.errors.push(
            Diagnostic::new("E003", span, format!("{delimiter} 没有结束"))
                .with_hint(format!("在内容末尾补上 {delimiter}")),
        );
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        self.tokens.push(Token { kind, span });
    }

    fn here(&self, length: usize) -> Span {
        Span {
            line: self.line,
            column: self.column,
            length,
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.position + offset).copied()
    }

    fn advance(&mut self) {
        if let Some(c) = self.peek(0) {
            self.position += 1;
            if c == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == ':'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(script: &str) -> Vec<TokenKind> {
        let (tokens, errors) = tokenize(script);
        assert!(errors.is_empty(), "{errors:?}");
        tokens.into_iter().map(|token| token.kind).collect()
    }

    fn word(text: &str) -> TokenKind {
        TokenKind::Word(text.to_owned())
    }

    #[test]
    fn splits_words_and_symbols() {
        assert_eq!(
            kinds("link a-b->c # 注释\nsource sha256:ab 1..9, x"),
            [
                word("link"),
                word("a-b"),
                TokenKind::Arrow,
                word("c"),
                TokenKind::Newline,
                word("source"),
                word("sha256:ab"),
                word("1"),
                TokenKind::DotDot,
                word("9"),
                TokenKind::Comma,
                word("x"),
            ]
        );
    }

    #[test]
    fn block_text_keeps_quotes_and_newlines() {
        assert_eq!(
            kinds("\"\"\"\n他说\"你好\"\n第二行\n\"\"\""),
            [TokenKind::Block("他说\"你好\"\n第二行".to_owned())]
        );
    }

    #[test]
    fn line_numbers_follow_block_text() {
        let (tokens, _) = tokenize("\"\"\"a\nb\"\"\" x");

        assert_eq!(tokens[1].span.line, 2);
        assert_eq!(tokens[1].span.column, 6);
    }

    #[test]
    fn reports_bad_input_without_stopping() {
        let (_, errors) = tokenize("show @ \"没结束\nshow \"\"\"也没结束");

        let codes: Vec<&str> = errors.iter().map(|error| error.code).collect();
        assert_eq!(codes, ["E001", "E002", "E003"]);
    }
}
