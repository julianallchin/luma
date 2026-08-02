use super::types::{Loc, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    At,
    Dash,
    Colon,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    Comma,
    Equals,
    And,
    Or,
    Xor,
    Not,
    Fallback,
    HexColor,
    Number,
    String,
    Identifier,
    Unknown,
    Newline,
    Comment,
    Eof,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub value: String,
    pub span: Span,
}

pub fn tokenize(source: &str) -> Vec<Token> {
    Tokenizer::new(source).run()
}

struct Tokenizer<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    offset: usize,
    line: usize,
    column: usize,
}

impl<'a> Tokenizer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            tokens: Vec::new(),
            offset: 0,
            line: 1,
            column: 0,
        }
    }

    fn run(mut self) -> Vec<Token> {
        while self.offset < self.source.len() {
            self.skip_inline_whitespace();
            if self.offset >= self.source.len() {
                break;
            }

            let start = self.current();
            let character = self.peek().expect("offset is in bounds");
            match character {
                '\n' | '\r' => self.newline(start, character),
                '#' => self.color_or_comment(start),
                '@' => self.single(TokenKind::At, "@", start),
                '-' => self.single(TokenKind::Dash, "-", start),
                ':' => self.single(TokenKind::Colon, ":", start),
                '(' => self.single(TokenKind::LeftParen, "(", start),
                ')' => self.single(TokenKind::RightParen, ")", start),
                '[' => self.single(TokenKind::LeftBracket, "[", start),
                ']' => self.single(TokenKind::RightBracket, "]", start),
                '{' => self.single(TokenKind::LeftBrace, "{", start),
                '}' => self.single(TokenKind::RightBrace, "}", start),
                ',' => self.single(TokenKind::Comma, ",", start),
                '=' => self.single(TokenKind::Equals, "=", start),
                '&' => self.single(TokenKind::And, "&", start),
                '|' => self.single(TokenKind::Or, "|", start),
                '^' => self.single(TokenKind::Xor, "^", start),
                '~' => self.single(TokenKind::Not, "~", start),
                '>' => self.single(TokenKind::Fallback, ">", start),
                '"' => self.string(start),
                value if value.is_ascii_digit() => self.number(start),
                value if value.is_ascii_alphabetic() || value == '_' => self.identifier(start),
                _ => {
                    let value = self.advance().expect("offset is in bounds").to_string();
                    self.push(TokenKind::Unknown, value, start);
                }
            }
        }

        let end = self.current();
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            value: String::new(),
            span: Span { start: end, end },
        });
        self.tokens
    }

    fn peek(&self) -> Option<char> {
        self.source.get(self.offset..)?.chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        self.column += 1;
        Some(character)
    }

    fn current(&self) -> Loc {
        Loc {
            line: self.line,
            column: self.column,
            offset: self.offset,
        }
    }

    fn push(&mut self, kind: TokenKind, value: String, start: Loc) {
        self.tokens.push(Token {
            kind,
            value,
            span: Span {
                start,
                end: self.current(),
            },
        });
    }

    fn single(&mut self, kind: TokenKind, value: &str, start: Loc) {
        self.advance();
        self.push(kind, value.to_owned(), start);
    }

    fn skip_inline_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.advance();
        }
    }

    fn newline(&mut self, start: Loc, first: char) {
        self.advance();
        if first == '\r' && self.peek() == Some('\n') {
            self.advance();
        }
        self.push(TokenKind::Newline, "\n".to_owned(), start);
        self.line += 1;
        self.column = 0;
    }

    fn color_or_comment(&mut self, start: Loc) {
        let after_equals = self
            .tokens
            .last()
            .is_some_and(|token| token.kind == TokenKind::Equals);
        if after_equals {
            let tail = &self.source[self.offset + 1..];
            for digits in [8, 6] {
                let candidate = tail.get(..digits);
                if candidate.is_some_and(|value| {
                    value.len() == digits && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    self.advance();
                    for _ in 0..digits {
                        self.advance();
                    }
                    let value = self.source[start.offset..self.offset].to_owned();
                    self.push(TokenKind::HexColor, value, start);
                    return;
                }
            }
        }

        self.advance();
        let content_start = self.offset;
        while !matches!(self.peek(), None | Some('\n' | '\r')) {
            self.advance();
        }
        let value = self.source[content_start..self.offset].trim().to_owned();
        self.push(TokenKind::Comment, value, start);
    }

    fn string(&mut self, start: Loc) {
        let raw_start = self.offset;
        self.advance();
        let mut escaped = false;
        let mut terminated = false;
        while let Some(character) = self.advance() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                terminated = true;
                break;
            }
        }
        let raw = &self.source[raw_start..self.offset];
        if terminated {
            if let Ok(value) = serde_json::from_str::<String>(raw) {
                self.push(TokenKind::String, value, start);
                return;
            }
        }
        self.push(TokenKind::Unknown, raw.to_owned(), start);
    }

    fn number(&mut self, start: Loc) {
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.advance();
        }
        if self.peek() == Some('.') {
            self.advance();
            while self.peek().is_some_and(|value| value.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.advance();
            if matches!(self.peek(), Some('+' | '-')) {
                self.advance();
            }
            while self.peek().is_some_and(|value| value.is_ascii_digit()) {
                self.advance();
            }
        }
        self.push(
            TokenKind::Number,
            self.source[start.offset..self.offset].to_owned(),
            start,
        );
    }

    fn identifier(&mut self, start: Loc) {
        while self
            .peek()
            .is_some_and(|value| value.is_ascii_alphanumeric() || value == '_')
        {
            self.advance();
        }
        self.push(
            TokenKind::Identifier,
            self.source[start.offset..self.offset].to_owned(),
            start,
        );
    }
}
