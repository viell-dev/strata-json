//! JSON token rules: composable matchers for the RFC 8259 lexical grammar.
//!
//! Each rule is a zero-sized [`Matcher`](strata_lexer::Matcher) recognizing one RFC 8259 token
//! shape from read-only lookahead. A rule *measures* how far it would match and never consumes
//! input; the [`SpecScanner`] driver owns all consumption. Rules are exported individually so
//! superset languages (JSONC, JSON5, ...) can reuse exactly the shapes they share with JSON,
//! lifting [`JsonToken`] into their own kind enum via
//! [`MatcherExt::map_kind`](strata_lexer::MatcherExt::map_kind), and add or replace only what
//! differs (e.g. a comment rule).
//!
//! String and number rules validate *lexical shape* here — bad escapes, unescaped control
//! characters, malformed `\uXXXX`, invalid UTF-8, and number-grammar violations are lexical
//! failures reported at the offending byte. Escape decoding and numeric conversion stay
//! downstream, at the resolver boundary.

use std::sync::Arc;
use strata_lexer::{
    Candidate, LexError, Lookahead, Match, Matcher, MatcherSet, Position, SpecScanner,
};

/// The UTF-8 encoding of U+FEFF, the byte-order mark.
const BOM_BYTES: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Token kinds produced by the RFC 8259 token rules.
///
/// Kinds are payload-free: a token records *what was found and where* (kind plus byte span), and
/// the source bytes stay addressable through the retained source. Superset grammars typically
/// wrap this enum in their own kind (e.g. `JsoncToken::Json(JsonToken)`) and lift the rules with
/// [`MatcherExt::map_kind`](strata_lexer::MatcherExt::map_kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonToken {
    /// `{` — begin-object structural character (RFC 8259 §2).
    LeftBrace,
    /// `}` — end-object structural character (RFC 8259 §2).
    RightBrace,
    /// `[` — begin-array structural character (RFC 8259 §2).
    LeftBracket,
    /// `]` — end-array structural character (RFC 8259 §2).
    RightBracket,
    /// `:` — name-separator structural character (RFC 8259 §2).
    Colon,
    /// `,` — value-separator structural character (RFC 8259 §2).
    Comma,
    /// The `true` literal (RFC 8259 §3).
    True,
    /// The `false` literal (RFC 8259 §3).
    False,
    /// The `null` literal (RFC 8259 §3).
    Null,
    /// A complete string token, span covering both quotes (RFC 8259 §7).
    String,
    /// A complete number token (RFC 8259 §6).
    Number,
    /// A run of insignificant whitespace: space, tab, CR, LF (RFC 8259 §2).
    Whitespace,
    /// A byte-order mark (U+FEFF) at the very start of the input (RFC 8259 §8.1).
    Bom,
    /// End of input.
    Eof,
}

impl JsonToken {
    /// Returns whether this kind is trivia — grammatically insignificant source content that the
    /// CST retains instead of discarding.
    pub const fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Bom)
    }

    /// A short human-readable description for diagnostics.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::LeftBrace => "'{'",
            Self::RightBrace => "'}'",
            Self::LeftBracket => "'['",
            Self::RightBracket => "']'",
            Self::Colon => "':'",
            Self::Comma => "','",
            Self::True => "'true'",
            Self::False => "'false'",
            Self::Null => "'null'",
            Self::String => "a string",
            Self::Number => "a number",
            Self::Whitespace => "whitespace",
            Self::Bom => "a byte-order mark",
            Self::Eof => "end of input",
        }
    }
}

/// The strict RFC 8259 rule set as a fully monomorphic matcher tuple.
///
/// [`BomRule`] is part of the set on purpose: the token rules describe which shapes *exist* in
/// the input, while the parser owns the policy for them. Recognizing a leading BOM as its own
/// token lets the strict parser reject it with a precise diagnostic (and the `allow_bom` option
/// tolerate it per RFC 8259 §8.1) instead of surfacing a generic unrecognized-byte error.
pub type StrictRuleSet = (
    WhitespaceRule,
    PunctRule,
    LiteralRule,
    StringRule,
    NumberRule,
    BomRule,
);

/// Returns the strict RFC 8259 rule set.
pub const fn strict_rule_set() -> StrictRuleSet {
    (
        WhitespaceRule,
        PunctRule,
        LiteralRule,
        StringRule,
        NumberRule,
        BomRule,
    )
}

/// Returns a [`SpecScanner`] over the strict rule set with [`JsonToken::Eof`] as its
/// end-of-input token.
pub const fn strict_scanner() -> SpecScanner<StrictRuleSet, JsonToken> {
    SpecScanner::new(strict_rule_set(), JsonToken::Eof)
}

/// A cheaply cloneable, shared rule set.
///
/// [`MatcherVec`](strata_lexer::MatcherVec) and other non-`Clone` rule sets cannot be handed to a
/// reusable parser directly, because the parser clones its rules into a fresh scanner per parse.
/// `SharedRuleSet` wraps any [`MatcherSet`] in an [`Arc`] so cloning shares one set. It is
/// generic over the kind, so superset crates can use it for their own rule sets too.
#[derive(Debug)]
pub struct SharedRuleSet<M> {
    rules: Arc<M>,
}

impl<M> SharedRuleSet<M> {
    /// Wraps `rules` for shared, cloneable use.
    pub fn new(rules: M) -> Self {
        Self {
            rules: Arc::new(rules),
        }
    }
}

impl<M> Clone for SharedRuleSet<M> {
    fn clone(&self) -> Self {
        Self {
            rules: Arc::clone(&self.rules),
        }
    }
}

impl<M: MatcherSet> MatcherSet for SharedRuleSet<M> {
    type Kind = M::Kind;

    fn measure(&self, look: &mut Lookahead<'_>) -> Result<Vec<Candidate<Self::Kind>>, LexError> {
        self.rules.measure(look)
    }
}

/// Measures the run of bytes satisfying `pred` starting at `from`, filling on demand.
fn run_len(
    look: &mut Lookahead<'_>,
    from: usize,
    mut pred: impl FnMut(u8) -> bool,
) -> Result<usize, LexError> {
    let mut len = from;

    loop {
        let bytes = look.peek(len.saturating_add(1))?;

        match bytes.get(len) {
            Some(&byte) if pred(byte) => len = len.saturating_add(1),
            _ => return Ok(len),
        }
    }
}

/// Returns `start` advanced by `offset` bytes.
///
/// Line and column stay valid because every rule reporting through this helper has already
/// verified that the bytes before `offset` contain no line breaks (numbers are ASCII digits and
/// signs; string errors are reported before any unescaped control byte could pass).
fn offset_position(start: Position, offset: usize) -> Position {
    let offset = u64::try_from(offset).unwrap_or(u64::MAX);

    Position {
        byte: start.byte.saturating_add(offset),
        line: start.line,
        column: start.column.saturating_add(offset),
    }
}

/// Matches a run of insignificant whitespace (RFC 8259 §2): space, tab, CR, LF — exactly those
/// four bytes, nothing else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WhitespaceRule;

impl Matcher for WhitespaceRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        let len = run_len(look, 0, |byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))?;

        Ok((len > 0).then(|| Match::new(JsonToken::Whitespace, len)))
    }
}

/// Matches the six structural characters (RFC 8259 §2): `{` `}` `[` `]` `:` `,`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PunctRule;

impl Matcher for PunctRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        let kind = match look.peek_byte()? {
            Some(b'{') => JsonToken::LeftBrace,
            Some(b'}') => JsonToken::RightBrace,
            Some(b'[') => JsonToken::LeftBracket,
            Some(b']') => JsonToken::RightBracket,
            Some(b':') => JsonToken::Colon,
            Some(b',') => JsonToken::Comma,
            _ => return Ok(None),
        };

        Ok(Some(Match::new(kind, 1)))
    }
}

/// Matches the three literal names (RFC 8259 §3): `true`, `false`, `null` — exactly lowercase,
/// no other spelling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiteralRule;

impl Matcher for LiteralRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        let (word, kind): (&[u8], JsonToken) = match look.peek_byte()? {
            Some(b't') => (b"true", JsonToken::True),
            Some(b'f') => (b"false", JsonToken::False),
            Some(b'n') => (b"null", JsonToken::Null),
            _ => return Ok(None),
        };

        if look.peek(word.len())? == word {
            Ok(Some(Match::new(kind, word.len())))
        } else {
            Ok(None)
        }
    }
}

/// Matches a JSON string token (RFC 8259 §7), validating lexical shape.
///
/// The measured span covers both quotes. Shape violations are lexical errors
/// ([`LexErrorKind::Unrecognized`](strata_lexer::LexErrorKind::Unrecognized)) reported at the
/// offending byte:
///
/// - an unescaped control character (`U+0000`–`U+001F`) — must be escaped per §7,
/// - an escape other than `\" \\ \/ \b \f \n \r \t \uXXXX` (reported at the backslash),
/// - `\u` not followed by exactly four hex digits (reported at the backslash),
/// - invalid UTF-8 (the input is required to be UTF-8 per §8.1).
///
/// Escape *decoding* — including UTF-16 surrogate-pair combination — is resolver work; lone
/// surrogate escapes are lexically valid (§8.2 says they are "not recommended", not forbidden).
/// An unclosed string is
/// [`LexErrorKind::UnterminatedDelimiter`](strata_lexer::LexErrorKind::UnterminatedDelimiter) at
/// the opening quote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StringRule;

impl Matcher for StringRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        if look.peek_byte()? != Some(b'"') {
            return Ok(None);
        }

        let start = look.position();
        let mut index: usize = 1; // past the opening quote

        loop {
            let bytes = look.peek(index.saturating_add(1))?;

            match bytes.get(index) {
                None => return Err(LexError::unterminated_delimiter(start)),
                Some(b'"') => {
                    let len = index.saturating_add(1);
                    return finish_string(look, start, len);
                }
                Some(b'\\') => index = validate_escape(look, start, index)?,
                Some(&byte) if byte < 0x20 => {
                    return Err(LexError::unrecognized(offset_position(start, index)));
                }
                Some(_) => index = index.saturating_add(1),
            }
        }
    }
}

/// Validates the escape sequence whose backslash sits at `index`, returning the index just past
/// it.
fn validate_escape(
    look: &mut Lookahead<'_>,
    start: Position,
    index: usize,
) -> Result<usize, LexError> {
    let bytes = look.peek(index.saturating_add(2))?;

    match bytes.get(index.saturating_add(1)) {
        None => Err(LexError::unterminated_delimiter(start)),
        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => Ok(index.saturating_add(2)),
        Some(b'u') => {
            let hex_start = index.saturating_add(2);
            let hex_end = index.saturating_add(6);
            let bytes = look.peek(hex_end)?;
            let valid = bytes
                .get(hex_start..hex_end)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));

            if valid {
                Ok(hex_end)
            } else {
                Err(LexError::unrecognized(offset_position(start, index)))
            }
        }
        Some(_) => Err(LexError::unrecognized(offset_position(start, index))),
    }
}

/// Validates that the measured string token is valid UTF-8 and produces the match.
fn finish_string(
    look: &mut Lookahead<'_>,
    start: Position,
    len: usize,
) -> Result<Option<Match<JsonToken>>, LexError> {
    let raw = look.peek(len)?;

    match core::str::from_utf8(raw) {
        Ok(_) => Ok(Some(Match::new(JsonToken::String, len))),
        Err(error) => Err(LexError::unrecognized(offset_position(
            start,
            error.valid_up_to(),
        ))),
    }
}

/// Matches a JSON number token per the RFC 8259 §6 grammar:
///
/// ```text
/// number = [ "-" ] int [ frac ] [ exp ]
/// int    = "0" / ( digit1-9 *DIGIT )
/// frac   = "." 1*DIGIT
/// exp    = ( "e" / "E" ) [ "+" / "-" ] 1*DIGIT
/// ```
///
/// Leading zeros (`01`), an empty fraction (`1.`), and an empty exponent (`1e`, `1e+`) would
/// produce ambiguous token boundaries, so they are rejected here as lexical errors
/// ([`LexErrorKind::Unrecognized`](strata_lexer::LexErrorKind::Unrecognized)) at the offending
/// byte. A bare `-`, a leading `+`, or a bare `.` simply do not match — no rule covers them, so
/// the scanner reports the byte as unrecognized.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NumberRule;

impl Matcher for NumberRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        if !matches!(look.peek_byte()?, Some(b'-' | b'0'..=b'9')) {
            return Ok(None);
        }

        let start = look.position();
        let mut index: usize = 0;

        // Optional leading minus.
        if look.peek(1)?.first() == Some(&b'-') {
            index = 1;
        }

        // Integer part.
        match look.peek(index.saturating_add(1))?.get(index) {
            Some(b'0') => {
                index = index.saturating_add(1);

                // A digit after a leading zero is a grammar error (e.g. `01`).
                if matches!(
                    look.peek(index.saturating_add(1))?.get(index),
                    Some(b'0'..=b'9')
                ) {
                    return Err(LexError::unrecognized(offset_position(start, index)));
                }
            }
            Some(b'1'..=b'9') => {
                index = run_len(look, index, |byte| byte.is_ascii_digit())?;
            }
            _ => return Ok(None), // bare minus: no digit follows
        }

        // Optional fraction.
        if look.peek(index.saturating_add(1))?.get(index) == Some(&b'.') {
            let frac_start = index.saturating_add(1);
            let frac_end = run_len(look, frac_start, |byte| byte.is_ascii_digit())?;

            if frac_end == frac_start {
                return Err(LexError::unrecognized(offset_position(start, index)));
            }

            index = frac_end;
        }

        // Optional exponent.
        if matches!(
            look.peek(index.saturating_add(1))?.get(index),
            Some(b'e' | b'E')
        ) {
            index = index.saturating_add(1);

            if matches!(
                look.peek(index.saturating_add(1))?.get(index),
                Some(b'+' | b'-')
            ) {
                index = index.saturating_add(1);
            }

            let exp_end = run_len(look, index, |byte| byte.is_ascii_digit())?;

            if exp_end == index {
                return Err(LexError::unrecognized(offset_position(start, index)));
            }

            index = exp_end;
        }

        Ok(Some(Match::new(JsonToken::Number, index)))
    }
}

/// Matches the UTF-8 encoding of U+FEFF at byte offset 0 only (RFC 8259 §8.1).
///
/// The rule recognizes the shape; the parser owns the policy: strict parsing rejects the token
/// with a precise diagnostic, and the `allow_bom` option retains it as trivia, per the §8.1
/// permission to ignore a leading byte-order mark. A BOM anywhere else in the input is not
/// matched by any rule and surfaces as an unrecognized byte.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BomRule;

impl Matcher for BomRule {
    type Kind = JsonToken;

    fn try_match(&self, look: &mut Lookahead<'_>) -> Result<Option<Match<JsonToken>>, LexError> {
        if look.position().byte != 0 {
            return Ok(None);
        }

        Ok((look.peek(3)? == BOM_BYTES.as_slice()).then(|| Match::new(JsonToken::Bom, 3)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use strata_lexer::{LexErrorKind, Lexer};

    fn lex_bytes(input: &[u8]) -> Result<Vec<(JsonToken, u64, u64)>, LexError> {
        let mut lexer = Lexer::new(Cursor::new(input.to_vec()), strict_scanner());
        let mut out = Vec::new();

        loop {
            let token = lexer.next_token()?;
            let done = token.kind == JsonToken::Eof;
            out.push((token.kind, token.span.start, token.span.end));

            if done {
                break;
            }
        }

        Ok(out)
    }

    fn lex(input: &str) -> Result<Vec<(JsonToken, u64, u64)>, LexError> {
        lex_bytes(input.as_bytes())
    }

    fn lex_error(input: &str) -> LexError {
        match lex(input) {
            Err(error) => error,
            Ok(tokens) => panic!("expected a lexical error, got {tokens:?}"),
        }
    }

    #[test]
    fn structural_characters_lex_individually() -> Result<(), LexError> {
        assert_eq!(
            lex("{}[]:,")?,
            vec![
                (JsonToken::LeftBrace, 0, 1),
                (JsonToken::RightBrace, 1, 2),
                (JsonToken::LeftBracket, 2, 3),
                (JsonToken::RightBracket, 3, 4),
                (JsonToken::Colon, 4, 5),
                (JsonToken::Comma, 5, 6),
                (JsonToken::Eof, 6, 6),
            ]
        );

        Ok(())
    }

    #[test]
    fn literals_lex_with_exact_spelling() -> Result<(), LexError> {
        assert_eq!(
            lex("true false null")?,
            vec![
                (JsonToken::True, 0, 4),
                (JsonToken::Whitespace, 4, 5),
                (JsonToken::False, 5, 10),
                (JsonToken::Whitespace, 10, 11),
                (JsonToken::Null, 11, 15),
                (JsonToken::Eof, 15, 15),
            ]
        );

        Ok(())
    }

    #[test]
    fn uppercase_literals_are_unrecognized() {
        let error = lex_error("TRUE");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 0);
    }

    #[test]
    fn truncated_literal_is_unrecognized() {
        let error = lex_error("tru");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
    }

    #[test]
    fn whitespace_run_is_one_token() -> Result<(), LexError> {
        assert_eq!(
            lex(" \t\r\n ")?,
            vec![(JsonToken::Whitespace, 0, 5), (JsonToken::Eof, 5, 5)]
        );

        Ok(())
    }

    #[test]
    fn non_json_whitespace_is_unrecognized() {
        // Vertical tab (0x0B) is not RFC 8259 whitespace.
        let error = lex_error("\u{0B}null");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 0);
    }

    #[test]
    fn valid_numbers_measure_full_spans() -> Result<(), LexError> {
        for input in [
            "0", "-0", "42", "-17", "3.14", "-0.5", "1e10", "2.5E-3", "1e+5", "5e-4",
        ] {
            let tokens = lex(input)?;
            let expected_end = u64::try_from(input.len()).unwrap_or(u64::MAX);
            assert_eq!(
                tokens,
                vec![
                    (JsonToken::Number, 0, expected_end),
                    (JsonToken::Eof, expected_end, expected_end),
                ],
                "input {input:?}"
            );
        }

        Ok(())
    }

    #[test]
    fn leading_zero_is_rejected_at_second_digit() {
        let error = lex_error("01");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 1);

        let error = lex_error("-01");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 2);
    }

    #[test]
    fn empty_fraction_and_exponent_are_rejected() {
        for input in ["1.", "1e", "1E", "1e+", "1e-"] {
            let error = lex_error(input);
            assert!(
                matches!(error.kind, LexErrorKind::Unrecognized),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn bare_minus_and_leading_plus_are_unrecognized() {
        for input in ["-", "+1"] {
            let error = lex_error(input);
            assert!(
                matches!(error.kind, LexErrorKind::Unrecognized),
                "input {input:?}"
            );
            assert_eq!(error.position.byte, 0, "input {input:?}");
        }
    }

    #[test]
    fn strings_measure_including_quotes() -> Result<(), LexError> {
        assert_eq!(
            lex(r#""hello""#)?,
            vec![(JsonToken::String, 0, 7), (JsonToken::Eof, 7, 7)]
        );
        assert_eq!(
            lex(r#""""#)?,
            vec![(JsonToken::String, 0, 2), (JsonToken::Eof, 2, 2)]
        );

        Ok(())
    }

    #[test]
    fn string_escapes_are_accepted() -> Result<(), LexError> {
        let input = r#""\"\\\/\b\f\n\r\t€😀""#;
        let tokens = lex(input)?;
        let expected_end = u64::try_from(input.len()).unwrap_or(u64::MAX);
        assert_eq!(
            tokens,
            vec![
                (JsonToken::String, 0, expected_end),
                (JsonToken::Eof, expected_end, expected_end),
            ]
        );

        Ok(())
    }

    #[test]
    fn string_with_multibyte_utf8_is_accepted() -> Result<(), LexError> {
        let input = "\"caf\u{E9} \u{1F600}\"";
        let tokens = lex(input)?;
        assert_eq!(tokens.first().map(|token| token.0), Some(JsonToken::String));

        Ok(())
    }

    #[test]
    fn invalid_escape_is_rejected_at_backslash() {
        // Bytes: 0 `"`, 1 `\`, 2 `q`.
        let error = lex_error(r#""\q""#);
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 1);
    }

    #[test]
    fn escaped_quote_does_not_close_the_string() {
        // `"\"` — the escaped quote is content, so the string never closes.
        let error = lex_error(r#""\""#);
        assert!(matches!(error.kind, LexErrorKind::UnterminatedDelimiter));
    }

    #[test]
    fn short_unicode_escape_is_rejected() {
        let error = lex_error(r#""\u123""#);
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 1);
    }

    #[test]
    fn non_hex_unicode_escape_is_rejected() {
        let error = lex_error(r#""\u12G4""#);
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
    }

    #[test]
    fn unescaped_control_character_is_rejected_at_its_byte() {
        // Bytes: 0 `"`, 1 tab.
        let error = lex_error("\"\t\"");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 1);

        let error = lex_error("\"a\nb\"");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 2);
    }

    #[test]
    fn unterminated_string_is_rejected_at_opening_quote() {
        let error = lex_error(r#""open"#);
        assert!(matches!(error.kind, LexErrorKind::UnterminatedDelimiter));
        assert_eq!(error.position.byte, 0);
    }

    #[test]
    fn invalid_utf8_in_string_is_rejected() {
        // 0xFF can never appear in UTF-8.
        let result = lex_bytes(b"\"a\xFFb\"");
        let Err(error) = result else {
            panic!("expected a lexical error");
        };
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 2);
    }

    #[test]
    fn bom_matches_only_at_offset_zero() -> Result<(), LexError> {
        assert_eq!(
            lex("\u{FEFF}1")?,
            vec![
                (JsonToken::Bom, 0, 3),
                (JsonToken::Number, 3, 4),
                (JsonToken::Eof, 4, 4),
            ]
        );

        // The same bytes later in the input are not a BOM token.
        let error = lex_error("1 \u{FEFF}");
        assert!(matches!(error.kind, LexErrorKind::Unrecognized));
        assert_eq!(error.position.byte, 2);

        Ok(())
    }

    #[test]
    fn shared_rule_set_measures_like_the_original() -> Result<(), LexError> {
        let shared = SharedRuleSet::new(strict_rule_set());
        let scanner = SpecScanner::new(shared.clone(), JsonToken::Eof);
        let mut lexer = Lexer::new(Cursor::new("[1]".as_bytes().to_vec()), scanner);

        assert_eq!(lexer.next_token()?.kind, JsonToken::LeftBracket);
        assert_eq!(lexer.next_token()?.kind, JsonToken::Number);
        assert_eq!(lexer.next_token()?.kind, JsonToken::RightBracket);
        assert_eq!(lexer.next_token()?.kind, JsonToken::Eof);

        Ok(())
    }
}
