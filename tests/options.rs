//! Option passthrough to the parser and resolver-level policies.

use strata_json::{JsonError, JsonErrorKind, JsonNumber, JsonResolver, TriviaKind};

#[test]
fn strict_default_rejects_a_leading_bom() {
    let Err(error) = JsonResolver::new().resolve_str("\u{FEFF}1") else {
        panic!("expected a BOM error");
    };

    let JsonErrorKind::Parse(parse) = &error.kind else {
        panic!("expected a parse error, got {:?}", error.kind);
    };
    assert!(matches!(
        parse.kind,
        strata_json_parser::JsonErrorKind::UnexpectedBom
    ));
}

#[test]
fn allow_bom_retains_the_bom_as_trivia() -> Result<(), JsonError> {
    let resolver = JsonResolver::builder().allow_bom(true).build();
    let document = resolver.resolve_str("\u{FEFF}1")?;

    let [bom] = document.root().meta().leading() else {
        panic!("expected the BOM as leading trivia");
    };
    assert_eq!(bom.kind(), TriviaKind::Bom);
    assert_eq!(document.root().as_number(), Some(JsonNumber::U64(1)));

    Ok(())
}

#[test]
fn max_depth_passes_through() {
    let resolver = JsonResolver::builder().max_depth(1).build();

    assert!(resolver.resolve_str("[1]").is_ok());

    let Err(error) = resolver.resolve_str("[[1]]") else {
        panic!("expected a depth error");
    };
    let JsonErrorKind::Parse(parse) = &error.kind else {
        panic!("expected a parse error, got {:?}", error.kind);
    };
    assert!(matches!(
        parse.kind,
        strata_json_parser::JsonErrorKind::DepthLimitExceeded { limit: 1 }
    ));
}

#[test]
fn resolver_accessors_report_configuration() {
    let resolver = JsonResolver::builder()
        .duplicate_keys(strata_json::DuplicateKeys::Error)
        .lone_surrogates(strata_json::LoneSurrogates::Replace)
        .build();

    assert_eq!(resolver.duplicate_keys(), strata_json::DuplicateKeys::Error);
    assert_eq!(
        resolver.lone_surrogates(),
        strata_json::LoneSurrogates::Replace
    );
    assert!(!resolver.parser().allow_bom());
}

#[test]
fn resolve_document_accepts_an_external_parse() -> Result<(), JsonError> {
    let parser = strata_json_parser::JsonParser::builder()
        .allow_bom(true)
        .build();
    let Ok(parsed) = parser.parse_str("\u{FEFF}[true]") else {
        panic!("expected the parse to succeed");
    };

    let document = JsonResolver::new().resolve_document(parsed)?;
    let Some(array) = document.root().as_array() else {
        panic!("expected an array");
    };
    assert_eq!(array.len(), 1);

    Ok(())
}
