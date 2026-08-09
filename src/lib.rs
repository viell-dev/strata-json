//! JSON resolver for the Strata parser ecosystem: a dynamic, owned document model with source
//! fidelity.
//!
//! The resolver is the consumer-facing layer of the JSON stack: it drives
//! `strata-json-parser`, then turns the zero-copy CST into owned values — strings unescaped
//! (with UTF-16 surrogate-pair combination and a configurable lone-surrogate policy), numbers
//! converted integer-preserving — while keeping fidelity available on every node: byte spans,
//! positions, leading trivia (whitespace runs, blank lines, a tolerated BOM), member order, and
//! duplicate members. The raw source text behind any node stays reachable through
//! [`JsonDocument::bytes`] and promotable [`source_ref`](JsonDocument::source_ref) handles.
//!
//! [`JsonResolver::new`] is the fully strict path; [`JsonResolver::builder`] exposes the
//! parser's spec-permitted options (`allow_bom`, `max_depth`), lower-layer replacement, and the
//! two resolution policies: [`DuplicateKeys`] (retain by default, `get` resolves to the last
//! occurrence) and [`LoneSurrogates`] (reject by default). The model is read-only; editing and
//! writing back stay at the CST/source level of the lower layers.

mod document;
mod error;
mod resolve;
mod value;

pub use document::JsonDocument;
pub use error::{JsonError, JsonErrorKind};
pub use resolve::{DuplicateKeys, JsonResolver, JsonResolverBuilder};
pub use strata_json_parser::decode::LoneSurrogates;
pub use strata_json_parser::{Trivia, TriviaKind};
pub use value::{JsonArray, JsonMember, JsonNumber, JsonObject, JsonValue, Meta, NumberFormat};

/// Compile-checks the README examples alongside the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}
