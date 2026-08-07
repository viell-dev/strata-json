//! The owned document model: values with decoded content and per-node fidelity.

use strata_json_parser::Trivia;
use strata_lexer::{Position, Span};

pub use strata_json_parser::decode::Number as JsonNumber;

/// Per-node source fidelity: where the node sits and what preceded it.
///
/// Metas are only created by resolution, from CST nodes whose positions are ordered by
/// construction; the raw source bytes behind [`Meta::span`] stay reachable through
/// [`JsonDocument::bytes`](crate::JsonDocument::bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    span: Span,
    start: Position,
    end: Position,
    leading: Vec<Trivia>,
}

impl Meta {
    pub(crate) const fn new(
        span: Span,
        start: Position,
        end: Position,
        leading: Vec<Trivia>,
    ) -> Self {
        Self {
            span,
            start,
            end,
            leading,
        }
    }

    /// The absolute byte range of the node's lexemes, first to last (leading trivia excluded).
    #[inline]
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Position of the node's first lexeme byte.
    #[inline]
    #[must_use]
    pub const fn start(&self) -> Position {
        self.start
    }

    /// Position just past the node's last lexeme byte.
    #[inline]
    #[must_use]
    pub const fn end(&self) -> Position {
        self.end
    }

    /// Trivia between the previous token and this node, in source order.
    #[inline]
    #[must_use]
    pub fn leading(&self) -> &[Trivia] {
        &self.leading
    }
}

/// An owned JSON value with decoded content and fidelity metadata.
///
/// Strings are unescaped and numbers converted ([`JsonNumber`] preserves integer identity where
/// the value fits); the raw source text of every node stays reachable through its [`Meta`] span
/// and the document. Object member order and duplicate members are retained.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// The `null` literal.
    Null(Meta),
    /// A `true` or `false` literal.
    Bool(bool, Meta),
    /// A number, converted integer-preserving.
    Number(JsonNumber, Meta),
    /// A string, unescaped.
    String(String, Meta),
    /// An array.
    Array(JsonArray),
    /// An object.
    Object(JsonObject),
}

impl JsonValue {
    /// The node's fidelity metadata.
    #[must_use]
    pub const fn meta(&self) -> &Meta {
        match self {
            Self::Null(meta)
            | Self::Bool(_, meta)
            | Self::Number(_, meta)
            | Self::String(_, meta) => meta,
            Self::Array(array) => array.meta(),
            Self::Object(object) => object.meta(),
        }
    }

    /// The absolute byte range the node covers.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.meta().span()
    }

    /// Returns `true` for `null`.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null(_))
    }

    /// The boolean value, if this is a boolean.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value, _) => Some(*value),
            _ => None,
        }
    }

    /// The converted number, if this is a number.
    #[must_use]
    pub const fn as_number(&self) -> Option<JsonNumber> {
        match self {
            Self::Number(value, _) => Some(*value),
            _ => None,
        }
    }

    /// The number as `f64`, if this is a number; integers convert (rounding beyond 2^53).
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().map(JsonNumber::as_f64)
    }

    /// The decoded string content, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value, _) => Some(value),
            _ => None,
        }
    }

    /// The array, if this is an array.
    #[must_use]
    pub const fn as_array(&self) -> Option<&JsonArray> {
        match self {
            Self::Array(array) => Some(array),
            _ => None,
        }
    }

    /// The object, if this is an object.
    #[must_use]
    pub const fn as_object(&self) -> Option<&JsonObject> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }
}

/// An owned JSON array.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonArray {
    meta: Meta,
    elements: Vec<JsonValue>,
}

impl JsonArray {
    pub(crate) const fn new(meta: Meta, elements: Vec<JsonValue>) -> Self {
        Self { meta, elements }
    }

    /// The array's fidelity metadata; the span covers `[` through `]`.
    #[inline]
    #[must_use]
    pub const fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The elements in source order.
    #[inline]
    #[must_use]
    pub fn elements(&self) -> &[JsonValue] {
        &self.elements
    }

    /// The number of elements.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Returns `true` when the array has no elements.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The element at `index`, if present.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&JsonValue> {
        self.elements.get(index)
    }

    /// Iterates the elements in source order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &JsonValue> {
        self.elements.iter()
    }
}

/// An owned JSON object: members in source order, duplicates retained.
///
/// RFC 8259 §4 says member names *should* be unique but does not require it. The model keeps
/// what was written; [`JsonObject::get`] resolves a name to its **last** occurrence — the
/// behavior of most JSON implementations (ECMA-262's `JSON.parse` keeps the last value) — while
/// [`JsonObject::members`], [`JsonObject::get_all`], and [`JsonObject::duplicates`] expose the
/// full fidelity. A resolver configured with
/// [`DuplicateKeys::Error`](crate::DuplicateKeys::Error) rejects duplicates up front instead.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonObject {
    meta: Meta,
    members: Vec<JsonMember>,
}

impl JsonObject {
    pub(crate) const fn new(meta: Meta, members: Vec<JsonMember>) -> Self {
        Self { meta, members }
    }

    /// The object's fidelity metadata; the span covers `{` through `}`.
    #[inline]
    #[must_use]
    pub const fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The members in source order, duplicates included.
    #[inline]
    #[must_use]
    pub fn members(&self) -> &[JsonMember] {
        &self.members
    }

    /// The number of members, duplicates included.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Returns `true` when the object has no members.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The value of the **last** member named `name`, if any.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&JsonValue> {
        self.members
            .iter()
            .rev()
            .find(|member| member.name() == name)
            .map(JsonMember::value)
    }

    /// Every member named `name`, in source order.
    pub fn get_all<'object>(
        &'object self,
        name: &'object str,
    ) -> impl Iterator<Item = &'object JsonMember> {
        self.members
            .iter()
            .filter(move |member| member.name() == name)
    }

    /// Returns `true` when a member named `name` exists.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.members.iter().any(|member| member.name() == name)
    }

    /// Iterates the members in source order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &JsonMember> {
        self.members.iter()
    }

    /// The members shadowed by a later member with the same name, in source order.
    pub fn duplicates(&self) -> impl Iterator<Item = &JsonMember> {
        self.members
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                let shadowed = self
                    .members
                    .get(index.saturating_add(1)..)
                    .is_some_and(|later| later.iter().any(|other| other.name() == member.name()));

                shadowed.then_some(member)
            })
    }
}

/// One owned object member: decoded name, name fidelity, and value.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonMember {
    name: String,
    name_meta: Meta,
    value: JsonValue,
}

impl JsonMember {
    pub(crate) const fn new(name: String, name_meta: Meta, value: JsonValue) -> Self {
        Self {
            name,
            name_meta,
            value,
        }
    }

    /// The decoded (unescaped) member name.
    #[inline]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name token's fidelity metadata; its span covers the raw name including quotes.
    #[inline]
    #[must_use]
    pub const fn name_meta(&self) -> &Meta {
        &self.name_meta
    }

    /// The member value.
    #[inline]
    #[must_use]
    pub const fn value(&self) -> &JsonValue {
        &self.value
    }
}
