//! The concrete syntax tree (CST): a lossless, zero-copy document shape.
//!
//! The CST retains what the author wrote, not just what it evaluates to: every syntactic token
//! keeps its byte span and position, trivia (whitespace runs, a tolerated byte-order mark) is
//! attached to the token it precedes instead of being discarded, and string/number content stays
//! as spans into the retained source — no eager decoding, no owned copies. Concatenating every
//! token's leading trivia and lexeme in source order reproduces the input byte-for-byte.
//!
//! Duplicate object member names are retained verbatim, in source order. RFC 8259 §4 says names
//! *should* be unique but does not require it; a fidelity-preserving CST records what was
//! written and leaves duplicate policy to downstream consumers.

use crate::rules::JsonToken;
use strata_lexer::{Position, SourceRef, SourceStore, Span};

/// Classification of a retained trivia piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriviaKind {
    /// A run of insignificant whitespace (RFC 8259 §2).
    Whitespace,
    /// A tolerated leading byte-order mark (RFC 8259 §8.1, `allow_bom`).
    Bom,
}

/// A retained piece of grammatically insignificant source content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trivia {
    /// What kind of trivia this piece is.
    pub kind: TriviaKind,
    /// Absolute byte range in the source.
    pub span: Span,
    /// Position of the first byte.
    pub start: Position,
    /// Position just past the last byte.
    pub end: Position,
}

impl Trivia {
    /// The number of line breaks inside this trivia piece.
    pub const fn line_breaks(&self) -> u64 {
        self.end.line.saturating_sub(self.start.line)
    }

    /// The number of blank lines inside this trivia piece.
    ///
    /// A whitespace run separating two tokens contains one line break when the tokens merely sit
    /// on adjacent lines; every additional break is a blank line — the kind of implicit visual
    /// grouping the Strata ecosystem preserves.
    pub const fn blank_lines(&self) -> u64 {
        self.line_breaks().saturating_sub(1)
    }
}

/// A significant token in the CST: kind, span, positions, and the trivia that preceded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxToken {
    /// The token kind.
    pub kind: JsonToken,
    /// Absolute byte range of the lexeme (excluding leading trivia).
    pub span: Span,
    /// Position of the first lexeme byte.
    pub start: Position,
    /// Position just past the last lexeme byte.
    pub end: Position,
    /// Trivia between the previous significant token and this one, in source order.
    pub leading: Vec<Trivia>,
}

/// A JSON value node.
///
/// Scalar variants carry the token whose span addresses the raw source bytes — a string token's
/// span includes both quotes, and its content is *not* decoded here. Container variants keep
/// every structural token, including the commas between entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// The `null` literal.
    Null(SyntaxToken),
    /// A `true` or `false` literal.
    Bool {
        /// The literal's value.
        value: bool,
        /// The literal token.
        token: SyntaxToken,
    },
    /// A number; the token span covers the raw, unconverted source text.
    Number(SyntaxToken),
    /// A string; the token span covers the raw source text including both quotes.
    String(SyntaxToken),
    /// An array.
    Array(ArrayNode),
    /// An object.
    Object(ObjectNode),
}

impl Value {
    /// The first significant token of this value, whose leading trivia is everything between the
    /// previous token and this value.
    pub const fn first_token(&self) -> &SyntaxToken {
        match self {
            Self::Null(token)
            | Self::Number(token)
            | Self::String(token)
            | Self::Bool { token, .. } => token,
            Self::Array(array) => &array.open,
            Self::Object(object) => &object.open,
        }
    }

    /// The last significant token of this value.
    pub const fn last_token(&self) -> &SyntaxToken {
        match self {
            Self::Null(token)
            | Self::Number(token)
            | Self::String(token)
            | Self::Bool { token, .. } => token,
            Self::Array(array) => &array.close,
            Self::Object(object) => &object.close,
        }
    }

    /// The absolute byte range this value covers, from its first to its last lexeme byte
    /// (leading trivia excluded).
    pub const fn span(&self) -> Span {
        Span::new(self.first_token().span.start, self.last_token().span.end)
    }

    /// Visits every significant token of this value in source order.
    pub fn for_each_token<'value>(&'value self, visit: &mut impl FnMut(&'value SyntaxToken)) {
        match self {
            Self::Null(token)
            | Self::Number(token)
            | Self::String(token)
            | Self::Bool { token, .. } => visit(token),
            Self::Array(array) => {
                visit(&array.open);

                for element in &array.elements {
                    element.value.for_each_token(visit);

                    if let Some(comma) = &element.comma {
                        visit(comma);
                    }
                }

                visit(&array.close);
            }
            Self::Object(object) => {
                visit(&object.open);

                for member in &object.members {
                    visit(&member.name);
                    visit(&member.colon);
                    member.value.for_each_token(visit);

                    if let Some(comma) = &member.comma {
                        visit(comma);
                    }
                }

                visit(&object.close);
            }
        }
    }
}

/// An array node: `[` *elements* `]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayNode {
    /// The `[` token.
    pub open: SyntaxToken,
    /// The elements in source order.
    pub elements: Vec<Element>,
    /// The `]` token.
    pub close: SyntaxToken,
}

/// One array element and the comma that followed it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    /// The element value.
    pub value: Value,
    /// The `,` after this element; `None` on the last element.
    pub comma: Option<SyntaxToken>,
}

/// An object node: `{` *members* `}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectNode {
    /// The `{` token.
    pub open: SyntaxToken,
    /// The members in source order, duplicates included.
    pub members: Vec<Member>,
    /// The `}` token.
    pub close: SyntaxToken,
}

/// One object member: *name* `:` *value*, plus the comma that followed it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member name; a string token whose span includes both quotes.
    pub name: SyntaxToken,
    /// The `:` token.
    pub colon: SyntaxToken,
    /// The member value.
    pub value: Value,
    /// The `,` after this member; `None` on the last member.
    pub comma: Option<SyntaxToken>,
}

/// A parsed document: the root value, trailing trivia, and the retained source.
///
/// The document owns the [`SourceStore`] holding the original bytes, so every span in the tree
/// stays addressable — zero-copy — for as long as the document lives. [`Document::source_ref`]
/// hands out promotable handles that survive even store reclamation (see `strata-lexer`'s
/// retention layer); [`Document::bytes`] is the plain borrow path.
#[derive(Debug)]
pub struct Document {
    root: Value,
    trailing: Vec<Trivia>,
    source: SourceStore,
}

impl Document {
    /// Assembles a document from its parts. Used by the parser.
    pub(crate) const fn new(root: Value, trailing: Vec<Trivia>, source: SourceStore) -> Self {
        Self {
            root,
            trailing,
            source,
        }
    }

    /// The root value. Any RFC 8259 value is a valid top-level document (§2).
    pub const fn root(&self) -> &Value {
        &self.root
    }

    /// Trivia between the end of the root value and the end of input.
    pub fn trailing_trivia(&self) -> &[Trivia] {
        &self.trailing
    }

    /// The tolerated leading byte-order mark, if the input carried one and the parser was
    /// configured with `allow_bom`.
    pub fn bom(&self) -> Option<&Trivia> {
        self.root
            .first_token()
            .leading
            .first()
            .filter(|trivia| trivia.kind == TriviaKind::Bom)
    }

    /// Borrows the raw source bytes covered by `span`, zero-copy.
    ///
    /// Returns `None` if the span is not (or no longer) backed by the retained source, e.g.
    /// after [`Document::source_mut`] was used to retire buffers.
    pub fn bytes(&self, span: Span) -> Option<&[u8]> {
        self.source.bytes(span)
    }

    /// Borrows the raw lexeme bytes of `token`, zero-copy.
    pub fn token_bytes(&self, token: &SyntaxToken) -> Option<&[u8]> {
        self.bytes(token.span)
    }

    /// Creates a promotable, owned-on-demand handle to the bytes covered by `span`.
    ///
    /// This is the resolver-facing path: the handle stays zero-copy while the store retains the
    /// buffer and degrades to a small owned copy if the store reclaims it.
    pub fn source_ref(&mut self, span: Span) -> Option<SourceRef> {
        self.source.source_ref(span)
    }

    /// The retained source store.
    pub const fn source(&self) -> &SourceStore {
        &self.source
    }

    /// Mutable access to the retained source store, e.g. for owner-side reclamation
    /// (`maintain` / `retire_all`).
    pub const fn source_mut(&mut self) -> &mut SourceStore {
        &mut self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn position(byte: u64, line: u64, column: u64) -> Position {
        Position { byte, line, column }
    }

    fn trivia(start: Position, end: Position) -> Trivia {
        Trivia {
            kind: TriviaKind::Whitespace,
            span: Span::new(start.byte, end.byte),
            start,
            end,
        }
    }

    #[test]
    fn single_space_has_no_line_breaks() {
        let piece = trivia(position(1, 1, 2), position(2, 1, 3));
        assert_eq!(piece.line_breaks(), 0);
        assert_eq!(piece.blank_lines(), 0);
    }

    #[test]
    fn newline_between_lines_is_a_break_but_not_blank() {
        let piece = trivia(position(1, 1, 2), position(2, 2, 1));
        assert_eq!(piece.line_breaks(), 1);
        assert_eq!(piece.blank_lines(), 0);
    }

    #[test]
    fn double_newline_is_one_blank_line() {
        let piece = trivia(position(1, 1, 2), position(3, 3, 1));
        assert_eq!(piece.line_breaks(), 2);
        assert_eq!(piece.blank_lines(), 1);
    }

    #[test]
    fn value_first_and_last_token_cover_containers() {
        let token = |kind: JsonToken, start: u64, end: u64| SyntaxToken {
            kind,
            span: Span::new(start, end),
            start: position(start, 1, start.saturating_add(1)),
            end: position(end, 1, end.saturating_add(1)),
            leading: Vec::new(),
        };
        let array = Value::Array(ArrayNode {
            open: token(JsonToken::LeftBracket, 0, 1),
            elements: vec![Element {
                value: Value::Number(token(JsonToken::Number, 1, 2)),
                comma: None,
            }],
            close: token(JsonToken::RightBracket, 2, 3),
        });

        assert_eq!(array.first_token().kind, JsonToken::LeftBracket);
        assert_eq!(array.last_token().kind, JsonToken::RightBracket);
        assert_eq!(array.span(), Span::new(0, 3));

        let mut kinds = Vec::new();
        array.for_each_token(&mut |token| kinds.push(token.kind));
        assert_eq!(
            kinds,
            vec![
                JsonToken::LeftBracket,
                JsonToken::Number,
                JsonToken::RightBracket,
            ]
        );
    }
}
