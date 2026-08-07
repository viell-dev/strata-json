# strata-json

`strata-json` is a strict RFC 8259 JSON parser for the Strata parser ecosystem. It turns a byte
stream into a lossless, zero-copy concrete syntax tree (CST): tokens keep absolute byte spans,
trivia (whitespace runs, blank lines, a tolerated byte-order mark) is retained instead of
discarded, and string/number content stays as raw source bytes. Decoding — escape resolution,
numeric conversion — belongs to a downstream resolver or deserializer.

The crate sits at the parser stage of the ecosystem pipeline:

```text
reader -> lexer -> parser (this crate) -> resolver -> deserializer
```

- **Edition / MSRV:** Rust 2024, MSRV `1.87.0`.
- **License:** MPL-2.0.
- **Dependencies:** `strata-lexer` only (which brings `strata-reader` and `strata-lease`).

## 1. Module and public-API map

| Module  | Public items |
|---------|--------------|
| `rules` | `JsonToken`, `WhitespaceRule`, `PunctRule`, `LiteralRule`, `StringRule`, `NumberRule`, `BomRule`, `StrictRuleSet`, `strict_rule_set()`, `strict_scanner()`, `SharedRuleSet<M>` |
| `cst`   | `Document`, `Value`, `ArrayNode`, `Element`, `ObjectNode`, `Member`, `SyntaxToken`, `Trivia`, `TriviaKind` |
| `parse` | `JsonParser<M>`, `JsonParserBuilder<M>`, `JsonError`, `JsonErrorKind`, `DEFAULT_MAX_DEPTH` |

Everything is re-exported at the crate root.

## 2. Token rules (`rules`)

`JsonToken` is a payload-free, `Copy` kind enum: the six structural characters, `True`/`False`/
`Null`, `String`, `Number`, `Whitespace`, `Bom`, and `Eof`. `JsonToken::is_trivia()` classifies
`Whitespace`/`Bom`; `describe()` yields diagnostic names.

Each rule is a zero-sized `strata_lexer::Matcher` recognizing one RFC 8259 token shape from
read-only lookahead (measure-then-advance; a declined match cannot move the cursor):

- `WhitespaceRule` — a run of exactly space/tab/CR/LF (§2).
- `PunctRule` — `{` `}` `[` `]` `:` `,` (§2).
- `LiteralRule` — exactly `true`, `false`, `null` (§3).
- `StringRule` — a complete string including both quotes (§7). Validates *lexical shape* at the
  offending byte: unescaped control characters, escapes outside `\" \\ \/ \b \f \n \r \t \uXXXX`,
  `\u` without four hex digits, and invalid UTF-8 are lexical errors
  (`LexErrorKind::Unrecognized` with a precise position; unclosed strings are
  `UnterminatedDelimiter` at the opening quote). Lone surrogate escapes are accepted (§8.2
  discourages, does not forbid); surrogate-pair combination is resolver work.
- `NumberRule` — the exact §6 grammar `[ "-" ] int [ frac ] [ exp ]`. Leading zeros and empty
  fraction/exponent parts are lexical errors; a bare `-`/`+`/`.` matches nothing.
- `BomRule` — the UTF-8 encoding of U+FEFF at byte offset 0 only (§8.1). The rule recognizes the
  shape; the *parser* owns the policy (reject strictly, or retain as trivia under `allow_bom`).
  This buys a precise "leading byte-order mark" diagnostic instead of a generic unrecognized
  byte.

`StrictRuleSet` is the monomorphic tuple of all six rules; `strict_rule_set()` constructs it and
`strict_scanner()` wraps it in a `SpecScanner` with `JsonToken::Eof`. `SharedRuleSet<M>` is an
`Arc`-backed `MatcherSet` wrapper making non-`Clone` rule sets (e.g. `MatcherVec`) cheaply
cloneable for a reusable parser; it is kind-generic, so superset crates can use it too.

## 3. CST (`cst`)

The tree is lossless: concatenating every token's leading trivia and lexeme in source order
reproduces the input byte-for-byte (`tests/trivia_spans.rs` proves this).

- `SyntaxToken` — kind, `Span`, start/end `Position`, and `leading: Vec<Trivia>`: all trivia
  between the previous significant token and this one. Trailing trivia of the document (between
  the root value and EOF) lives in `Document::trailing_trivia()`.
- `Trivia` — kind (`Whitespace` or `Bom`), span, positions. `line_breaks()` and `blank_lines()`
  surface the implicit visual grouping (blank-line separation) the ecosystem preserves.
- `Value` — `Null(SyntaxToken)`, `Bool { value, token }`, `Number(SyntaxToken)`,
  `String(SyntaxToken)`, `Array(ArrayNode)`, `Object(ObjectNode)`. Scalars carry the token whose
  span addresses the raw bytes (string spans include both quotes; nothing is decoded). Helpers:
  `first_token()`, `last_token()`, `span()`, `for_each_token()`.
- `ArrayNode`/`ObjectNode` keep `open`/`close` tokens; `Element`/`Member` keep each entry's
  trailing `comma: Option<SyntaxToken>`, and `Member` keeps the `name` string token and `colon`.
- Duplicate member names are retained verbatim in source order. RFC 8259 §4 says names SHOULD be
  unique but does not require it; duplicate policy belongs downstream, so there is no option.
- `Document` owns the root value, trailing trivia, and the `SourceStore` holding the parked
  source bytes. Access: `bytes(span)` / `token_bytes(&token)` (zero-copy borrows),
  `source_ref(span)` (promotable handle that survives store reclamation — the resolver-facing
  copy point), `source()`/`source_mut()` (e.g. `retire_all`), `bom()`.

## 4. Parser (`parse`)

`JsonParser::new()` / `default()` is the fully strict path: strict rules, no BOM tolerance,
`DEFAULT_MAX_DEPTH` (128) nesting. `parse(impl Read)`, `parse_str`, and `parse_bytes` each run
one document to EOF; trailing non-whitespace content is an error. The parser clones its rule set
into a fresh `SpecScanner` per parse, so one parser serves many inputs. On success the consumed
reader buffer is parked into the document's `SourceStore` (compaction is forced off during the
parse so the bytes survive).

`JsonParser::builder()` exposes, per the ecosystem's builders-per-layer shape:

- `with_rules(set)` — replace the token rule set (any `MatcherSet<Kind = JsonToken> + Clone`;
  wrap non-`Clone` sets in `SharedRuleSet`). Mocking and adjusted rule sets plug in here.
- `with_lexer_config(config)` — replace the lexer/reader configuration (capacities, position
  tracking). `compact_after` is always overridden to `None` at parse time.
- `allow_bom(bool)` — off by default. RFC 8259 §8.1: parsers "MAY ignore the presence of a byte
  order mark rather than treating it as an error". When on, a leading BOM is retained as
  `TriviaKind::Bom` (visible via `Document::bom()`); when off it is `JsonErrorKind::UnexpectedBom`.
  A BOM anywhere else is always an error.
- `max_depth(usize)` — RFC 8259 §9 implementation limit on container nesting, default 128. Also
  bounds the parser's recursion.

Errors are `JsonError { position, kind }` with one-based line/column plus absolute byte.
`JsonErrorKind`: `Lex(LexError)` (unrecognized bytes and token-shape violations, at the exact
byte), `Expected { expected, found }`, `UnexpectedEof { expected }`, `TrailingContent { found }`,
`UnexpectedBom`, `DepthLimitExceeded { limit }`.

## 5. Composition points for supersets

A superset language (JSONC, JSON5, ...) does not wrap `JsonParser`; it defines its own kind enum
and parser and reuses the *rules layer*:

```rust
enum JsoncToken { Json(JsonToken), Comment, Eof }

let wrap: fn(JsonToken) -> JsoncToken = JsoncToken::Json;
let rules: MatcherVec<JsoncToken> = vec![
    LineCommentRule.boxed(),                  // the shape the superset adds
    WhitespaceRule.map_kind(wrap).boxed(),    // everything shared with JSON, lifted
    PunctRule.map_kind(wrap).boxed(),
    LiteralRule.map_kind(wrap).boxed(),
    StringRule.map_kind(wrap).boxed(),
    NumberRule.map_kind(wrap).boxed(),
];
let scanner = SpecScanner::new(rules, JsoncToken::Eof);
```

Rules to keep are lifted with `MatcherExt::map_kind`; rules to change are simply not included
(`BomRule` is omitted above); new rules are added alongside. Reused rules keep their validation —
`01` stays a lexical error inside the superset. `tests/composition.rs` is the worked example.
Spec-permitted extensions should follow this crate's pattern: parser-level options that include
the corresponding rule (or accept the corresponding token) when set, strict by default.

## 6. Design choices

- **Validation at the lexical boundary.** String/number shape violations are caught in the rules
  (per the lexer's format-boundary guidance), so a parsed CST is known-well-formed without any
  decoding pass. The cost: such failures surface as `LexErrorKind::Unrecognized` with an exact
  position rather than a named "bad escape" kind.
- **Structure/data separation.** No `String` payloads in tokens or nodes; spans plus the
  document-owned `SourceStore` keep the tree zero-copy until a caller resolves bytes.
- **Leading-trivia attachment.** Every trivia piece precedes exactly one significant token (or
  EOF), which makes reconstruction trivial and keeps trivia queries local.
- **BOM as a token, policy in the parser.** See `BomRule` above.
- **No extensions.** Comments, `NaN`/`Infinity`, single quotes, trailing commas belong to
  superset crates; RFC 8259 grants no latitude for them.

## 7. Quality bar

Same as the ecosystem: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings` under the strict lint set in `Cargo.toml`, and `cargo test` all clean. Lint exceptions
use `#[expect(lint, reason = "…")]` on the smallest existing scope.

## 8. Limitations and follow-up

- No resolver: string unescaping (including surrogate pairs) and number conversion are left to a
  future `strata-json` resolver layer or serde glue.
- Single-buffer retention: the parser parks one consumed buffer after EOF; streaming/chunked DOM
  construction awaits the lexer's batch protocol.
- The public API is not settled; this is an initial-development crate on an experimental branch.
