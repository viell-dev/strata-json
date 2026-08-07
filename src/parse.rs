//! The strict RFC 8259 parser: token stream in, CST out.
//!
//! [`JsonParser::new`] is the fully strict path. [`JsonParser::builder`] exposes the two places
//! where RFC 8259 explicitly grants parsers latitude — tolerating a leading byte-order mark
//! (§8.1) and the nesting-depth implementation limit (§9) — plus `with_*` hooks to customize or
//! replace the lower layers: the token rules and the lexer configuration. The parser hands the
//! lexer its rule set at construction, which is what lets a superset language swap in an
//! extended rule set while reusing the machinery.

use crate::cst::{
    ArrayNode, Document, Element, Member, ObjectNode, SyntaxToken, Trivia, TriviaKind, Value,
};
use crate::rules::{JsonToken, StrictRuleSet, strict_rule_set};
use std::error::Error;
use std::fmt;
use std::io::{Cursor, Read};
use strata_lexer::{
    LexError, Lexer, LexerConfig, MatcherSet, Position, SourceStore, SpecScanner, Token,
};

/// The default nesting-depth limit.
///
/// RFC 8259 §9 permits parsers to set limits on the depth of nesting; this is the crate's
/// default for the maximum number of nested arrays/objects. Override it with
/// [`JsonParserBuilder::max_depth`]. The limit also bounds the parser's recursion, so raising it
/// substantially trades stack headroom for depth.
pub const DEFAULT_MAX_DEPTH: usize = 128;

/// A parse failure, carrying the position at which it was observed.
#[derive(Debug)]
pub struct JsonError {
    /// Position of the offending byte or token start.
    pub position: Position,
    /// What went wrong.
    pub kind: JsonErrorKind,
}

/// Categories of parse failures.
#[derive(Debug)]
pub enum JsonErrorKind {
    /// The lexer failed: I/O, an unrecognized byte, or a token-shape violation (invalid string
    /// escapes, unescaped control characters, number-grammar violations, invalid UTF-8) reported
    /// at the offending byte.
    Lex(LexError),
    /// A token of an unexpected kind where the grammar required something else.
    Expected {
        /// What the grammar required here.
        expected: &'static str,
        /// The token kind actually found.
        found: JsonToken,
    },
    /// The input ended where the grammar required more.
    UnexpectedEof {
        /// What the grammar required here.
        expected: &'static str,
    },
    /// A significant token followed the complete top-level value.
    TrailingContent {
        /// The token kind found after the value.
        found: JsonToken,
    },
    /// The input starts with a byte-order mark and the parser was not configured to tolerate it.
    ///
    /// RFC 8259 §8.1 forbids adding a BOM but permits implementations to ignore one; enable
    /// [`JsonParserBuilder::allow_bom`] to opt into that permission.
    UnexpectedBom,
    /// Nesting exceeded the configured depth limit (RFC 8259 §9 implementation limit).
    DepthLimitExceeded {
        /// The configured limit.
        limit: usize,
    },
}

impl fmt::Display for JsonErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lex(error) => write!(formatter, "{error}"),
            Self::Expected { expected, found } => {
                write!(formatter, "expected {expected}, found {}", found.describe())
            }
            Self::UnexpectedEof { expected } => {
                write!(formatter, "unexpected end of input, expected {expected}")
            }
            Self::TrailingContent { found } => write!(
                formatter,
                "unexpected {} after the top-level value",
                found.describe()
            ),
            Self::UnexpectedBom => formatter.write_str(
                "leading byte-order mark (enable allow_bom to tolerate it per RFC 8259 \u{a7}8.1)",
            ),
            Self::DepthLimitExceeded { limit } => {
                write!(formatter, "nesting depth limit of {limit} exceeded")
            }
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            // The lexer's message already carries its byte position.
            JsonErrorKind::Lex(error) => write!(formatter, "{error}"),
            kind => write!(
                formatter,
                "{kind} at line {}, column {} (byte {})",
                self.position.line, self.position.column, self.position.byte
            ),
        }
    }
}

impl Error for JsonError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            JsonErrorKind::Lex(error) => Some(error),
            _ => None,
        }
    }
}

impl From<LexError> for JsonError {
    fn from(error: LexError) -> Self {
        Self {
            position: error.position,
            kind: JsonErrorKind::Lex(error),
        }
    }
}

/// A reusable, strict RFC 8259 parser.
///
/// `M` is the token rule set handed to the lexer for every parse; it defaults to the strict
/// RFC 8259 rules. The parser clones the rules into a fresh scanner per [`JsonParser::parse`]
/// call, so one configured parser can serve many inputs.
#[derive(Debug, Clone)]
pub struct JsonParser<M = StrictRuleSet> {
    rules: M,
    allow_bom: bool,
    max_depth: usize,
    lexer_config: LexerConfig,
}

impl JsonParser<StrictRuleSet> {
    /// Creates a fully strict parser: strict rules, no BOM tolerance,
    /// [`DEFAULT_MAX_DEPTH`] nesting.
    pub fn new() -> Self {
        Self {
            rules: strict_rule_set(),
            allow_bom: false,
            max_depth: DEFAULT_MAX_DEPTH,
            lexer_config: LexerConfig::new(),
        }
    }

    /// Starts a builder for customizing spec-permitted options or replacing lower layers.
    pub fn builder() -> JsonParserBuilder<StrictRuleSet> {
        JsonParserBuilder::new()
    }
}

impl Default for JsonParser<StrictRuleSet> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> JsonParser<M>
where
    M: MatcherSet<Kind = JsonToken> + Clone,
{
    /// Parses a complete JSON text from `reader` into a [`Document`].
    ///
    /// Any RFC 8259 value is a valid top-level document (§2). The entire input must be consumed:
    /// trailing non-whitespace content is an error. On success the consumed source bytes are
    /// retained in the document's [`SourceStore`], keeping the CST zero-copy.
    pub fn parse<R: Read>(&self, reader: R) -> Result<Document, JsonError> {
        // Compaction must stay off so the consumed bytes survive until they are parked into the
        // document's source store.
        let config = self.lexer_config.with_compact_after(None);
        let scanner = SpecScanner::new(self.rules.clone(), JsonToken::Eof);
        let mut driver = Driver {
            lexer: Lexer::with_config(reader, scanner, config),
            pending: Vec::new(),
            peeked: None,
            allow_bom: self.allow_bom,
            max_depth: self.max_depth,
        };

        let root = driver.parse_value(0)?;
        let eof = driver.next_token()?;

        if eof.kind != JsonToken::Eof {
            return Err(JsonError {
                position: eof.start,
                kind: JsonErrorKind::TrailingContent { found: eof.kind },
            });
        }

        let (mut input, _scanner) = driver.lexer.into_parts();
        let mut source = SourceStore::new();
        input.park_consumed_buffer(&mut source)?;

        Ok(Document::new(root, eof.leading, source))
    }

    /// Parses a complete JSON text from a string slice.
    pub fn parse_str(&self, input: &str) -> Result<Document, JsonError> {
        self.parse(Cursor::new(input.as_bytes()))
    }

    /// Parses a complete JSON text from a byte slice. The bytes must be UTF-8 (RFC 8259 §8.1);
    /// violations inside string tokens are lexical errors, and any non-ASCII byte outside a
    /// string matches no rule.
    pub fn parse_bytes(&self, input: &[u8]) -> Result<Document, JsonError> {
        self.parse(Cursor::new(input))
    }

    /// The token rule set handed to the lexer at each parse.
    pub const fn rules(&self) -> &M {
        &self.rules
    }

    /// Whether a leading byte-order mark is tolerated (RFC 8259 §8.1).
    pub const fn allow_bom(&self) -> bool {
        self.allow_bom
    }

    /// The configured nesting-depth limit (RFC 8259 §9).
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }
}

/// Builder for [`JsonParser`], strict by default.
#[derive(Debug, Clone)]
pub struct JsonParserBuilder<M = StrictRuleSet> {
    rules: M,
    allow_bom: bool,
    max_depth: usize,
    lexer_config: LexerConfig,
}

impl JsonParserBuilder<StrictRuleSet> {
    /// Starts from the fully strict defaults.
    pub fn new() -> Self {
        Self {
            rules: strict_rule_set(),
            allow_bom: false,
            max_depth: DEFAULT_MAX_DEPTH,
            lexer_config: LexerConfig::new(),
        }
    }
}

impl Default for JsonParserBuilder<StrictRuleSet> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> JsonParserBuilder<M> {
    /// Replaces the token rule set handed to the lexer.
    ///
    /// This is the rules-layer composition point: tests can mock the rules, and adjusted rule
    /// sets can be swapped in without touching the grammar driver. The set must still produce
    /// [`JsonToken`] kinds; a superset *language* instead defines its own kind enum and parser,
    /// reusing this crate's rule structs via
    /// [`MatcherExt::map_kind`](strata_lexer::MatcherExt::map_kind).
    pub fn with_rules<N>(self, rules: N) -> JsonParserBuilder<N>
    where
        N: MatcherSet<Kind = JsonToken> + Clone,
    {
        JsonParserBuilder {
            rules,
            allow_bom: self.allow_bom,
            max_depth: self.max_depth,
            lexer_config: self.lexer_config,
        }
    }

    /// Replaces the lexer configuration (reader capacities, position tracking, ...).
    ///
    /// The parser always disables buffer compaction (`compact_after = None`) at parse time: the
    /// consumed bytes must survive until they are parked into the document's source store.
    #[must_use]
    pub const fn with_lexer_config(mut self, config: LexerConfig) -> Self {
        self.lexer_config = config;
        self
    }

    /// Tolerates one leading byte-order mark, retaining it as trivia.
    ///
    /// Off by default. RFC 8259 §8.1: a JSON text must not begin with a BOM, but implementations
    /// "MAY ignore the presence of a byte order mark rather than treating it as an error". With
    /// the option off, a leading BOM is [`JsonErrorKind::UnexpectedBom`]; a BOM anywhere else in
    /// the input is always an error.
    #[must_use]
    pub const fn allow_bom(mut self, allow: bool) -> Self {
        self.allow_bom = allow;
        self
    }

    /// Sets the nesting-depth limit.
    ///
    /// RFC 8259 §9: "An implementation may set limits on the maximum depth of nesting." The
    /// limit counts nested arrays/objects — with a limit of 1, `[1]` parses and `[[1]]` does
    /// not. It also bounds the parser's recursion depth; see [`DEFAULT_MAX_DEPTH`].
    #[must_use]
    pub const fn max_depth(mut self, limit: usize) -> Self {
        self.max_depth = limit;
        self
    }

    /// Finishes the builder.
    pub fn build(self) -> JsonParser<M>
    where
        M: MatcherSet<Kind = JsonToken> + Clone,
    {
        JsonParser {
            rules: self.rules,
            allow_bom: self.allow_bom,
            max_depth: self.max_depth,
            lexer_config: self.lexer_config,
        }
    }
}

/// The per-parse grammar driver: pulls tokens, folds trivia into the next significant token, and
/// recursively builds the CST.
struct Driver<R: Read, M: MatcherSet<Kind = JsonToken>> {
    lexer: Lexer<R, SpecScanner<M, JsonToken>>,
    pending: Vec<Trivia>,
    peeked: Option<SyntaxToken>,
    allow_bom: bool,
    max_depth: usize,
}

impl<R: Read, M: MatcherSet<Kind = JsonToken>> Driver<R, M> {
    /// Returns the next significant token with its leading trivia attached.
    fn next_token(&mut self) -> Result<SyntaxToken, JsonError> {
        if let Some(token) = self.peeked.take() {
            return Ok(token);
        }

        loop {
            let token = self.lexer.next_token()?;

            match token.kind {
                JsonToken::Whitespace => self.push_trivia(TriviaKind::Whitespace, &token),
                JsonToken::Bom => {
                    if self.allow_bom {
                        self.push_trivia(TriviaKind::Bom, &token);
                    } else {
                        return Err(JsonError {
                            position: token.start,
                            kind: JsonErrorKind::UnexpectedBom,
                        });
                    }
                }
                _ => {
                    return Ok(SyntaxToken {
                        kind: token.kind,
                        span: token.span,
                        start: token.start,
                        end: token.end,
                        leading: std::mem::take(&mut self.pending),
                    });
                }
            }
        }
    }

    /// Peeks the kind of the next significant token without consuming it.
    fn peek_kind(&mut self) -> Result<JsonToken, JsonError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.next_token()?);
        }

        Ok(self
            .peeked
            .as_ref()
            .map_or(JsonToken::Eof, |token| token.kind))
    }

    fn push_trivia(&mut self, kind: TriviaKind, token: &Token<JsonToken>) {
        self.pending.push(Trivia {
            kind,
            span: token.span,
            start: token.start,
            end: token.end,
        });
    }

    /// Parses one value. `depth` is the number of containers already entered.
    fn parse_value(&mut self, depth: usize) -> Result<Value, JsonError> {
        let token = self.next_token()?;

        match token.kind {
            JsonToken::LeftBrace => self.parse_object(token, depth),
            JsonToken::LeftBracket => self.parse_array(token, depth),
            JsonToken::String => Ok(Value::String(token)),
            JsonToken::Number => Ok(Value::Number(token)),
            JsonToken::True => Ok(Value::Bool { value: true, token }),
            JsonToken::False => Ok(Value::Bool {
                value: false,
                token,
            }),
            JsonToken::Null => Ok(Value::Null(token)),
            JsonToken::Eof => Err(JsonError {
                position: token.start,
                kind: JsonErrorKind::UnexpectedEof {
                    expected: "a JSON value",
                },
            }),
            found => Err(JsonError {
                position: token.start,
                kind: JsonErrorKind::Expected {
                    expected: "a JSON value",
                    found,
                },
            }),
        }
    }

    /// Checks the depth limit on entering a container, returning the child depth.
    fn enter_container(&self, depth: usize, open: &SyntaxToken) -> Result<usize, JsonError> {
        let entered = depth.saturating_add(1);

        if entered > self.max_depth {
            Err(JsonError {
                position: open.start,
                kind: JsonErrorKind::DepthLimitExceeded {
                    limit: self.max_depth,
                },
            })
        } else {
            Ok(entered)
        }
    }

    fn parse_object(&mut self, open: SyntaxToken, depth: usize) -> Result<Value, JsonError> {
        let depth = self.enter_container(depth, &open)?;
        let mut members = Vec::new();

        if self.peek_kind()? == JsonToken::RightBrace {
            let close = self.next_token()?;
            return Ok(Value::Object(ObjectNode {
                open,
                members,
                close,
            }));
        }

        loop {
            let name = self.next_token()?;

            match name.kind {
                JsonToken::String => {}
                JsonToken::Eof => {
                    return Err(JsonError {
                        position: name.start,
                        kind: JsonErrorKind::UnexpectedEof {
                            expected: "an object member name (a string)",
                        },
                    });
                }
                found => {
                    return Err(JsonError {
                        position: name.start,
                        kind: JsonErrorKind::Expected {
                            expected: "an object member name (a string)",
                            found,
                        },
                    });
                }
            }

            let colon = self.expect(JsonToken::Colon, "':'")?;
            let value = self.parse_value(depth)?;
            let separator = self.next_token()?;

            match separator.kind {
                JsonToken::Comma => members.push(Member {
                    name,
                    colon,
                    value,
                    comma: Some(separator),
                }),
                JsonToken::RightBrace => {
                    members.push(Member {
                        name,
                        colon,
                        value,
                        comma: None,
                    });

                    return Ok(Value::Object(ObjectNode {
                        open,
                        members,
                        close: separator,
                    }));
                }
                JsonToken::Eof => {
                    return Err(JsonError {
                        position: separator.start,
                        kind: JsonErrorKind::UnexpectedEof {
                            expected: "',' or '}'",
                        },
                    });
                }
                found => {
                    return Err(JsonError {
                        position: separator.start,
                        kind: JsonErrorKind::Expected {
                            expected: "',' or '}'",
                            found,
                        },
                    });
                }
            }
        }
    }

    fn parse_array(&mut self, open: SyntaxToken, depth: usize) -> Result<Value, JsonError> {
        let depth = self.enter_container(depth, &open)?;
        let mut elements = Vec::new();

        if self.peek_kind()? == JsonToken::RightBracket {
            let close = self.next_token()?;
            return Ok(Value::Array(ArrayNode {
                open,
                elements,
                close,
            }));
        }

        loop {
            let value = self.parse_value(depth)?;
            let separator = self.next_token()?;

            match separator.kind {
                JsonToken::Comma => elements.push(Element {
                    value,
                    comma: Some(separator),
                }),
                JsonToken::RightBracket => {
                    elements.push(Element { value, comma: None });

                    return Ok(Value::Array(ArrayNode {
                        open,
                        elements,
                        close: separator,
                    }));
                }
                JsonToken::Eof => {
                    return Err(JsonError {
                        position: separator.start,
                        kind: JsonErrorKind::UnexpectedEof {
                            expected: "',' or ']'",
                        },
                    });
                }
                found => {
                    return Err(JsonError {
                        position: separator.start,
                        kind: JsonErrorKind::Expected {
                            expected: "',' or ']'",
                            found,
                        },
                    });
                }
            }
        }
    }

    /// Consumes the next token, requiring `kind`.
    fn expect(
        &mut self,
        kind: JsonToken,
        describe: &'static str,
    ) -> Result<SyntaxToken, JsonError> {
        let token = self.next_token()?;

        if token.kind == kind {
            Ok(token)
        } else if token.kind == JsonToken::Eof {
            Err(JsonError {
                position: token.start,
                kind: JsonErrorKind::UnexpectedEof { expected: describe },
            })
        } else {
            Err(JsonError {
                position: token.start,
                kind: JsonErrorKind::Expected {
                    expected: describe,
                    found: token.kind,
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_fully_strict() {
        let parser = JsonParser::new();
        assert!(!parser.allow_bom());
        assert_eq!(parser.max_depth(), DEFAULT_MAX_DEPTH);
    }

    #[test]
    fn builder_defaults_match_new() {
        let parser = JsonParser::builder().build();
        assert!(!parser.allow_bom());
        assert_eq!(parser.max_depth(), DEFAULT_MAX_DEPTH);
    }

    #[test]
    fn builder_sets_options() {
        let parser = JsonParser::builder().allow_bom(true).max_depth(4).build();
        assert!(parser.allow_bom());
        assert_eq!(parser.max_depth(), 4);
    }

    #[test]
    fn error_display_includes_position() {
        let Err(error) = JsonParser::new().parse_str("[1 2]") else {
            panic!("expected a parse error");
        };
        let message = error.to_string();
        assert!(message.contains("expected ',' or ']'"), "{message}");
        assert!(message.contains("line 1"), "{message}");
        assert!(message.contains("byte 3"), "{message}");
    }

    #[test]
    fn bom_error_display_mentions_the_option() {
        let Err(error) = JsonParser::new().parse_str("\u{FEFF}1") else {
            panic!("expected a parse error");
        };
        assert!(matches!(error.kind, JsonErrorKind::UnexpectedBom));
        assert!(error.to_string().contains("allow_bom"), "{error}");
    }
}
