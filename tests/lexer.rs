//! The error-tolerant lexer (`lexer::highlight`) used by editors, and the
//! keyword and builtin tables. The tolerant lexer must agree exactly with the
//! compiler's lexer on everything the compiler accepts, and report the
//! compiler's own error first on everything it rejects.

// A panic in a test is a test failure, so these lints do not apply here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use vela::ast::{BUILTIN_TYPES, Span, Type};
use vela::error::Error;
use vela::lexer::{self, KEYWORDS, LexKind, LexToken, Lexer, Token, highlight};

fn vela_files(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "vela"))
        .collect();
    files.sort();
    files
}

/// Checks `highlight` against `Lexer::tokenize` on `source`, and the shape of
/// its output: tokens in order, without overlaps, each range matching its
/// line/column positions, ending with a single `Eof`.
fn check_agreement(source: &str, what: &str) {
    let tokens = highlight(source);
    let Some(last) = tokens.last() else { panic!("{}: no tokens", what) };
    assert_eq!(last.kind, LexKind::Token(Token::Eof), "{}: must end with Eof", what);

    let mut previous_end = 0;
    for token in &tokens {
        assert!(token.range.start >= previous_end, "{}: overlapping {:?}", what, token);
        assert!(token.range.end >= token.range.start && token.range.end <= source.len(), "{}: {:?}", what, token);
        assert_eq!(position(source, token.range.start), token.start, "{}: start of {:?}", what, token);
        assert_eq!(position(source, token.range.end), token.end, "{}: end of {:?}", what, token);
        previous_end = token.range.end;
    }
    assert_eq!(tokens.iter().filter(|t| t.kind == LexKind::Token(Token::Eof)).count(), 1, "{}", what);

    let first_error = tokens.iter().find_map(|t| match &t.kind {
        LexKind::Invalid(error) => Some(error.clone()),
        _ => None,
    });
    match (Lexer::new(source).tokenize(), first_error) {
        (Ok((strict_tokens, strict_spans)), None) => {
            let tolerant: Vec<(Token, Span)> = tokens
                .iter()
                .filter_map(|t| match &t.kind {
                    LexKind::Token(token) => Some((token.clone(), t.start)),
                    _ => None,
                })
                .collect();
            let strict: Vec<(Token, Span)> = strict_tokens.into_iter().zip(strict_spans).collect();
            assert_eq!(tolerant, strict, "{}: tokens differ", what);
        }
        (Err(strict), Some(tolerant)) => assert_eq!(tolerant, strict, "{}: first error differs", what),
        (strict, tolerant) => panic!("{}: tokenize gave {:?} but highlight's first error is {:?}", what, strict.err(), tolerant),
    }
}

/// The line and column of byte offset `offset`, counted like the lexer does.
fn position(source: &str, offset: usize) -> Span {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    // The `\r` of a CRLF line end takes no column.
    let last = before.rsplit('\n').next().unwrap();
    let mut col = last.chars().count() + 1 - usize::from(last.ends_with('\r') && source[offset..].starts_with('\n'));
    // A byte order mark at the start takes no column either.
    if line == 1 && source.starts_with('\u{feff}') && offset > 0 {
        col -= 1;
    }
    Span::new(line, col)
}

fn kinds(source: &str) -> Vec<LexKind> {
    highlight(source).into_iter().map(|t| t.kind).collect()
}

fn token(t: Token) -> LexKind {
    LexKind::Token(t)
}

fn invalid(line: usize, col: usize, message: &str) -> LexKind {
    LexKind::Invalid(Error::at(Span::new(line, col), message))
}

#[test]
fn agrees_with_the_compiler_on_every_test_program() {
    for dir in ["examples", "tests/run", "tests/fail"] {
        for file in vela_files(dir) {
            let source = fs::read_to_string(&file).unwrap();
            check_agreement(&source, &file.display().to_string());
        }
    }
}

#[test]
fn agrees_with_the_compiler_on_every_prefix_of_the_examples() {
    // Every half-typed state of a real program: highlighting must never fail,
    // and must still agree with the compiler.
    for file in vela_files("examples") {
        let source = fs::read_to_string(&file).unwrap();
        for (offset, _) in source.char_indices() {
            check_agreement(&source[..offset], &format!("{} cut at byte {}", file.display(), offset));
        }
    }
}

#[test]
fn agrees_with_the_compiler_on_malformed_input() {
    let cases = [
        "", "\"", "\"abc", "\"a\\", "\"a\\q\" x", "\"a\\q\nb", "\"line one\nline two\"", "_x", "a__b", "café",
        "élan", "12abc", "1.5e3", "99999999999999999999", "x @ y", "a ! b", "\0", "\"\0\"", "# only a comment",
        "task main()\n    show(\"unterminated)\nfi\n", "\"bad \\q then\nunterminated",
        "x\r\ny", "# comment\r\nfi", "\"a\\\r\nb\"", "\u{feff}task", "1.", "1..5", "a && b", "!x",
        "1__000", "1_", "1_000abc", "_", "a _ b", "{_}", "_é", "__", "case _\n", "^Node@r", "new@r int(1) in r", "@", "region",
    ];
    for source in cases {
        check_agreement(source, &format!("{:?}", source));
    }
}

#[test]
fn invalid_text_becomes_a_token_and_lexing_continues() {
    assert_eq!(
        kinds("x $ y"),
        [
            token(Token::Ident("x".into())),
            invalid(1, 3, "unexpected character '$'"),
            token(Token::Ident("y".into())),
            token(Token::Eof),
        ]
    );
    assert_eq!(
        kinds("_tmp = 12abc"),
        [
            invalid(1, 1, "names must start with a letter"),
            token(Token::Assign),
            invalid(1, 8, "invalid number '12abc'"),
            token(Token::Eof),
        ]
    );
    // A bad escape is reported where it is, and the literal still ends at its quote.
    assert_eq!(kinds("\"a\\qb\" fi"), [invalid(1, 3, "invalid escape '\\q'"), token(Token::Fi), token(Token::Eof)]);
}

#[test]
fn an_unterminated_string_ends_with_its_line() {
    let tokens = highlight("show(\"oops)\nfi\n");
    let string = &tokens[2];
    assert_eq!(string.kind, invalid(1, 6, "unterminated string"));
    assert_eq!((string.range.clone(), string.end), (5..11, Span::new(1, 12)));
    let rest: Vec<LexKind> = tokens[3..].iter().map(|t| t.kind.clone()).collect();
    assert_eq!(rest, [token(Token::Newline), token(Token::Fi), token(Token::Newline), token(Token::Eof)]);
}

#[test]
fn comments_are_kept_with_their_extent() {
    let tokens = highlight("fi # done\n");
    let comment: &LexToken = &tokens[1];
    assert_eq!(comment.kind, LexKind::Comment);
    assert_eq!((comment.range.clone(), comment.start, comment.end), (3..9, Span::new(1, 4), Span::new(1, 10)));
}

#[test]
fn ranges_are_bytes_and_positions_are_characters() {
    let tokens = highlight("\"é\" x");
    assert_eq!((tokens[0].range.clone(), tokens[0].end), (0..4, Span::new(1, 4)));
    assert_eq!((tokens[1].range.clone(), tokens[1].start), (5..6, Span::new(1, 5)));
}

#[test]
fn every_keyword_lexes_to_its_token() {
    for (word, keyword) in KEYWORDS {
        assert_eq!(kinds(word), [token(keyword.clone()), token(Token::Eof)], "{}", word);
        assert_eq!(keyword.describe(), format!("'{}'", word));
    }
    // Longer words that start with a keyword are names.
    assert_eq!(kinds("fine"), [token(Token::Ident("fine".into())), token(Token::Eof)]);
}

#[test]
fn the_send_arrow_is_one_token() {
    let lt = || token(Token::Lt);
    let minus = || token(Token::Minus);
    assert_eq!(kinds("<-"), [token(Token::Send), token(Token::Eof)]);
    assert_eq!(Token::Send.describe(), "'<-'");
    // Written without a space, `<-` is always the arrow, never `<` then `-`.
    assert_eq!(kinds("a<-1")[1], token(Token::Send));
    assert_eq!(kinds("a < -1")[1..3], [lt(), minus()]);
    assert_eq!(kinds("<=")[0], token(Token::Le));
    // `send` is an ordinary name.
    assert_eq!(kinds("send"), [token(Token::Ident("send".into())), token(Token::Eof)]);
}

#[test]
fn the_underscore_alone_is_a_symbol() {
    assert_eq!(kinds("_"), [token(Token::Underscore), token(Token::Eof)]);
    assert_eq!(Token::Underscore.describe(), "'_'");
    assert_eq!(kinds("{_}"), [token(Token::LBrace), token(Token::Underscore), token(Token::RBrace), token(Token::Eof)]);
    assert_eq!(kinds("fail _")[1], token(Token::Underscore));
    // A name still can't start with `_`.
    assert_eq!(kinds("_x")[0], invalid(1, 1, "names must start with a letter"));
    assert_eq!(kinds("__")[0], invalid(1, 1, "names must start with a letter"));
    // `maybe`, `is`, and `fail` are keywords; longer words are names.
    assert_eq!(kinds("maybe int")[0], token(Token::Maybe));
    assert_eq!(kinds("issue")[0], token(Token::Ident("issue".into())));
}

#[test]
fn regions_are_named_with_a_keyword_and_an_at_sign() {
    let ident = |name: &str| token(Token::Ident(name.into()));
    assert_eq!(kinds("region r"), [token(Token::Region), ident("r"), token(Token::Eof)]);
    assert_eq!(kinds("^Node@r")[1..4], [ident("Node"), token(Token::At), ident("r")]);
    assert_eq!(kinds("new@r")[..3], [token(Token::New), token(Token::At), ident("r")]);
    assert_eq!(Token::At.describe(), "'@'");
    // `free` is an ordinary name now that regions free memory.
    assert_eq!(kinds("free"), [ident("free"), token(Token::Eof)]);
    assert_eq!(kinds("regions")[0], ident("regions"));
}

#[test]
fn every_escape_is_accepted() {
    for (escape, value) in lexer::ESCAPES {
        let source = format!("\"\\{}\"", escape);
        assert_eq!(kinds(&source), [token(Token::Text(value.to_string())), token(Token::Eof)], "{}", source);
    }
}

#[test]
fn builtin_types_are_named_as_written() {
    for (name, ty) in BUILTIN_TYPES {
        assert_eq!(ty.to_string(), *name);
        assert_eq!(Type::builtin(name).as_ref(), Some(ty));
    }
    assert_eq!(Type::builtin("Point"), None);
}

#[test]
fn underscores_between_digits_are_ignored() {
    assert_eq!(kinds("1_000"), [token(Token::Int(1000)), token(Token::Eof)]);
    assert_eq!(kinds("1_000_000"), [token(Token::Int(1_000_000)), token(Token::Eof)]);
    assert_eq!(kinds("1_2_3"), [token(Token::Int(123)), token(Token::Eof)]);
    assert_eq!(kinds("1_000.000_001"), [token(Token::Float(1000.000001)), token(Token::Eof)]);
    assert_eq!(kinds("1__000")[0], invalid(1, 1, "invalid number '1__000'"));
    assert_eq!(kinds("1_")[0], invalid(1, 1, "invalid number '1_'"));
}

#[test]
fn crlf_line_ends_read_like_lf() {
    // Positions don't count the `\r`, so errors point where they would in an LF file.
    let tokens = highlight("bind x =\r\nfi # done\r\n");
    let newline = &tokens[3];
    assert_eq!((newline.kind.clone(), newline.start), (token(Token::Newline), Span::new(1, 9)));
    assert_eq!(tokens[4].start, Span::new(2, 1));
    // A comment stops before the `\r`.
    assert_eq!(tokens[5].kind, LexKind::Comment);
    assert_eq!(tokens[5].range, 13..19);
    // A backslash at the end of a CRLF line is reported like one at the end of an LF line.
    assert_eq!(kinds("\"a\\\r\nb\"")[0], invalid(1, 3, "invalid escape: '\\' at the end of a line"));
    // A line break inside text is `\n` either way.
    assert_eq!(kinds("\"a\r\nb\"")[0], token(Token::Text("a\nb".into())));
}

#[test]
fn a_byte_order_mark_is_skipped() {
    let tokens = highlight("\u{feff}fi");
    assert_eq!(tokens[0].kind, token(Token::Fi));
    // Columns start after it; byte ranges still count it.
    assert_eq!((tokens[0].start, tokens[0].range.clone()), (Span::new(1, 1), 3..5));
}

#[test]
fn common_mistakes_get_hints() {
    assert_eq!(kinds("1.")[0], invalid(1, 1, "invalid number '1.': write '1.0'"));
    assert_eq!(kinds("1..5")[1], token(Token::Range));
    assert_eq!(kinds("a && b")[1], invalid(1, 3, "unexpected '&&'; Vela writes 'and'"));
    assert_eq!(kinds("a || b")[1], invalid(1, 3, "unexpected '||'; Vela writes 'or'"));
    assert_eq!(kinds("!x")[0], invalid(1, 1, "unexpected '!'; Vela negates with 'not'"));
    // Floats are described as Vela writes them.
    assert_eq!(Token::Float(1e20).describe(), "float 100000000000000000000.0");
    assert_eq!(Token::Float(2.5).describe(), "float 2.5");
}
