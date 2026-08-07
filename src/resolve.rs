//! The resolver: CST in, owned document model out.

use crate::document::JsonDocument;
use crate::error::{JsonError, JsonErrorKind};
use crate::value::JsonArray;
use crate::value::{JsonMember, JsonObject, JsonValue, Meta};
use std::collections::HashSet;
use std::io::{Cursor, Read};
use strata_json_parser::decode::{self, LoneSurrogates};
use strata_json_parser::{
    JsonParser, JsonParserBuilder, JsonToken, StrictRuleSet, SyntaxToken, Value,
};
use strata_lexer::{LexerConfig, MatcherSet, SourceStore, Span};

/// Policy for repeated object member names.
///
/// RFC 8259 §4 says names *should* be unique but does not require it, and leaves the behavior of
/// software receiving duplicates unpredictable; a resolver therefore chooses a policy up front.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DuplicateKeys {
    /// Retain every member in source order; [`JsonObject::get`] resolves to the last
    /// occurrence. The default.
    #[default]
    Retain,
    /// Reject the document with [`JsonErrorKind::DuplicateMember`] at the repeated name.
    Error,
}

/// A reusable JSON resolver: parses input and resolves the CST into a [`JsonDocument`].
///
/// `M` is the token rule set handed down to the parser; it defaults to the strict RFC 8259
/// rules. [`JsonResolver::new`] is the fully strict path; [`JsonResolver::builder`] exposes the
/// parser's spec-permitted options plus the two resolution policies ([`DuplicateKeys`],
/// [`LoneSurrogates`]).
#[derive(Debug, Clone)]
pub struct JsonResolver<M = StrictRuleSet> {
    parser: JsonParser<M>,
    duplicate_keys: DuplicateKeys,
    lone_surrogates: LoneSurrogates,
}

impl JsonResolver<StrictRuleSet> {
    /// Creates a fully strict resolver: strict parser defaults, duplicates retained, lone
    /// surrogates rejected.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: JsonParser::new(),
            duplicate_keys: DuplicateKeys::default(),
            lone_surrogates: LoneSurrogates::default(),
        }
    }

    /// Starts a builder for customizing options or replacing lower layers.
    #[must_use]
    pub fn builder() -> JsonResolverBuilder<StrictRuleSet> {
        JsonResolverBuilder::new()
    }
}

impl Default for JsonResolver<StrictRuleSet> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> JsonResolver<M>
where
    M: MatcherSet<Kind = JsonToken> + Clone,
{
    /// Parses and resolves a complete JSON text from `reader`.
    pub fn resolve<R: Read>(&self, reader: R) -> Result<JsonDocument, JsonError> {
        self.resolve_document(self.parser.parse(reader)?)
    }

    /// Parses and resolves a complete JSON text from a string slice.
    pub fn resolve_str(&self, input: &str) -> Result<JsonDocument, JsonError> {
        self.resolve(Cursor::new(input.as_bytes()))
    }

    /// Parses and resolves a complete JSON text from a byte slice.
    pub fn resolve_bytes(&self, input: &[u8]) -> Result<JsonDocument, JsonError> {
        self.resolve(Cursor::new(input))
    }

    /// Resolves an already-parsed [`Document`](strata_json_parser::Document).
    ///
    /// The document's retained source must still back the tree's spans; a document whose store
    /// was reclaimed first resolves to [`JsonErrorKind::SourceUnavailable`].
    pub fn resolve_document(
        &self,
        document: strata_json_parser::Document,
    ) -> Result<JsonDocument, JsonError> {
        let (root, trailing, source) = document.into_parts();
        let resolution = Resolution {
            source: &source,
            lone_surrogates: self.lone_surrogates,
            duplicate_keys: self.duplicate_keys,
        };
        let root = resolution.resolve_value(&root)?;

        Ok(JsonDocument::new(root, trailing, source))
    }

    /// The configured duplicate-member policy.
    #[must_use]
    pub const fn duplicate_keys(&self) -> DuplicateKeys {
        self.duplicate_keys
    }

    /// The configured lone-surrogate policy.
    #[must_use]
    pub const fn lone_surrogates(&self) -> LoneSurrogates {
        self.lone_surrogates
    }

    /// The underlying parser.
    #[must_use]
    pub const fn parser(&self) -> &JsonParser<M> {
        &self.parser
    }
}

/// Builder for [`JsonResolver`], strict by default.
#[derive(Debug, Clone)]
pub struct JsonResolverBuilder<M = StrictRuleSet> {
    parser: JsonParserBuilder<M>,
    duplicate_keys: DuplicateKeys,
    lone_surrogates: LoneSurrogates,
}

impl JsonResolverBuilder<StrictRuleSet> {
    /// Starts from the fully strict defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: JsonParserBuilder::new(),
            duplicate_keys: DuplicateKeys::default(),
            lone_surrogates: LoneSurrogates::default(),
        }
    }
}

impl Default for JsonResolverBuilder<StrictRuleSet> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> JsonResolverBuilder<M> {
    /// Replaces the token rule set handed down to the parser.
    pub fn with_rules<N>(self, rules: N) -> JsonResolverBuilder<N>
    where
        N: MatcherSet<Kind = JsonToken> + Clone,
    {
        JsonResolverBuilder {
            parser: self.parser.with_rules(rules),
            duplicate_keys: self.duplicate_keys,
            lone_surrogates: self.lone_surrogates,
        }
    }

    /// Replaces the lexer configuration handed down to the parser.
    #[must_use]
    pub fn with_lexer_config(mut self, config: LexerConfig) -> Self {
        self.parser = self.parser.with_lexer_config(config);
        self
    }

    /// Tolerates one leading byte-order mark (RFC 8259 §8.1). Off by default.
    #[must_use]
    pub fn allow_bom(mut self, allow: bool) -> Self {
        self.parser = self.parser.allow_bom(allow);
        self
    }

    /// Sets the parser's nesting-depth limit (RFC 8259 §9).
    #[must_use]
    pub fn max_depth(mut self, limit: usize) -> Self {
        self.parser = self.parser.max_depth(limit);
        self
    }

    /// Sets the repeated-member-name policy. Retain by default.
    #[must_use]
    pub const fn duplicate_keys(mut self, policy: DuplicateKeys) -> Self {
        self.duplicate_keys = policy;
        self
    }

    /// Sets the lone-surrogate policy for string decoding. Error by default.
    #[must_use]
    pub const fn lone_surrogates(mut self, policy: LoneSurrogates) -> Self {
        self.lone_surrogates = policy;
        self
    }

    /// Finishes the builder.
    pub fn build(self) -> JsonResolver<M>
    where
        M: MatcherSet<Kind = JsonToken> + Clone,
    {
        JsonResolver {
            parser: self.parser.build(),
            duplicate_keys: self.duplicate_keys,
            lone_surrogates: self.lone_surrogates,
        }
    }
}

/// One resolution pass over a parsed tree, reading raw lexemes from the retained source.
struct Resolution<'source> {
    source: &'source SourceStore,
    lone_surrogates: LoneSurrogates,
    duplicate_keys: DuplicateKeys,
}

impl Resolution<'_> {
    fn resolve_value(&self, value: &Value) -> Result<JsonValue, JsonError> {
        match value {
            Value::Null(token) => Ok(JsonValue::Null(token_meta(token))),
            Value::Bool { value, token } => Ok(JsonValue::Bool(*value, token_meta(token))),
            Value::Number(token) => {
                let raw = self.raw_bytes(token.span())?;
                let number =
                    decode::parse_number(&raw).map_err(|error| Self::decode_error(token, error))?;

                Ok(JsonValue::Number(number, token_meta(token)))
            }
            Value::String(token) => {
                let text = self.decode_string(token)?;

                Ok(JsonValue::String(text, token_meta(token)))
            }
            Value::Array(array) => {
                let elements = array
                    .elements()
                    .iter()
                    .map(|element| self.resolve_value(element.value()))
                    .collect::<Result<Vec<_>, _>>()?;
                let meta = node_meta(array.open(), array.close());

                Ok(JsonValue::Array(JsonArray::new(meta, elements)))
            }
            Value::Object(object) => {
                let mut members = Vec::with_capacity(object.members().len());

                for member in object.members() {
                    let name = self.decode_string(member.name())?;
                    let name_meta = token_meta(member.name());
                    let value = self.resolve_value(member.value())?;
                    members.push(JsonMember::new(name, name_meta, value));
                }

                if self.duplicate_keys == DuplicateKeys::Error {
                    reject_duplicates(&members)?;
                }

                let meta = node_meta(object.open(), object.close());

                Ok(JsonValue::Object(JsonObject::new(meta, members)))
            }
        }
    }

    /// Decodes a string token's content, mapping decode failures onto resolver errors.
    fn decode_string(&self, token: &SyntaxToken) -> Result<String, JsonError> {
        let raw = self.raw_bytes(token.span())?;

        decode::unescape_string(&raw, self.lone_surrogates)
            .map_err(|error| Self::decode_error(token, error))
    }

    /// Reads the raw bytes behind `span`, borrowing when one buffer covers it.
    fn raw_bytes(&self, span: Span) -> Result<std::borrow::Cow<'_, [u8]>, JsonError> {
        if let Some(bytes) = self.source.bytes(span) {
            return Ok(std::borrow::Cow::Borrowed(bytes));
        }

        match self.source.copy_bytes(span) {
            Some(bytes) => Ok(std::borrow::Cow::Owned(bytes)),
            None => Err(JsonError {
                position: strata_lexer::Position::START,
                kind: JsonErrorKind::SourceUnavailable { span },
            }),
        }
    }

    /// Maps a decode failure at `token` onto a resolver error with an absolute position.
    ///
    /// The offset advances on the token's start line: RFC 8259 string and number lexemes cannot
    /// contain raw line breaks, so the line is exact for strict-rule tokens; for replaced rule
    /// sets admitting multi-line lexemes the line may understate.
    fn decode_error(token: &SyntaxToken, error: decode::Error) -> JsonError {
        let position = token
            .start()
            .advanced_by(u64::try_from(error.offset).unwrap_or(u64::MAX));

        match error.kind {
            decode::ErrorKind::LoneSurrogate { unit } => JsonError {
                position,
                kind: JsonErrorKind::LoneSurrogate { unit },
            },
            decode::ErrorKind::MalformedToken => JsonError {
                position,
                kind: JsonErrorKind::InvalidLexeme { error },
            },
        }
    }
}

/// Builds the fidelity metadata of a scalar or name token.
fn token_meta(token: &SyntaxToken) -> Meta {
    Meta::new(
        token.span(),
        token.start(),
        token.end(),
        token.leading().to_vec(),
    )
}

/// Builds the fidelity metadata of a container from its open and close tokens.
fn node_meta(open: &SyntaxToken, close: &SyntaxToken) -> Meta {
    let start = open.span().start();
    let span = Span::from_start_len(start, close.span().end().saturating_sub(start));

    Meta::new(span, open.start(), close.end(), open.leading().to_vec())
}

/// Rejects the first member shadowing an earlier name, blaming the later occurrence.
fn reject_duplicates(members: &[JsonMember]) -> Result<(), JsonError> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(members.len());

    for member in members {
        if !seen.insert(member.name()) {
            return Err(JsonError {
                position: member.name_meta().start(),
                kind: JsonErrorKind::DuplicateMember {
                    name: member.name().to_owned(),
                },
            });
        }
    }

    Ok(())
}
