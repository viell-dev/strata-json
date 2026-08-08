# strata-json

JSON resolver for the Strata parser ecosystem: a read-only, dynamic document model with owned
values — decoded strings and converted numbers — that keeps source fidelity available (spans, raw
bytes, member order, duplicate members, and trivia), built on `strata-json-parser`'s CST.

## Status

This crate is in initial development. The public API is not settled yet.

## Usage

```rust
use strata_json::{JsonError, JsonNumber, JsonResolver};

fn main() -> Result<(), JsonError> {
    let document = JsonResolver::new().resolve_str(
        "{\n  \"name\": \"caf\\u00e9\",\n\n  \"count\": 3\n}",
    )?;

    let Some(object) = document.root().as_object() else {
        panic!("expected an object root");
    };

    // Owned, decoded values...
    assert_eq!(object.get("name").and_then(|v| v.as_str()), Some("café"));
    assert_eq!(
        object.get("count").and_then(|v| v.as_number()),
        Some(JsonNumber::U64(3))
    );

    // ...with source fidelity still attached: spans, raw bytes, blank lines.
    let [count] = &object.get_all("count").collect::<Vec<_>>()[..] else {
        panic!("expected one \"count\" member");
    };
    assert_eq!(
        document.bytes(count.name_meta().span()),
        Some(&b"\"count\""[..])
    );
    let [gap] = count.name_meta().leading() else {
        panic!("expected the whitespace before \"count\"");
    };
    assert_eq!(gap.blank_lines(), 1);

    Ok(())
}
```

See [docs/strata-json.md](docs/strata-json.md) for the design reference, including the
duplicate-member and lone-surrogate policies.

## Minimum Supported Rust Version

The MSRV is **1.87.0**.

## License

Licensed under the Mozilla Public License, Version 2.0 (see [LICENSE](LICENSE)
or <https://www.mozilla.org/en-US/MPL/2.0/>).

MPL-2.0 is a file-level weak copyleft license: its obligations attach to this crate's files (in
source and compiled form) and to modifications of them, not to your own source files that merely
use this crate. Distributing a binary that includes this crate obliges you to tell recipients
that the crate is included and how to obtain its source; for an unmodified crate, pointing them
at the exact crates.io release is a reasonable way to do that. Example material (documentation
code examples, `examples/`, integration tests, and benchmarks) is additionally licensed under
[MIT-0](LICENSE-MIT-0), so it can be copied freely. See
[LICENSE-CLARIFICATION.md](LICENSE-CLARIFICATION.md) for the maintainers' full statement of
intent regarding normal Rust usage (linking, monomorphization, macro expansion), binary
distribution, and generated code, and [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution
policy.
