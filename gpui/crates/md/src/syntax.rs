//! Syntax highlighting for fenced code.
//!
//! # A lexer, not a parser
//!
//! zeron highlights through Tree-sitter. We do not carry a grammar per
//! language, and for a chat transcript we do not need one: what a reader wants
//! from a code block is to tell a comment from a string from a keyword at a
//! glance, and that separation is *lexical*. So this is one scanner over a
//! per-language `Lang` description — comment markers, string delimiters,
//! keyword sets — and a closed [`TokenKind`] vocabulary the palette resolves.
//!
//! The cost of the shortcut is stated honestly: a language the table does not
//! know is not guessed at, it is [`None`], and the block paints plain. There is
//! no half-highlighted state where a Rust `'a` lifetime is painted as an
//! unterminated char literal for the rest of the file.
//!
//! # Highlighting can never move anything
//!
//! Tokens are byte ranges into the block's own source and are painted as
//! recolored `TextRun`s on the identical mono font, so a block highlights,
//! stops highlighting, or changes palette without a relayout. That is the
//! invariant the [`crate::Highlighter`] seam exists to keep, and it is why
//! this module may be as approximate as it likes.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::Arc;

use crate::theme::{theme_generation, Theme};
use crate::{HighlightSpan, HighlightedCode, Highlighter};

/// What a token is, in the smallest vocabulary that reads as highlighting.
///
/// Closed and deliberately coarse: every distinction here is one a reader can
/// name, and a kind nothing paints differently would be a token type with no
/// design decision behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// A line or block comment, including its markers.
    Comment,
    /// A reserved word.
    Keyword,
    /// A quoted literal, including its delimiters.
    String,
    /// A numeric literal.
    Number,
    /// `true`, `null`, and the language's other spelled-out literals.
    Constant,
    /// A built-in or capitalized type name.
    Type,
    /// An identifier applied to an argument list.
    Function,
    /// A key in an object literal — a string that is followed by `:`.
    Property,
    /// Everything else: identifiers, operators, whitespace.
    Plain,
}

/// One language's lexical surface. Everything the scanner needs and nothing
/// about its grammar.
struct Lang {
    keywords: &'static [&'static str],
    constants: &'static [&'static str],
    types: &'static [&'static str],
    line_comment: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// String delimiters, **longest first** — `"""` has to be tried before `"`.
    strings: &'static [&'static str],
    /// Whether `\` escapes the next byte inside a string.
    escape: bool,
    /// Whether a capitalized identifier is a type. True where the language's
    /// convention actually says so, false where it would paint every enum
    /// variant and constant as a type.
    caps_are_types: bool,
    /// Whether a string followed by `:` is an object key.
    keyed_strings: bool,
}

const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield", "match", "case",
    ],
    constants: &["True", "False", "None", "self", "cls"],
    types: &[
        "bool", "bytes", "dict", "float", "int", "list", "set", "str", "tuple",
    ],
    line_comment: &["#"],
    block_comment: None,
    strings: &["\"\"\"", "'''", "\"", "'"],
    escape: true,
    caps_are_types: false,
    keyed_strings: false,
};

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type", "unsafe",
        "use", "where", "while",
    ],
    constants: &["true", "false", "None", "Some", "Ok", "Err"],
    types: &[
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "isize", "str", "u8", "u16",
        "u32", "u64", "usize", "String", "Vec", "Option", "Result",
    ],
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &["\""],
    escape: true,
    caps_are_types: true,
    keyed_strings: false,
};

const JAVASCRIPT: Lang = Lang {
    keywords: &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "return",
        "static",
        "switch",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "yield",
    ],
    constants: &["true", "false", "null", "undefined", "this", "NaN"],
    types: &[
        "Array", "boolean", "number", "object", "Object", "Promise", "string", "String",
    ],
    line_comment: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: &["\"", "'", "`"],
    escape: true,
    caps_are_types: false,
    keyed_strings: true,
};

const JSON: Lang = Lang {
    keywords: &[],
    constants: &["true", "false", "null"],
    types: &[],
    line_comment: &[],
    block_comment: None,
    strings: &["\""],
    escape: true,
    caps_are_types: false,
    keyed_strings: true,
};

const SHELL: Lang = Lang {
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "then", "until", "while",
    ],
    constants: &["true", "false"],
    types: &[
        "cargo", "cd", "echo", "git", "grep", "ls", "mkdir", "rm", "sed",
    ],
    line_comment: &["#"],
    block_comment: None,
    strings: &["\"", "'"],
    escape: true,
    caps_are_types: false,
    keyed_strings: false,
};

/// The language a fence's info string names, if the table knows it.
fn lang_for(name: &str) -> Option<&'static Lang> {
    // The info string may carry more than a name (`rust,ignore`, `python{1}`).
    let name = name
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '+' && c != '#')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match name.as_str() {
        "py" | "python" | "python3" => Some(&PYTHON),
        "rs" | "rust" => Some(&RUST),
        "js" | "jsx" | "javascript" | "ts" | "tsx" | "typescript" => Some(&JAVASCRIPT),
        "json" | "jsonc" => Some(&JSON),
        "sh" | "bash" | "zsh" | "shell" | "console" => Some(&SHELL),
        _ => None,
    }
}

/// Every token in `code`, in order and covering it exactly, or `None` for a
/// language the table does not know.
///
/// Exact cover is the contract the renderer relies on: with no gaps there is
/// no untokenized text for a default color to apply to, so the block's paint
/// is decided entirely here.
#[must_use]
pub fn tokens(language: Option<&str>, code: &str) -> Option<Vec<(Range<usize>, TokenKind)>> {
    let lang = lang_for(language?)?;
    Some(lex(lang, code))
}

fn lex(lang: &Lang, code: &str) -> Vec<(Range<usize>, TokenKind)> {
    let mut out: Vec<(Range<usize>, TokenKind)> = Vec::new();
    let mut at = 0usize;
    while at < code.len() {
        let rest = &code[at..];

        if lang.line_comment.iter().any(|m| rest.starts_with(m)) {
            let end = rest.find('\n').map_or(code.len(), |n| at + n);
            push(&mut out, at..end, TokenKind::Comment);
            at = end;
            continue;
        }

        if let Some((open, close)) = lang.block_comment {
            if let Some(body) = rest.strip_prefix(open) {
                let end = body
                    .find(close)
                    .map_or(code.len(), |n| at + open.len() + n + close.len());
                push(&mut out, at..end, TokenKind::Comment);
                at = end;
                continue;
            }
        }

        if let Some(delim) = lang.strings.iter().find(|d| rest.starts_with(**d)) {
            let end = string_end(code, at, delim, lang.escape);
            let kind = if lang.keyed_strings && next_nonspace(code, end) == Some(b':') {
                TokenKind::Property
            } else {
                TokenKind::String
            };
            push(&mut out, at..end, kind);
            at = end;
            continue;
        }

        let byte = code.as_bytes()[at];
        if byte.is_ascii_digit() {
            let end = run_while(code, at, |b| {
                b.is_ascii_alphanumeric() || b == b'.' || b == b'_'
            });
            push(&mut out, at..end, TokenKind::Number);
            at = end;
            continue;
        }

        if byte.is_ascii_alphabetic() || byte == b'_' {
            let end = run_while(code, at, |b| b.is_ascii_alphanumeric() || b == b'_');
            let word = &code[at..end];
            let kind = if lang.keywords.contains(&word) {
                TokenKind::Keyword
            } else if lang.constants.contains(&word) {
                TokenKind::Constant
            } else if lang.types.contains(&word) {
                TokenKind::Type
            } else if next_nonspace(code, end) == Some(b'(') {
                TokenKind::Function
            } else if lang.caps_are_types && word.starts_with(|c: char| c.is_ascii_uppercase()) {
                TokenKind::Type
            } else {
                TokenKind::Plain
            };
            push(&mut out, at..end, kind);
            at = end;
            continue;
        }

        // Whitespace and operators. Advanced by whole characters so every range
        // this function emits lands on a char boundary, which `&code[range]`
        // and the veil's slicing both require.
        let step = rest.chars().next().map_or(1, char::len_utf8);
        push(&mut out, at..at + step, TokenKind::Plain);
        at += step;
    }
    out
}

/// Append a token, fusing it with the previous one when they are the same kind
/// and touch — the renderer turns each token into a `TextRun`, and a run per
/// character would be a shaping cost with no paint to show for it.
fn push(out: &mut Vec<(Range<usize>, TokenKind)>, range: Range<usize>, kind: TokenKind) {
    if range.is_empty() {
        return;
    }
    match out.last_mut() {
        Some((last, last_kind)) if *last_kind == kind && last.end == range.start => {
            last.end = range.end;
        }
        _ => out.push((range, kind)),
    }
}

/// Where the string opened by `delim` at `at` ends — past its closing
/// delimiter, or at the end of the block for one that never closes.
///
/// Running to the end rather than to the newline is what makes a triple-quoted
/// docstring one token, and it is also what a *streaming* half-typed string
/// wants: the tail paints as string until its quote arrives, and nothing moves
/// when it does.
fn string_end(code: &str, at: usize, delim: &str, escape: bool) -> usize {
    let mut ix = at + delim.len();
    while ix < code.len() {
        let rest = &code[ix..];
        if escape && rest.starts_with('\\') {
            ix += rest.chars().take(2).map(char::len_utf8).sum::<usize>();
            continue;
        }
        if rest.starts_with(delim) {
            return ix + delim.len();
        }
        ix += rest.chars().next().map_or(1, char::len_utf8);
    }
    code.len()
}

fn run_while(code: &str, at: usize, mut keep: impl FnMut(u8) -> bool) -> usize {
    let bytes = code.as_bytes();
    let mut ix = at;
    while ix < bytes.len() && keep(bytes[ix]) {
        ix += 1;
    }
    ix
}

/// The next byte at or after `at` that is not a space or tab. Newlines stop the
/// search: a call's parenthesis is on the identifier's own line.
fn next_nonspace(code: &str, at: usize) -> Option<u8> {
    code.as_bytes()[at..]
        .iter()
        .copied()
        .find(|b| *b != b' ' && *b != b'\t')
        .filter(|b| *b != b'\n')
}

// -- the highlighter ---------------------------------------------------------

/// How many derivations are kept. Small because a panel shows a handful of
/// code blocks and a streaming one asks about a *growing* body of code — the
/// entries a stream leaves behind are the ones worth dropping first.
const MEMO_CAPACITY: usize = 16;

struct Memo {
    generation: u32,
    language: String,
    code: String,
    result: Arc<HighlightedCode>,
}

thread_local! {
    /// Derivations, most recent first.
    ///
    /// Process-local rather than owned by a [`Syntax`] because the renderer's
    /// cross-frame cache keys on the returned `Arc`'s identity: the same block
    /// has to hand back the *same allocation* every frame or its `TextRun`s
    /// rebuild each time, and the caller who has the block is a free function
    /// with nowhere to keep one. Keying on the source text makes that safe —
    /// this memoizes a pure function, so who is asking cannot matter.
    static MEMO: RefCell<Vec<Memo>> = const { RefCell::new(Vec::new()) };
}

/// The lexer, as a [`Highlighter`].
///
/// Cheap to construct: it holds a resolved palette and nothing else, so the
/// renderer's call site can build one per frame rather than threading a
/// long-lived object through the transcript.
pub struct Syntax {
    palette: crate::theme::SyntaxPalette,
}

impl Syntax {
    /// A highlighter painting in `theme`'s syntax palette.
    #[must_use]
    pub fn new(theme: &Theme) -> Self {
        Self {
            palette: theme.syntax.clone(),
        }
    }
}

impl Highlighter for Syntax {
    fn highlight(&self, language: Option<&str>, code: &str) -> Option<Arc<HighlightedCode>> {
        let language = language?;
        let generation = theme_generation();
        if let Some(hit) = MEMO.with_borrow_mut(|memo| {
            let ix = memo.iter().position(|entry| {
                entry.generation == generation && entry.language == language && entry.code == code
            })?;
            let entry = memo.remove(ix);
            let result = entry.result.clone();
            memo.insert(0, entry);
            Some(result)
        }) {
            return Some(hit);
        }

        let tokens = tokens(Some(language), code)?;
        let mut lines: Vec<Vec<HighlightSpan>> = vec![Vec::new(); code.split('\n').count()];
        let mut line = 0usize;
        let mut line_start = 0usize;
        for (range, kind) in tokens {
            let color = self.palette.color(kind);
            // One token can straddle newlines (a docstring, a block comment);
            // the renderer paints per line, so split it there.
            let mut at = range.start;
            while at < range.end {
                let newline = code[at..range.end].find('\n').map(|n| at + n);
                let end = newline.unwrap_or(range.end);
                if end > at {
                    lines[line].push(HighlightSpan {
                        range: at - line_start..end - line_start,
                        color,
                    });
                }
                match newline {
                    Some(nl) => {
                        line += 1;
                        line_start = nl + 1;
                        at = nl + 1;
                    }
                    None => at = end,
                }
            }
        }

        let result = Arc::new(HighlightedCode { lines });
        MEMO.with_borrow_mut(|memo| {
            memo.insert(
                0,
                Memo {
                    generation,
                    language: language.to_string(),
                    code: code.to_string(),
                    result: result.clone(),
                },
            );
            memo.truncate(MEMO_CAPACITY);
        });
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(language: &str, code: &str) -> Vec<(String, TokenKind)> {
        tokens(Some(language), code)
            .expect("a known language")
            .into_iter()
            .map(|(range, kind)| (code[range].to_string(), kind))
            .filter(|(text, kind)| !(*kind == TokenKind::Plain && text.trim().is_empty()))
            .collect()
    }

    /// The whole point: four different things paint four different colors.
    #[test]
    fn a_python_line_separates_its_token_kinds() {
        let got = kinds("python", "def f(x):  # note\n    return \"hi\", 4\n");
        assert!(got.contains(&("def".into(), TokenKind::Keyword)));
        assert!(got.contains(&("f".into(), TokenKind::Function)));
        assert!(got.contains(&("# note".into(), TokenKind::Comment)));
        assert!(got.contains(&("return".into(), TokenKind::Keyword)));
        assert!(got.contains(&("\"hi\"".into(), TokenKind::String)));
        assert!(got.contains(&("4".into(), TokenKind::Number)));
    }

    /// Exact cover is what lets the renderer skip a default color entirely.
    #[test]
    fn tokens_cover_the_source_exactly() {
        let code = "fn main() {\n    let x = 1; // hi\n}\n";
        let got = tokens(Some("rust"), code).expect("a known language");
        let mut at = 0;
        for (range, _) in &got {
            assert_eq!(range.start, at, "gap or overlap before {range:?}");
            at = range.end;
        }
        assert_eq!(at, code.len());
    }

    /// An unterminated string is the normal *streaming* state, not an error:
    /// it runs to the end of what has arrived and settles when the quote does.
    #[test]
    fn an_unclosed_string_runs_to_the_end() {
        let got = kinds("python", "x = \"half");
        assert_eq!(got.last(), Some(&("\"half".to_string(), TokenKind::String)));
    }

    /// A docstring is one token across lines, and the per-line split is the
    /// renderer's problem, not the lexer's.
    #[test]
    fn a_triple_quoted_string_spans_lines() {
        let got = kinds("python", "\"\"\"one\ntwo\"\"\"\n");
        assert_eq!(got[0].1, TokenKind::String);
        assert_eq!(got[0].0, "\"\"\"one\ntwo\"\"\"");
    }

    #[test]
    fn json_keys_and_values_read_apart() {
        let got = kinds("json", "{\"a\": \"b\", \"c\": true}");
        assert!(got.contains(&("\"a\"".into(), TokenKind::Property)));
        assert!(got.contains(&("\"b\"".into(), TokenKind::String)));
        assert!(got.contains(&("true".into(), TokenKind::Constant)));
    }

    /// A language the table does not know is not guessed at.
    #[test]
    fn an_unknown_language_is_not_highlighted() {
        assert!(tokens(Some("brainfuck"), "+++").is_none());
        assert!(tokens(None, "+++").is_none());
    }

    /// The info string can carry more than a name.
    #[test]
    fn a_decorated_info_string_still_names_its_language() {
        assert!(tokens(Some("rust,ignore"), "fn f() {}").is_some());
    }

    #[test]
    fn spans_are_grouped_by_line() {
        let theme = Theme::dark();
        let syntax = Syntax::new(&theme);
        let highlighted = syntax
            .highlight(Some("python"), "def f():\n    pass\n")
            .expect("a known language");
        assert_eq!(highlighted.lines.len(), 3);
        assert!(highlighted.lines[2].is_empty());
        for (li, line) in highlighted.lines.iter().enumerate() {
            let len = "def f():\n    pass\n".split('\n').nth(li).unwrap().len();
            assert!(line.iter().all(|span| span.range.end <= len));
        }
    }

    /// The renderer's cross-frame cache keys on the `Arc`'s identity, so the
    /// same block asked twice has to hand back the same allocation.
    #[test]
    fn the_same_block_memoizes_to_one_allocation() {
        let theme = Theme::dark();
        let syntax = Syntax::new(&theme);
        let first = syntax.highlight(Some("rust"), "fn f() {}").unwrap();
        let second = syntax.highlight(Some("rust"), "fn f() {}").unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
}
