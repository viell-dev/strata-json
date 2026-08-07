//! Strict RFC 8259 JSON parser for the Strata parser ecosystem.
//!
//! `strata-json` turns a byte stream into a lossless, zero-copy concrete syntax tree (CST): every
//! token keeps its absolute byte span, trivia (whitespace runs, blank lines, a tolerated
//! byte-order mark) is retained instead of discarded, and string/number content stays as raw
//! source bytes — decoding belongs to a downstream resolver or deserializer.
//!
//! The crate has three public layers:
//!
//! - [`rules`] — the RFC 8259 token rules, built on `strata-lexer`'s matcher layer. Each rule is
//!   exported individually so superset languages (JSONC, JSON5, ...) can reuse the shapes they
//!   share with JSON under their own token kind and add or replace only what differs.
//! - [`cst`] — the document shape: [`Document`], [`Value`], nodes, tokens, and trivia.
//! - [`parse`] — [`JsonParser`], strict by default via [`JsonParser::new`], with a builder for
//!   the two options RFC 8259 explicitly permits (tolerating a leading BOM per §8.1 and the
//!   nesting-depth limit per §9) and for replacing the lower layers.
//!
//! # Example
//!
//! ```
//! use strata_json::{JsonParser, Value};
//!
//! # fn main() -> Result<(), strata_json::JsonError> {
//! let document = JsonParser::new().parse_str(r#"{ "name": "strata" }"#)?;
//!
//! let Value::Object(object) = document.root() else {
//!     panic!("expected an object");
//! };
//! let member = object.members.first().unwrap();
//!
//! // Zero-copy: raw source bytes, quotes included, no decoding.
//! assert_eq!(document.token_bytes(&member.name), Some(&b"\"name\""[..]));
//! # Ok(())
//! # }
//! ```

pub mod cst;
pub mod parse;
pub mod rules;

pub use cst::{
    ArrayNode, Document, Element, Member, ObjectNode, SyntaxToken, Trivia, TriviaKind, Value,
};
pub use parse::{DEFAULT_MAX_DEPTH, JsonError, JsonErrorKind, JsonParser, JsonParserBuilder};
pub use rules::{
    BomRule, JsonToken, LiteralRule, NumberRule, PunctRule, SharedRuleSet, StrictRuleSet,
    StringRule, WhitespaceRule, strict_rule_set, strict_scanner,
};
