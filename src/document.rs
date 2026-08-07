//! The resolved document: owned values plus the retained source.

use crate::value::JsonValue;
use strata_json_parser::Trivia;
use strata_lexer::{SourceRef, SourceStore, Span};

/// A resolved JSON document: the owned root value, trailing trivia, and the retained source.
///
/// The document keeps the [`SourceStore`] of the parse it was resolved from, so every
/// [`Meta`](crate::Meta) span in the model stays addressable: [`JsonDocument::bytes`] borrows
/// raw source text zero-copy, and [`JsonDocument::source_ref`] hands out promotable handles that
/// survive store reclamation.
#[derive(Debug)]
pub struct JsonDocument {
    root: JsonValue,
    trailing: Vec<Trivia>,
    source: SourceStore,
}

impl JsonDocument {
    pub(crate) const fn new(root: JsonValue, trailing: Vec<Trivia>, source: SourceStore) -> Self {
        Self {
            root,
            trailing,
            source,
        }
    }

    /// The root value.
    #[inline]
    #[must_use]
    pub const fn root(&self) -> &JsonValue {
        &self.root
    }

    /// Trivia between the end of the root value and the end of input.
    #[inline]
    #[must_use]
    pub fn trailing_trivia(&self) -> &[Trivia] {
        &self.trailing
    }

    /// Borrows the raw source bytes covered by `span`, zero-copy.
    ///
    /// Returns `None` if the span is not (or no longer) backed by the retained source, e.g.
    /// after [`JsonDocument::source_mut`] was used to retire buffers.
    #[must_use]
    pub fn bytes(&self, span: Span) -> Option<&[u8]> {
        self.source.bytes(span)
    }

    /// Creates a promotable, owned-on-demand handle to the bytes covered by `span`.
    pub fn source_ref(&mut self, span: Span) -> Option<SourceRef> {
        self.source.source_ref(span)
    }

    /// The retained source store.
    #[inline]
    #[must_use]
    pub const fn source(&self) -> &SourceStore {
        &self.source
    }

    /// Mutable access to the retained source store, e.g. for owner-side reclamation
    /// (`maintain` / `retire_all`). The owned values stay intact either way; only raw-byte
    /// lookups through [`JsonDocument::bytes`] stop resolving for reclaimed spans.
    #[inline]
    #[must_use]
    pub const fn source_mut(&mut self) -> &mut SourceStore {
        &mut self.source
    }

    /// Decomposes the document into the root value, trailing trivia, and retained source.
    #[must_use]
    pub fn into_parts(self) -> (JsonValue, Vec<Trivia>, SourceStore) {
        (self.root, self.trailing, self.source)
    }
}
