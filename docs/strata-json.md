# strata-json

`strata-json` is the JSON resolver for the Strata parser ecosystem — the plain, consumer-facing
crate of the JSON stack. It drives `strata-json-parser` and turns the zero-copy CST into a
read-only, owned document model: strings unescaped, numbers converted integer-preserving, while
source fidelity stays available on every node (byte spans, positions, leading trivia, member
order, duplicate members) and the raw source text stays reachable through the document.

The crate sits at the resolver stage of the per-language stack:

```text
strata-json-rules -> strata-json-parser -> strata-json (this crate)
```

- **Edition / MSRV:** Rust 2024, MSRV `1.87.0`.
- **License:** MPL-2.0.
- **Dependencies:** `strata-json-parser`, `strata-lexer`.

## 1. Module and public-API map

| Module     | Public items |
|------------|--------------|
| `resolve`  | `JsonResolver<M>`, `JsonResolverBuilder<M>`, `DuplicateKeys` |
| `value`    | `JsonValue`, `JsonArray`, `JsonObject`, `JsonMember`, `JsonNumber`, `NumberFormat`, `Meta` |
| `document` | `JsonDocument` |
| `error`    | `JsonError`, `JsonErrorKind` |

Everything is re-exported at the crate root, plus `LoneSurrogates` and the parser's
`Trivia`/`TriviaKind`.

## 2. The resolver (`resolve`)

`JsonResolver::new()` / `default()` is the fully strict path: strict parser defaults, duplicates
retained, lone surrogates rejected. `resolve(impl Read)`, `resolve_str`, and `resolve_bytes`
parse and resolve in one step; `resolve_document(parser::Document)` resolves an already-parsed
CST (whose retained source must still back the tree — a reclaimed store yields
`SourceUnavailable`).

`JsonResolver::builder()` exposes the parser passthroughs (`allow_bom`, `max_depth`,
`with_rules`, `with_lexer_config`) plus the two resolution policies:

- `duplicate_keys(DuplicateKeys)` — `Retain` (default) keeps every member; `Error` rejects the
  document at the repeated name with the decoded name in the error.
- `lone_surrogates(LoneSurrogates)` — `Error` (default) rejects `"\uD800"`-style unpaired
  escapes at the offending backslash; `Replace` substitutes U+FFFD.

## 3. The model (`value`, `document`)

- `JsonValue` — `Null(Meta)`, `Bool(bool, Meta)`, `Number(JsonNumber, NumberFormat, Meta)`,
  `String(String, Meta)`, `Array(JsonArray)`, `Object(JsonObject)`, with `meta()`, `span()`, and
  `as_*`/`is_*` accessors. `JsonNumber` is the parser `decode::Number` re-exported: `U64` for
  non-negative integers that fit, `I64` for negative, `F64` otherwise (`-0` keeps its sign;
  oversized integers round with the raw text still reachable for arbitrary precision). The
  written form rides along as a `NumberFormat` (`Integer`, `Decimal`, `Exponential`) exposed by
  `JsonValue::number_format()` — author-intent fidelity without re-reading raw bytes.
- `Meta` — per-node fidelity: lexeme span (containers cover open through close), start/end
  positions, and the leading trivia (whitespace runs with `line_breaks()`/`blank_lines()`, a
  tolerated BOM). Metas are only created by resolution, so they always describe genuine nodes.
- `JsonObject` — members in source order, duplicates retained. `get(name)` resolves to the
  **last** occurrence (ECMA-262 `JSON.parse` behavior); `get_all`, `members`, and
  `duplicates()` (members shadowed by a later same-name member) expose full fidelity.
  `JsonMember` carries the decoded name, the name token's `Meta` (raw spelling reachable via its
  span), and the value.
- `JsonDocument` — owns the root, trailing trivia, and the retained `SourceStore`:
  `bytes(span)` borrows raw source zero-copy, `source_ref(span)` hands out promotable handles
  that survive store reclamation, `source_mut()` allows owner-side reclamation (owned values are
  unaffected; only raw-byte lookups stop resolving).

## 4. Errors

`JsonError { position, kind }` with `JsonErrorKind`: `Parse(parser error)` (lexical and grammar
failures pass through), `LoneSurrogate { unit }`, `DuplicateMember { name }` (at the repeated
name), `InvalidLexeme` (decode failure — only reachable with replaced rule sets whose tokens
deviate from the RFC shapes), and `SourceUnavailable { span }` (resolving a document whose store
was reclaimed first). Decode-failure positions advance on the token's start line, which is exact
for strict-rule tokens because RFC lexemes cannot contain raw line breaks.

## 5. Design choices and open decisions

- **Read-only model.** Editing and fidelity-preserving write-back stay at the CST/source level
  of the lower layers; the resolver is the "give me real values" endpoint.
- **Last-occurrence `get`** matches ECMA-262 `JSON.parse` and most implementations. Flagged as a
  maintainer-unratified decision; `get_all`/`duplicates` keep every alternative reachable.
- **Trivia reuse.** `Meta` carries the parser's `Trivia` values directly instead of mirroring
  them into a resolver-local type — they are already owned, `Copy`, and construction-validated.
- **`-0` and hex-overflow classification** (via the shared decode module) are likewise
  documented-but-unratified: `-0` stays `F64(-0.0)`; integers beyond 64 bits round to `f64`.

## 6. Quality bar

Same as the ecosystem: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings` under the strict lint set in `Cargo.toml`, and `cargo test` all clean. Lint exceptions
use `#[expect(lint, reason = "…")]` on the smallest existing scope.

## 7. Limitations and follow-up

- No serde glue: deserialization onto user structs is the planned `strata-json-serde` crate,
  which goes parser → structs directly rather than through this model.
- `JsonObject::get` is a linear scan; a lazily built index is possible future work if large
  objects show up in practice.
- The public API is not settled; this is an initial-development crate on an experimental branch.
- Package verification (`cargo package`) is blocked until the path dependencies are published.
