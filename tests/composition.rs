//! Superset composition: a miniature JSONC-style grammar built from this crate's public token
//! rules, proving that a superset crate can reuse them without forking.
//!
//! The pattern: define a wider token kind wrapping [`JsonToken`], lift each JSON rule with
//! `map_kind`, and add the rules that differ (here, a `//` line comment). This mirrors how a
//! real `strata-jsonc` would depend on `strata-json`'s rules layer only.

use std::io::Cursor;
use strata_json::{JsonToken, LiteralRule, NumberRule, PunctRule, StringRule, WhitespaceRule};
use strata_lexer::{
    LexError, Lexer, Lookahead, Match, Matcher, MatcherExt, MatcherVec, SpecScanner,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsoncToken {
    Json(JsonToken),
    Comment,
    Eof,
}

/// A `//` line comment rule — the one shape this superset adds over JSON.
#[derive(Debug, Clone, Copy, Default)]
struct LineCommentRule;

impl Matcher for LineCommentRule {
    type Kind = JsoncToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsoncToken>>, LexError> {
        if look.peek(2)? != b"//" {
            return Ok(None);
        }

        let mut len: usize = 2;

        loop {
            let bytes = look.peek(len.saturating_add(1))?;

            match bytes.get(len) {
                Some(b'\n' | b'\r') | None => {
                    return Ok(Match::from_len(JsoncToken::Comment, len));
                }
                Some(_) => len = len.saturating_add(1),
            }
        }
    }
}

fn jsonc_rules() -> MatcherVec<JsoncToken> {
    let wrap: fn(JsonToken) -> JsoncToken = JsoncToken::Json;

    vec![
        LineCommentRule.boxed(),
        WhitespaceRule.map_kind(wrap).boxed(),
        PunctRule.map_kind(wrap).boxed(),
        LiteralRule.map_kind(wrap).boxed(),
        StringRule.map_kind(wrap).boxed(),
        NumberRule.map_kind(wrap).boxed(),
        // BomRule deliberately omitted: this superset simply does not know the shape.
    ]
}

fn lex_kinds(input: &str) -> Result<Vec<JsoncToken>, LexError> {
    let scanner = SpecScanner::new(jsonc_rules(), JsoncToken::Eof);
    let mut lexer = Lexer::new(Cursor::new(input.as_bytes().to_vec()), scanner);
    let mut kinds = Vec::new();

    loop {
        let token = lexer.next_token()?;
        let done = token.kind == JsoncToken::Eof;
        kinds.push(token.kind);

        if done {
            return Ok(kinds);
        }
    }
}

#[test]
fn reused_json_rules_lex_under_the_wider_kind() -> Result<(), LexError> {
    let kinds = lex_kinds("{\"a\": 1, \"b\": true}")?;

    assert_eq!(
        kinds,
        vec![
            JsoncToken::Json(JsonToken::LeftBrace),
            JsoncToken::Json(JsonToken::String),
            JsoncToken::Json(JsonToken::Colon),
            JsoncToken::Json(JsonToken::Whitespace),
            JsoncToken::Json(JsonToken::Number),
            JsoncToken::Json(JsonToken::Comma),
            JsoncToken::Json(JsonToken::Whitespace),
            JsoncToken::Json(JsonToken::String),
            JsoncToken::Json(JsonToken::Colon),
            JsoncToken::Json(JsonToken::Whitespace),
            JsoncToken::Json(JsonToken::True),
            JsoncToken::Json(JsonToken::RightBrace),
            JsoncToken::Eof,
        ]
    );

    Ok(())
}

#[test]
fn added_comment_rule_extends_the_grammar() -> Result<(), LexError> {
    let kinds = lex_kinds("1 // trailing note")?;

    assert_eq!(
        kinds,
        vec![
            JsoncToken::Json(JsonToken::Number),
            JsoncToken::Json(JsonToken::Whitespace),
            JsoncToken::Comment,
            JsoncToken::Eof,
        ]
    );

    Ok(())
}

#[test]
fn comment_ends_at_the_line_break() -> Result<(), LexError> {
    let kinds = lex_kinds("// note\nnull")?;

    assert_eq!(
        kinds,
        vec![
            JsoncToken::Comment,
            JsoncToken::Json(JsonToken::Whitespace),
            JsoncToken::Json(JsonToken::Null),
            JsoncToken::Eof,
        ]
    );

    Ok(())
}

#[test]
fn json_shapes_stay_strict_inside_the_superset() {
    // Reusing the rules keeps their validation: `01` is still a lexical error.
    let Err(error) = lex_kinds("01") else {
        panic!("expected the reused number rule to keep rejecting leading zeros");
    };
    assert_eq!(error.position.byte(), 1);
}
