//! Pine Script lexer for syntax colouring, and the palette sampled from TradingView's dark theme.
//!
//! This file depends on `std` only: it knows nothing about egui, so it can be tested alone.
//!
//! The lexer is tolerant of half-typed code: an unterminated string runs to the end of its line
//! and an unknown character is skipped, where the compiler's lexer would report an error.

use std::ops::Range;

/// What a coloured piece of source is. Everything else stays in the plain colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    Comment,
    /// `if`, `for`, `var`, `and`, `=>` ...
    Keyword,
    /// `int`, `float`, `string`, `color`, `array<...>`, `series`, `simple` ...
    Type,
    /// `true`, `false`, `na`
    Constant,
    Number,
    /// `#RRGGBB` and `#RRGGBBAA`
    Color,
    Str,
    /// A built-in call, with its namespace: `ta.sma`, `input.int`, `indicator`
    BuiltinFn,
    /// A built-in variable or constant, with its namespace: `close`, `timeframe.isdaily`
    BuiltinVar,
    /// The name in `name(args) =>`
    FnDecl,
}

/// One colour per token class, as `[r, g, b]`.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub plain: [u8; 3],
    pub comment: [u8; 3],
    pub keyword: [u8; 3],
    pub ty: [u8; 3],
    pub constant: [u8; 3],
    pub number: [u8; 3],
    pub color: [u8; 3],
    pub string: [u8; 3],
    pub builtin_fn: [u8; 3],
    pub builtin_var: [u8; 3],
    pub fn_decl: [u8; 3],
}

impl Palette {
    pub fn of(&self, token: Token) -> [u8; 3] {
        match token {
            Token::Comment => self.comment,
            Token::Keyword => self.keyword,
            Token::Type => self.ty,
            Token::Constant => self.constant,
            Token::Number => self.number,
            Token::Color => self.color,
            Token::Str => self.string,
            Token::BuiltinFn => self.builtin_fn,
            Token::BuiltinVar => self.builtin_var,
            Token::FnDecl => self.fn_decl,
        }
    }
}

/// TradingView, dark theme. Sampled from a screenshot of the Pine Editor with a pixel picker:
/// plain, comment, keyword, built-in call, user function name, built-in variable/constant,
/// number, string and `#hex` colour are measured. `ty` is NOT sampled (the screenshots held no
/// type names); it reuses the keyword colour. Retune any entry here, nothing else depends on it.
pub const DARK: Palette = Palette {
    plain: [0xdb, 0xdb, 0xdb],
    comment: [0x80, 0x80, 0x80],
    keyword: [0x77, 0xba, 0xa8],
    ty: [0x77, 0xba, 0xa8],
    constant: [0xdc, 0x84, 0x82],
    number: [0xdb, 0x86, 0x2b],
    color: [0xdc, 0x84, 0x82],
    string: [0xdb, 0x84, 0x22],
    builtin_fn: [0x74, 0x9b, 0xf3],
    builtin_var: [0xdc, 0x84, 0x82],
    fn_decl: [0x51, 0x79, 0xf1],
};

/// Light theme. NOT sampled: a plausible guess, to be replaced once a light screenshot exists.
pub const LIGHT: Palette = Palette {
    plain: [0x13, 0x17, 0x22],
    comment: [0x78, 0x7b, 0x86],
    keyword: [0x08, 0x99, 0x81],
    ty: [0x08, 0x99, 0x81],
    constant: [0xc6, 0x3b, 0x3a],
    number: [0xd9, 0x6a, 0x00],
    color: [0xc6, 0x3b, 0x3a],
    string: [0xd9, 0x6a, 0x00],
    builtin_fn: [0x29, 0x62, 0xff],
    builtin_var: [0xc6, 0x3b, 0x3a],
    fn_decl: [0x18, 0x48, 0xcc],
};

const KEYWORDS: &[&str] = &[
    "and", "as", "break", "by", "continue", "else", "enum", "export", "for", "if", "import", "in",
    "method", "not", "or", "return", "switch", "to", "type", "var", "varip", "while",
];

/// Names that are types when written alone and namespaces or calls when followed by `.` or `(`.
const TYPES: &[&str] = &[
    "int", "float", "bool", "string", "color", "line", "label", "box", "table", "array", "matrix",
    "map", "linefill", "polyline", "series", "simple", "const",
];

/// Built-in namespaces; `ns.member` colours as one piece.
const NAMESPACES: &[&str] = &[
    "adjustment",
    "array",
    "backadjustment",
    "barstate",
    "box",
    "chart",
    "color",
    "currency",
    "dayofweek",
    "display",
    "dividends",
    "earnings",
    "extend",
    "font",
    "format",
    "hline",
    "input",
    "label",
    "line",
    "linefill",
    "location",
    "log",
    "map",
    "math",
    "matrix",
    "order",
    "plot",
    "polyline",
    "position",
    "request",
    "runtime",
    "scale",
    "session",
    "settlement_as_close",
    "shape",
    "size",
    "splits",
    "str",
    "strategy",
    "syminfo",
    "ta",
    "table",
    "text",
    "timeframe",
    "xloc",
    "yloc",
];

/// Built-in functions that need no namespace.
const BUILTIN_FNS: &[&str] = &[
    "alert",
    "alertcondition",
    "barcolor",
    "bgcolor",
    "fill",
    "fixnan",
    "hline",
    "indicator",
    "library",
    "max_bars_back",
    "nz",
    "plot",
    "plotarrow",
    "plotbar",
    "plotcandle",
    "plotchar",
    "plotshape",
    "strategy",
    "study",
    "timestamp",
];

/// Built-in variables that need no namespace. Several are also functions: `time(tf)`.
const BUILTIN_VARS: &[&str] = &[
    "bar_index",
    "close",
    "dayofmonth",
    "dayofweek",
    "high",
    "hl2",
    "hlc3",
    "hlcc4",
    "hour",
    "last_bar_index",
    "last_bar_time",
    "low",
    "minute",
    "month",
    "ohlc4",
    "open",
    "second",
    "time",
    "time_close",
    "time_tradingday",
    "timenow",
    "volume",
    "weekofyear",
    "year",
];

/// The coloured spans of `text`, in source order, never overlapping. No span crosses a newline.
pub fn tokenize(text: &str) -> Vec<(Range<usize>, Token)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let Some(c) = rest.chars().next() else { break };
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(text.len(), |newline| at + newline);
            out.push((at..end, Token::Comment));
            at = end;
        } else if c == '"' || c == '\'' {
            let end = string_end(text, at, c);
            out.push((at..end, Token::Str));
            at = end;
        } else if c == '#' {
            let digits = rest[1..]
                .chars()
                .take_while(|digit| digit.is_ascii_hexdigit())
                .count();
            // A colour literal is six or eight hex digits; anything else stays plain.
            if digits == 6 || digits == 8 {
                out.push((at..at + 1 + digits, Token::Color));
                at += 1 + digits;
            } else {
                at += 1;
            }
        } else if rest.starts_with("=>") {
            out.push((at..at + 2, Token::Keyword));
            at += 2;
        } else if c.is_ascii_digit()
            || (c == '.' && rest[1..].starts_with(|digit: char| digit.is_ascii_digit()))
        {
            let end = number_end(text, at);
            out.push((at..end, Token::Number));
            at = end;
        } else if c.is_alphabetic() || c == '_' {
            at = word(text, at, &mut out);
        } else {
            at += c.len_utf8();
        }
    }
    out
}

/// Classifies the identifier (or built-in `ns.member` chain) at `at`; returns where it ends.
fn word(text: &str, at: usize, out: &mut Vec<(Range<usize>, Token)>) -> usize {
    let name_end = ident_end(text, at);
    let name = &text[at..name_end];
    // A member of something that is not a built-in namespace (`bar.close`): plain. Built-in
    // chains are consumed whole from their head, so they never get here.
    if text[..at].ends_with('.') {
        return name_end;
    }
    if KEYWORDS.contains(&name) {
        out.push((at..name_end, Token::Keyword));
        return name_end;
    }
    let next = next_non_space(text, name_end);
    if next == Some('(') && is_fn_decl(text, at, name_end) {
        out.push((at..name_end, Token::FnDecl));
        return name_end;
    }
    if name == "true" || name == "false" {
        out.push((at..name_end, Token::Constant));
        return name_end;
    }
    if name == "na" {
        let kind = if next == Some('(') {
            Token::BuiltinFn
        } else {
            Token::Constant
        };
        out.push((at..name_end, kind));
        return name_end;
    }
    // `color = ...` inside a call is an argument name, not the type.
    if is_named_argument(text, name_end) {
        return name_end;
    }
    let is_namespace = NAMESPACES.contains(&name);
    if is_namespace && text[name_end..].starts_with('.') {
        let end = chain_end(text, at);
        if end > name_end {
            let chain = &text[at..end];
            let after = next_non_space(text, end);
            let last = chain.rsplit('.').next().unwrap_or_default();
            let kind = if chain == "chart.point" {
                Token::Type
            } else if after == Some('(') || (last == "new" && after == Some('<')) {
                Token::BuiltinFn
            } else {
                Token::BuiltinVar
            };
            out.push((at..end, kind));
            return end;
        }
    }
    let callable =
        is_namespace || BUILTIN_FNS.contains(&name) || BUILTIN_VARS.contains(&name) || TYPES.contains(&name);
    let kind = if next == Some('(') && callable {
        Some(Token::BuiltinFn)
    } else if TYPES.contains(&name) {
        Some(Token::Type)
    } else if BUILTIN_VARS.contains(&name) {
        Some(Token::BuiltinVar)
    } else {
        None
    };
    if let Some(kind) = kind {
        out.push((at..name_end, kind));
    }
    name_end
}

/// Whether the identifier at `start..name_end` opens a function declaration: it is the first
/// thing on its line (after `export` / `method`) and its closing `)` is followed by `=>`.
fn is_fn_decl(text: &str, start: usize, name_end: usize) -> bool {
    let line_start = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let head = text[line_start..start].trim();
    if !matches!(head, "" | "export" | "method" | "export method") {
        return false;
    }
    let Some(open) = text[name_end..].strip_prefix('(') else {
        return false;
    };
    let mut depth = 1;
    for (offset, character) in open.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return open[offset + 1..]
                        .trim_start_matches(|c| c == ' ' || c == '\t')
                        .starts_with("=>");
                }
            }
            '\n' => return false,
            _ => {}
        }
    }
    false
}

/// Whether the identifier ending at `name_end` is followed by a single `=`.
fn is_named_argument(text: &str, name_end: usize) -> bool {
    let rest = text[name_end..].trim_start_matches(|c| c == ' ' || c == '\t');
    rest.starts_with('=') && !rest.starts_with("==") && !rest.starts_with("=>")
}

/// The end of `ident(.ident)*` opening at `at`.
fn chain_end(text: &str, at: usize) -> usize {
    let mut end = ident_end(text, at);
    while let Some(after_dot) = text[end..].strip_prefix('.') {
        let starts_name = after_dot
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_');
        if !starts_name {
            break;
        }
        end = ident_end(text, end + 1);
    }
    end
}

fn ident_end(text: &str, at: usize) -> usize {
    let mut end = at;
    for (offset, character) in text[at..].char_indices() {
        if character.is_alphanumeric() || character == '_' {
            end = at + offset + character.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// The next character after spaces and tabs; a newline counts as a character.
fn next_non_space(text: &str, from: usize) -> Option<char> {
    text[from..].chars().find(|c| *c != ' ' && *c != '\t')
}

/// The end of the string literal opening at `at`, or the end of its line if it never closes.
fn string_end(text: &str, at: usize, quote: char) -> usize {
    let mut escaped = false;
    for (offset, character) in text[at + quote.len_utf8()..].char_indices() {
        if character == '\n' {
            return at + quote.len_utf8() + offset;
        }
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            return at + quote.len_utf8() + offset + character.len_utf8();
        }
    }
    text.len()
}

/// The end of the number literal opening at `at`.
fn number_end(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = at;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        let digits = end + 1;
        let mut exponent = digits;
        if exponent < bytes.len() && matches!(bytes[exponent], b'+' | b'-') {
            exponent += 1;
        }
        let first = exponent;
        while exponent < bytes.len() && bytes[exponent].is_ascii_digit() {
            exponent += 1;
        }
        if exponent > first {
            end = exponent;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(source: &str) -> Vec<(Token, &str)> {
        tokenize(source)
            .into_iter()
            .map(|(range, kind)| (kind, &source[range]))
            .collect()
    }

    #[test]
    fn a_whole_script_line_by_line() {
        let source =
            "//@version=6\nindicator(\"T\", overlay=true)\nx = ta.sma(close, 20) + 1.5\n#ff8800";
        assert_eq!(
            lex(source),
            vec![
                (Token::Comment, "//@version=6"),
                (Token::BuiltinFn, "indicator"),
                (Token::Str, "\"T\""),
                (Token::Constant, "true"),
                (Token::BuiltinFn, "ta.sma"),
                (Token::BuiltinVar, "close"),
                (Token::Number, "20"),
                (Token::Number, "1.5"),
                (Token::Color, "#ff8800"),
            ]
        );
    }

    #[test]
    fn a_function_declaration_is_not_a_call() {
        assert_eq!(
            lex("f(a) =>\n    a + 1\nplot(f(close))"),
            vec![
                (Token::FnDecl, "f"),
                (Token::Keyword, "=>"),
                (Token::Number, "1"),
                (Token::BuiltinFn, "plot"),
                (Token::BuiltinVar, "close"),
            ]
        );
    }

    #[test]
    fn argument_names_and_fields_stay_plain() {
        assert_eq!(
            lex("plot(close, color = color.red, title = x.close)"),
            vec![
                (Token::BuiltinFn, "plot"),
                (Token::BuiltinVar, "close"),
                (Token::BuiltinVar, "color.red"),
            ]
        );
        // `==` is a comparison, not an argument name.
        assert_eq!(lex("a == close"), vec![(Token::BuiltinVar, "close")]);
    }

    #[test]
    fn types_generics_and_constants() {
        assert_eq!(
            lex("float x = na\narray.new<float>(5)\nvar line l = line.new(1, 2)"),
            vec![
                (Token::Type, "float"),
                (Token::Constant, "na"),
                (Token::BuiltinFn, "array.new"),
                (Token::Type, "float"),
                (Token::Number, "5"),
                (Token::Keyword, "var"),
                (Token::Type, "line"),
                (Token::BuiltinFn, "line.new"),
                (Token::Number, "1"),
                (Token::Number, "2"),
            ]
        );
    }

    #[test]
    fn keywords_and_namespace_variables() {
        assert_eq!(
            lex("if timeframe.isdaily and barstate.islast\n    x := 1"),
            vec![
                (Token::Keyword, "if"),
                (Token::BuiltinVar, "timeframe.isdaily"),
                (Token::Keyword, "and"),
                (Token::BuiltinVar, "barstate.islast"),
                (Token::Number, "1"),
            ]
        );
    }

    #[test]
    fn half_typed_code_does_not_break_the_lexer() {
        assert_eq!(
            lex("x = \"abc\ny = 'q"),
            vec![(Token::Str, "\"abc"), (Token::Str, "'q")]
        );
        // Only six or eight hex digits make a colour.
        assert_eq!(lex("#ff88 #ff8800aa #abcdef1"), vec![(Token::Color, "#ff8800aa")]);
    }

    #[test]
    fn strings_hide_comment_markers_and_unicode_is_safe() {
        assert_eq!(lex("\"a // b\""), vec![(Token::Str, "\"a // b\"")]);
        assert_eq!(
            lex("// привет\nx = \"é\""),
            vec![(Token::Comment, "// привет"), (Token::Str, "\"é\"")]
        );
    }

    #[test]
    fn spans_are_ordered_and_sliceable() {
        let source = "a = .5 + 1e + 2e3 #zz é\n\"x\\\"y\" // c\nta.\nclose.\n";
        let mut last_end = 0;
        for (range, _) in tokenize(source) {
            assert!(range.start >= last_end, "{range:?}");
            assert!(range.end > range.start, "{range:?}");
            // Slicing panics off a character boundary.
            let _ = &source[range.clone()];
            last_end = range.end;
        }
    }
}
