use std::collections::HashMap;

use serde_json::{json, Value};

use crate::models::node_graph::{BeatGrid, BlendMode, PatternArgDef, PatternArgType};
use crate::models::patterns::PatternSummary;
use crate::services::track_edits::{revision_for_clips, TrackClip, TrackDocument};

use super::convert::{
    build_registry_with_unavailable, clips_to_document, document_to_clips, parse_group_expression,
    CompileErrorCode,
};
use super::error::{format_error, DslErrorCode};
use super::parser::{parse, ParseOptions};
use super::serializer::{serialize, serialize_group_expression, SerializeError, SerializeOptions};
use super::tokenizer::{tokenize, TokenKind};
use super::types::{BarRange, GroupExpr, PatternArgument, PatternDefinition, TimeUnit};
use super::workflow::ScoreDslContext;
use super::*;

fn argument(id: &str, arg_type: PatternArgType) -> PatternArgument {
    PatternArgument {
        id: id.to_owned(),
        name: id.to_owned(),
        arg_type,
        default_value: Value::Null,
    }
}

fn registry() -> PatternRegistry {
    PatternRegistry::new(vec![
        PatternDefinition {
            id: Some("solid-id".to_owned()),
            name: "solid_color".to_owned(),
            args: vec![argument("color", PatternArgType::Color)],
        },
        PatternDefinition {
            id: Some("spikes-id".to_owned()),
            name: "intensity_spikes".to_owned(),
            args: vec![
                argument("subdivision", PatternArgType::Scalar),
                argument("color", PatternArgType::Color),
                argument("selection", PatternArgType::Selection),
            ],
        },
        PatternDefinition {
            id: Some("noise-id".to_owned()),
            name: "smooth_dimmer_noise".to_owned(),
            args: Vec::new(),
        },
    ])
}

fn beat_grid() -> BeatGrid {
    BeatGrid {
        beats: vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5],
        downbeats: vec![0.0, 2.0, 4.0],
        bpm: 120.0,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    }
}

fn parse_ok(source: &str, registry: &PatternRegistry) -> (Document, Vec<DslWarning>) {
    match parse(source, registry, ParseOptions::default()) {
        ParseResult::Success { document, warnings } => (document, warnings),
        ParseResult::Failure { errors, .. } => panic!("parse failed: {errors:#?}"),
    }
}

fn names_for(registry: &PatternRegistry) -> PatternNames {
    registry
        .definitions()
        .iter()
        .filter_map(|pattern| {
            pattern
                .id
                .as_ref()
                .map(|id| (id.clone(), pattern.name.clone()))
        })
        .collect()
}

fn canonical_source(body: &str) -> String {
    if body.is_empty() {
        "# luma-score-schema: 1".to_owned()
    } else {
        format!("# luma-score-schema: 1\n{body}")
    }
}

#[test]
fn tokenizer_ports_human_grammar_and_locations() {
    let tokens = tokenize(
        "\"clip\": solid_color[\"solid-id\"](left & ~high > all) @-0.25s-1e2s color=#AABBCC blend=add # note\r\n",
    );
    let kinds: Vec<TokenKind> = tokens.iter().map(|token| token.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TokenKind::String,
            TokenKind::Colon,
            TokenKind::Identifier,
            TokenKind::LeftBracket,
            TokenKind::String,
            TokenKind::RightBracket,
            TokenKind::LeftParen,
            TokenKind::Identifier,
            TokenKind::And,
            TokenKind::Not,
            TokenKind::Identifier,
            TokenKind::Fallback,
            TokenKind::Identifier,
            TokenKind::RightParen,
            TokenKind::At,
            TokenKind::Dash,
            TokenKind::Number,
            TokenKind::Identifier,
            TokenKind::Dash,
            TokenKind::Number,
            TokenKind::Identifier,
            TokenKind::Identifier,
            TokenKind::Equals,
            TokenKind::HexColor,
            TokenKind::Identifier,
            TokenKind::Equals,
            TokenKind::Identifier,
            TokenKind::Comment,
            TokenKind::Newline,
            TokenKind::Eof,
        ]
    );
    assert_eq!(tokens[23].value, "#AABBCC");
    assert_eq!(tokens.last().unwrap().span.start.line, 2);
}

#[test]
fn tokenizer_never_discards_unknown_source() {
    let tokens = tokenize("solid_color(all) @1;");
    assert!(tokens
        .iter()
        .any(|token| token.kind == TokenKind::Unknown && token.value == ";"));
}

#[test]
fn parser_ports_ranges_layers_args_and_precedence() {
    let source = concat!(
        "\"one\": solid_color[\"solid-id\"]((left | right) & ~high > all) @5:3-7:2 color=#ff0000\n",
        "\n",
        "layer -7:\n",
        "\"two\": intensity_spikes[\"spikes-id\"](group_local: hit) @0.3333333333333333s-1.23456789012345s subdivision=2 blend=subtract",
    );
    let (document, warnings) = parse_ok(source, &registry());
    assert!(warnings.is_empty());
    assert_eq!(document.layers.len(), 2);
    assert_eq!(document.layers[0].z_index, 0);
    assert_eq!(document.layers[0].annotations[0].range.start, 5.5);
    assert_eq!(document.layers[0].annotations[0].range.end, 7.25);
    let second = &document.layers[1].annotations[0];
    assert_eq!(document.layers[1].z_index, -7);
    assert_eq!(second.id.as_deref(), Some("two"));
    assert_eq!(second.pattern_id.as_deref(), Some("spikes-id"));
    assert_eq!(
        second.selection_spatial_reference.as_deref(),
        Some("group_local")
    );
    assert_eq!(second.range.unit, TimeUnit::Seconds);
    assert_eq!(
        second.range.start.to_bits(),
        0.3333333333333333_f64.to_bits()
    );
    assert_eq!(second.range.end.to_bits(), 1.23456789012345_f64.to_bits());
    assert_eq!(second.blend, BlendMode::Subtract);
}

#[test]
fn parser_distinguishes_absent_and_explicit_selection() {
    let (document, _) = parse_ok(
        "\"a\": intensity_spikes[\"spikes-id\"]() @1\n\"b\": intensity_spikes[\"spikes-id\"](all) @2",
        &registry(),
    );
    assert_eq!(document.layers[0].annotations[0].selection, None);
    assert_eq!(
        document.layers[0].annotations[1].selection,
        Some(GroupExpr::Group {
            name: "all".to_owned()
        })
    );
}

#[test]
fn human_import_accepts_optional_ids_and_quoted_names_and_keys() {
    let registry = PatternRegistry::new(vec![PatternDefinition {
        id: Some("numeric-pattern".to_owned()),
        name: "250-500hz bass pulse".to_owned(),
        args: vec![PatternArgument {
            id: "gain-db".to_owned(),
            name: "Gain dB".to_owned(),
            arg_type: PatternArgType::Scalar,
            default_value: json!(0),
        }],
    }]);
    let (document, warnings) = parse_ok(
        "\"250-500hz bass pulse\"(all) @1 \"gain-db\"=0.5 \"blend\"=\"argument\"",
        &registry,
    );
    assert!(warnings.iter().any(|warning| warning.code == "unknown_arg"));
    let annotation = &document.layers[0].annotations[0];
    assert!(annotation.id.is_none());
    assert!(annotation.pattern_id.is_none());
    assert_eq!(annotation.pattern, "250-500hz bass pulse");
    assert_eq!(annotation.args[0].key, "gain-db");
    assert_eq!(annotation.args[1].key, "blend");
}

#[test]
fn human_import_resolves_a_stale_presentation_name_once() {
    let source = "\"clip\": old_name[\"solid-id\"](all) @1 color=#ff0000";
    let (document, warnings) = parse_ok(source, &registry());
    assert_eq!(warnings[0].code, "stale_pattern_name");
    assert_eq!(document.layers[0].annotations[0].pattern, "old_name");

    let imported = compile_import_track_document(
        source,
        &ScoreDslContext {
            beat_grid: beat_grid(),
            registry: registry(),
        },
        false,
    )
    .unwrap();
    assert!(imported
        .canonical_source
        .contains("solid_color[\"solid-id\"]()"));
    assert!(!imported.canonical_source.contains("old_name"));
    assert!(!imported.canonical_source.contains("(all)"));
    assert!(imported.canonical_source.contains("color={"));
}

#[test]
fn parser_warns_without_dropping_unknown_and_selection_args() {
    let (document, warnings) = parse_ok(
        "\"a\": intensity_spikes[\"spikes-id\"]() @1 selection={\"legacy\":true} orphan={\"nested\":[true,false,null]}",
        &registry(),
    );
    assert_eq!(document.layers[0].annotations[0].args.len(), 2);
    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings[0].code, "selection_as_arg");
    assert_eq!(warnings[1].code, "unknown_arg");
}

#[test]
fn parser_reports_unknown_patterns_types_blends_and_syntax() {
    for (source, expected) in [
        ("missing(all) @1", DslErrorCode::UnknownPattern),
        (
            "solid_color[\"solid-id\"](all) @1 color=42",
            DslErrorCode::TypeMismatch,
        ),
        (
            "solid_color[\"solid-id\"](all) @1 blend=bogus",
            DslErrorCode::InvalidBlendMode,
        ),
        (
            "solid_color[\"solid-id\"](all) @8-3",
            DslErrorCode::InvalidBarRange,
        ),
        (
            "solid_color[\"solid-id\"](all) @1;",
            DslErrorCode::UnexpectedToken,
        ),
    ] {
        let ParseResult::Failure { errors, .. } =
            parse(source, &registry(), ParseOptions::default())
        else {
            panic!("expected parse failure for {source}");
        };
        assert_eq!(errors[0].code, expected, "source: {source}");
    }
}

#[test]
fn parser_accumulates_errors_and_formats_spans() {
    let source = "missing(all) @1\n\nmissing_too(all) @2";
    let ParseResult::Failure { errors, partial } =
        parse(source, &registry(), ParseOptions::default())
    else {
        panic!("expected failure");
    };
    assert_eq!(errors.len(), 2);
    assert!(partial.layers.is_empty());
    let formatted = format_error(&errors[0], source);
    assert!(formatted.contains("line 1"));
    assert!(formatted.contains("^^^^^^^"));
    assert!(formatted.contains("Available patterns:"));
}

#[test]
fn comments_stay_attached_through_source_serialization() {
    let source = concat!(
        "# layer note\n",
        "layer 3: # layer tail\n",
        "# clip note\n",
        "\"clip\": solid_color[\"solid-id\"](all) @1 color=#ff0000 # clip tail\n",
        "# document tail",
    );
    let (document, _) = parse_ok(source, &registry());
    assert_eq!(
        document.layers[0].trivia.leading_comments[0].text,
        "layer note"
    );
    assert_eq!(
        document.layers[0].annotations[0].trivia.leading_comments[0].text,
        "clip note"
    );
    assert_eq!(document.trailing_comments[0].text, "document tail");
    assert_eq!(
        serialize(&document, &registry(), SerializeOptions::default()).unwrap(),
        source
    );
}

#[test]
fn canonical_serializer_emits_ids_explicit_z_and_stable_order() {
    let source = track_document_to_canonical_dsl(
        &TrackDocument {
            revision: String::new(),
            clips: vec![
                TrackClip {
                    id: "b".to_owned(),
                    pattern_id: "spikes-id".to_owned(),
                    start_time: 2.0,
                    end_time: 3.0,
                    z_index: 0,
                    blend_mode: BlendMode::Replace,
                    args: json!({"orphan": 2, "color": "#ffffff", "subdivision": 1}),
                },
                TrackClip {
                    id: "a".to_owned(),
                    pattern_id: "solid-id".to_owned(),
                    start_time: 1.0,
                    end_time: 2.0,
                    z_index: 1,
                    blend_mode: BlendMode::Replace,
                    args: json!({"blend": "arg", "color": "#000000"}),
                },
            ],
        },
        &names_for(&registry()),
    )
    .unwrap();
    assert_eq!(
        source,
        concat!(
            "# luma-score-schema: 1\n",
            "layer 0:\n",
            "\"b\": intensity_spikes[\"spikes-id\"]() @2s-3s color=\"#ffffff\" orphan=2 subdivision=1\n",
            "\n",
            "layer 1:\n",
            "\"a\": solid_color[\"solid-id\"]() @1s-2s \"blend\"=\"arg\" color=\"#000000\"",
        )
    );
}

#[test]
fn canonical_serializer_rejects_missing_ids() {
    let (mut document, _) = parse_ok("solid_color[\"solid-id\"](all) @1", &registry());
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::MissingClipId {
            pattern: "solid_color".to_owned()
        }
    );
    document.layers[0].annotations[0].id = Some("clip".to_owned());
    document.layers[0].annotations[0].pattern_id = None;
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::MissingPatternId {
            clip_id: "clip".to_owned()
        }
    );
}

#[test]
fn canonical_serializer_cannot_emit_contextual_sugar() {
    let (mut document, _) = parse_ok(
        "\"clip\": solid_color[\"solid-id\"](all) @1 color=#ff0000",
        &registry(),
    );
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::CanonicalBarTime {
            clip_id: "clip".to_owned(),
        }
    );

    let annotation = &mut document.layers[0].annotations[0];
    annotation.range = BarRange {
        start: 0.0,
        end: 1.0,
        unit: TimeUnit::Seconds,
    };
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::CanonicalSelectionSugar {
            clip_id: "clip".to_owned(),
        }
    );

    let annotation = &mut document.layers[0].annotations[0];
    annotation.selection = None;
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::CanonicalArgumentSugar {
            clip_id: "clip".to_owned(),
            key: "color".to_owned(),
        }
    );
}

#[test]
fn canonical_encoder_rejects_duplicate_clip_and_argument_identities() {
    let clip = TrackClip {
        id: "clip".to_owned(),
        pattern_id: "solid-id".to_owned(),
        start_time: 0.0,
        end_time: 1.0,
        z_index: 0,
        blend_mode: BlendMode::Replace,
        args: json!({"gain": 1}),
    };
    assert_eq!(
        clips_to_canonical_document(&[clip.clone(), clip.clone()], &names_for(&registry()))
            .unwrap_err()
            .code,
        CompileErrorCode::DuplicateClipId
    );

    let mut document =
        clips_to_canonical_document(std::slice::from_ref(&clip), &names_for(&registry())).unwrap();
    let duplicate_clip = document.layers[0].annotations[0].clone();
    document.layers[0].annotations.push(duplicate_clip);
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::DuplicateClipId {
            clip_id: "clip".to_owned(),
        }
    );

    document.layers[0].annotations.pop();
    let duplicate = document.layers[0].annotations[0].args[0].clone();
    document.layers[0].annotations[0].args.push(duplicate);
    assert_eq!(
        serialize_canonical(&document).unwrap_err(),
        SerializeError::DuplicateArgument {
            clip_id: "clip".to_owned(),
            key: "gain".to_owned(),
        }
    );
}

#[test]
fn canonical_decoder_rejects_every_contextual_or_ambiguous_form() {
    for (source, expected) in [
        (
            "layer 0:\n\"clip\": solid[\"pattern\"]() @1-2",
            CompileErrorCode::NonCanonicalTime,
        ),
        (
            "layer 0:\n\"clip\": solid[\"pattern\"](all) @0s-1s",
            CompileErrorCode::NonCanonicalSelection,
        ),
        (
            "layer 0:\nsolid[\"pattern\"]() @0s-1s",
            CompileErrorCode::MissingClipId,
        ),
        (
            concat!(
                "layer 0:\n",
                "\"clip\": solid[\"pattern\"]() @0s-1s\n",
                "\"clip\": solid[\"pattern\"]() @1s-2s",
            ),
            CompileErrorCode::DuplicateClipId,
        ),
        (
            "layer 0:\n\"clip\": solid[\"pattern\"]() @0s-1s gain=1 gain=2",
            CompileErrorCode::DuplicateArg,
        ),
        (
            "\"clip\": solid[\"pattern\"]() @0s-1s",
            CompileErrorCode::NonCanonicalLayer,
        ),
    ] {
        let canonical = canonical_source(source);
        let Err(SourceCompileError::Semantic(error)) = decode_canonical_track_document(&canonical)
        else {
            panic!("expected canonical semantic failure for {source:?}");
        };
        assert_eq!(error.code, expected, "source: {source}");
    }

    for source in [
        "layer 0:\n\"clip\": solid() @0s-1s",
        "layer 0:\n\"clip\": solid[\"pattern\"]() @0s-1s color=#ff0000",
        "layer 0:\n\"clip\": solid[\"pattern\"]() @0s-1s mode=rainbow",
    ] {
        assert!(
            matches!(
                decode_canonical_track_document(&canonical_source(source)),
                Err(SourceCompileError::Syntax(_))
            ),
            "source unexpectedly decoded: {source}"
        );
    }
}

#[test]
fn canonical_decoder_preserves_exact_seconds_raw_json_and_large_integers_without_context() {
    let source = canonical_source(concat!(
        "layer -9:\n",
        "\"clip\": old_pattern_name[\"deleted-pattern\"]() ",
        "@0.3333333333333333s-1.23456789012345s ",
        "huge=9007199254740993 ",
        "payload={\"a\":[true,null,\"#ff0000\"],\"z\":{\"nested\":0.30000000000000004}} ",
        "selection={\"expression\":\"left & wash\",\"spatialReference\":\"global\"} ",
        "blend=subtract",
    ));

    let (document, warnings) = decode_canonical_track_document(&source).unwrap();
    assert!(warnings.is_empty());
    let clip = &document.clips[0];
    assert_eq!(clip.pattern_id, "deleted-pattern");
    assert_eq!(clip.start_time.to_bits(), 0.3333333333333333_f64.to_bits());
    assert_eq!(clip.end_time.to_bits(), 1.23456789012345_f64.to_bits());
    assert_eq!(clip.z_index, -9);
    assert_eq!(clip.blend_mode, BlendMode::Subtract);
    assert_eq!(clip.args["huge"], json!(9_007_199_254_740_993_u64));
    assert_eq!(
        clip.args["selection"],
        json!({"expression": "left & wash", "spatialReference": "global"})
    );
    assert_eq!(
        track_document_to_canonical_dsl(
            &document,
            &PatternNames::from([("deleted-pattern".to_owned(), "old_pattern_name".to_owned(),)]),
        )
        .unwrap(),
        source
    );
}

#[test]
fn canonical_score_envelope_is_required_versioned_and_not_user_trivia() {
    for source in [
        "",
        "layer 0:",
        "# luma-score-schema: 0",
        "# luma-score-schema: 2",
        "# luma-score-schema: 01",
        "# luma-score-schema: one",
        "# luma-score-schema: 1 ",
    ] {
        let Err(SourceCompileError::Syntax(errors)) = decode_canonical_track_document(source)
        else {
            panic!("expected schema failure for {source:?}");
        };
        assert_eq!(errors[0].code, DslErrorCode::InvalidCanonicalSchema);
    }

    let source = serialize_canonical(&Document::default()).unwrap();
    assert_eq!(source, "# luma-score-schema: 1");
    let ParseResult::Success { document, warnings } = parse_canonical(&source) else {
        panic!("current empty canonical score did not parse");
    };
    assert!(document.layers.is_empty());
    assert!(document.trailing_comments.is_empty());
    assert!(warnings.is_empty());
    assert_eq!(serialize_canonical(&document).unwrap(), source);
    let (track, warnings) = decode_canonical_track_document(&source).unwrap();
    assert!(track.clips.is_empty());
    assert!(warnings.is_empty());

    let body = "layer 0:\n\"clip\": solid_color[\"solid-id\"]() @0s-1s color={\"hex\":\"#ff0000\"}";
    let checked_out = canonical_source(body);
    let installed = registry();
    let ParseResult::Success { document, .. } =
        parse(&checked_out, &installed, ParseOptions::default())
    else {
        panic!("a canonical checkout was not accepted as editable draft source");
    };
    assert_eq!(serialize_canonical(&document).unwrap(), checked_out);

    for malformed in [
        "# luma-score-schema",
        "# luma-score-schema: 2\nlayer 0:",
        "# luma-score-schema is merely a comment",
    ] {
        let ParseResult::Failure { errors, .. } =
            parse(malformed, &registry(), ParseOptions::default())
        else {
            panic!("reserved malformed envelope was accepted: {malformed:?}");
        };
        assert_eq!(errors[0].code, DslErrorCode::InvalidCanonicalSchema);
    }
}

#[test]
fn group_expression_roundtrip_preserves_precedence() {
    for source in [
        "left & wash",
        "hit | left & wash",
        "(left | right) & ~high > all",
        "left ^ right",
    ] {
        let expression = parse_group_expression(source).unwrap();
        assert_eq!(serialize_group_expression(&expression), source);
    }
}

#[test]
fn every_blend_mode_roundtrips() {
    for name in [
        "replace", "add", "multiply", "screen", "max", "min", "lighten", "value", "subtract",
    ] {
        let mode = super::parser::blend_mode_from_name(name).unwrap();
        let canonical = track_document_to_canonical_dsl(
            &TrackDocument {
                revision: String::new(),
                clips: vec![TrackClip {
                    id: "clip".to_owned(),
                    pattern_id: "solid-id".to_owned(),
                    start_time: 0.0,
                    end_time: 1.0,
                    z_index: 0,
                    blend_mode: mode,
                    args: json!({}),
                }],
            },
            &names_for(&registry()),
        )
        .unwrap();
        if name == "replace" {
            assert!(!canonical.contains("blend="));
        } else {
            assert!(canonical.ends_with(&format!("blend={name}")));
        }
    }
}

#[test]
fn lossless_track_roundtrip_preserves_every_authored_field() {
    let registry = PatternRegistry::new(vec![PatternDefinition {
        id: Some("pattern-a".to_owned()),
        name: "all_values".to_owned(),
        args: vec![
            argument("selection", PatternArgType::Selection),
            argument("selection_2", PatternArgType::Selection),
            argument("amount", PatternArgType::Scalar),
            argument("color", PatternArgType::Color),
            argument("palette", PatternArgType::Palette),
            argument("gradient", PatternArgType::Gradient),
        ],
    }]);
    let clip = TrackClip {
        id: "67b6b29f-6863-4889-91d7-058b590d91e4".to_owned(),
        pattern_id: "pattern-a".to_owned(),
        start_time: 0.3333333333333333,
        end_time: 1.23456789012345,
        z_index: -7,
        blend_mode: BlendMode::Subtract,
        args: json!({
            "selection": { "expression": "left & wash", "spatialReference": "group_local" },
            "selection_2": { "expression": "right", "spatialReference": "global" },
            "amount": 0.30000000000000004,
            "color": { "r": 12.5, "g": 34, "b": 56, "a": 0.5 },
            "palette": { "colors": ["#ff0080", "#00ffc8"] },
            "gradient": { "stops": [
                { "color": "#000000", "t": 0 },
                { "color": "#ffffff80", "t": 0.3333333333333333 }
            ] },
            "orphaned_arg": { "nested": [true, false, null, "unchanged"] },
            "blend": "this is an arg, not the clip blend mode"
        }),
    };
    let document =
        clips_to_canonical_document(std::slice::from_ref(&clip), &names_for(&registry)).unwrap();
    let source = serialize_canonical(&document).unwrap();
    assert!(source.contains("layer -7:"));
    assert!(source.contains("@0.3333333333333333s-1.23456789012345s"));
    assert!(source.contains("()"));
    assert!(source.contains(
        "selection={\"expression\":\"left & wash\",\"spatialReference\":\"group_local\"}"
    ));
    assert!(source.contains("\"blend\"="));
    assert!(source.contains("blend=subtract"));

    let (compiled, _) = decode_canonical_track_document(&source).unwrap();
    assert_eq!(compiled.clips, vec![clip]);
    assert_eq!(
        track_document_to_canonical_dsl(&compiled, &names_for(&registry)).unwrap(),
        source
    );
}

#[test]
fn absent_selection_and_legacy_selection_json_are_lossless() {
    let registry = PatternRegistry::new(vec![PatternDefinition {
        id: Some("pattern-a".to_owned()),
        name: "all_values".to_owned(),
        args: vec![argument("selection", PatternArgType::Selection)],
    }]);
    let clips = vec![
        TrackClip {
            id: "absent".to_owned(),
            pattern_id: "pattern-a".to_owned(),
            start_time: 0.0,
            end_time: 2.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            args: json!({}),
        },
        TrackClip {
            id: "legacy".to_owned(),
            pattern_id: "pattern-a".to_owned(),
            start_time: 2.0,
            end_time: 4.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            args: json!({
                "selection": {
                    "expression": "front_led_bars side_pars ",
                    "spatialReference": "global"
                }
            }),
        },
    ];
    let document = clips_to_document(&clips, &beat_grid(), &registry).unwrap();
    assert!(document.layers[0].annotations[0].selection.is_none());
    assert!(document.layers[0].annotations[1].selection.is_none());
    assert_eq!(
        document_to_clips(&document, &beat_grid(), &registry).unwrap(),
        clips
    );
}

#[test]
fn exact_seconds_compile_without_grid_but_bars_do_not() {
    let no_grid = BeatGrid {
        beats: Vec::new(),
        downbeats: Vec::new(),
        bpm: 120.0,
        downbeat_offset: 0.0,
        beats_per_bar: 4,
    };
    let (seconds, _) = parse_ok(
        "\"clip\": solid_color[\"solid-id\"](all) @0.125s-1.75s",
        &registry(),
    );
    let clips = document_to_clips(&seconds, &no_grid, &registry()).unwrap();
    assert_eq!(clips[0].start_time.to_bits(), 0.125_f64.to_bits());
    assert_eq!(clips[0].end_time.to_bits(), 1.75_f64.to_bits());

    let (bars, _) = parse_ok("\"clip\": solid_color[\"solid-id\"](all) @1-2", &registry());
    assert_eq!(
        document_to_clips(&bars, &no_grid, &registry())
            .unwrap_err()
            .code,
        CompileErrorCode::MissingBeatGrid
    );
}

#[test]
fn three_four_subdivisions_preserve_exact_compiled_seconds() {
    let grid = BeatGrid {
        beats: vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5],
        downbeats: vec![0.0, 1.5, 3.0],
        bpm: 120.0,
        downbeat_offset: 0.0,
        beats_per_bar: 3,
    };
    let source = "\"clip\": solid_color[\"solid-id\"](all) @1:2:2-2:3:4";
    let (document, _) = match parse(
        source,
        &registry(),
        ParseOptions {
            beats_per_bar: 3,
            subdivisions_per_beat: 4,
        },
    ) {
        ParseResult::Success { document, warnings } => (document, warnings),
        ParseResult::Failure { errors, .. } => panic!("parse failed: {errors:#?}"),
    };
    let clips = document_to_clips(&document, &grid, &registry()).unwrap();
    let expected_start = clips[0].start_time;
    let expected_end = clips[0].end_time;
    let canonical_document = clips_to_canonical_document(&clips, &names_for(&registry())).unwrap();
    let canonical = serialize_canonical(&canonical_document).unwrap();
    let (compiled, _) = decode_canonical_track_document(&canonical).unwrap();
    assert_eq!(
        compiled.clips[0].start_time.to_bits(),
        expected_start.to_bits()
    );
    assert_eq!(compiled.clips[0].end_time.to_bits(), expected_end.to_bits());
}

#[test]
fn pattern_and_argument_identity_disambiguate_names() {
    let registry = PatternRegistry::new(vec![
        PatternDefinition {
            id: Some("pattern-a".to_owned()),
            name: "duplicate".to_owned(),
            args: vec![
                PatternArgument {
                    id: "left_gain".to_owned(),
                    name: "gain".to_owned(),
                    arg_type: PatternArgType::Scalar,
                    default_value: Value::Null,
                },
                PatternArgument {
                    id: "right_gain".to_owned(),
                    name: "gain".to_owned(),
                    arg_type: PatternArgType::Scalar,
                    default_value: Value::Null,
                },
            ],
        },
        PatternDefinition {
            id: Some("pattern-b".to_owned()),
            name: "duplicate".to_owned(),
            args: Vec::new(),
        },
    ]);
    let ParseResult::Failure { errors, .. } =
        parse("duplicate(all) @1", &registry, ParseOptions::default())
    else {
        panic!("ambiguous name should fail");
    };
    assert_eq!(errors[0].code, DslErrorCode::UnknownPattern);

    let (document, _) = parse_ok(
        "\"clip\": duplicate[\"pattern-a\"]() @0s-1s gain=0.5",
        &registry,
    );
    assert_eq!(
        document_to_clips(&document, &beat_grid(), &registry)
            .unwrap_err()
            .code,
        CompileErrorCode::AmbiguousArg
    );
}

#[test]
fn track_document_revision_is_not_in_authored_source() {
    let track = TrackDocument {
        revision: "volatile-cas-token".to_owned(),
        clips: vec![TrackClip {
            id: "clip".to_owned(),
            pattern_id: "solid-id".to_owned(),
            start_time: 0.0,
            end_time: 2.0,
            z_index: 0,
            blend_mode: BlendMode::Replace,
            args: json!({ "color": { "r": 255, "g": 0, "b": 0, "a": 1 } }),
        }],
    };
    let source = track_document_to_canonical_dsl(&track, &names_for(&registry())).unwrap();
    assert!(!source.contains("volatile-cas-token"));
    let (compiled, _) = decode_canonical_track_document(&source).unwrap();
    assert_eq!(compiled.revision, revision_for_clips(&track.clips));
    assert_eq!(compiled.clips, track.clips);
}

#[test]
fn canonical_score_roundtrip_ignores_json_number_spelling() {
    let original = TrackClip {
        id: "clip".to_owned(),
        pattern_id: "spikes-id".to_owned(),
        start_time: f64::from(0.24_f32),
        end_time: f64::from(2.2_f32),
        z_index: 0,
        blend_mode: BlendMode::Replace,
        args: json!({
            "subdivision": serde_json::Number::from_f64(1.0).unwrap(),
            "selection": {"expression": "all", "spatialReference": "global"}
        }),
    };
    let source = track_document_to_canonical_dsl(
        &TrackDocument {
            revision: String::new(),
            clips: vec![original.clone()],
        },
        &names_for(&registry()),
    )
    .unwrap();
    let (compiled, _) = decode_canonical_track_document(&source).unwrap();

    assert!(
        source.contains("@0.23999999463558197s-2.200000047683716s"),
        "{source}"
    );
    assert!(source.contains("subdivision=1"));
    assert!(crate::canonical_json::equivalent(
        &compiled.clips[0].args,
        &original.args
    ));
    assert_eq!(
        revision_for_clips(&compiled.clips),
        revision_for_clips(&[original])
    );
}

#[test]
fn human_compile_allocates_only_missing_draft_identities() {
    let source = concat!(
        "solid_color[\"solid-id\"](all) @0s-1s\n",
        "\"existing-id\": solid_color[\"solid-id\"](all) @1s-2s\n",
        "\"new:dsl-0\": solid_color[\"solid-id\"](all) @2s-3s\n",
        "solid_color[\"solid-id\"](all) @3s-4s",
    );
    let (track, _) =
        compile_draft_track_document(source, "revision".to_owned(), &beat_grid(), &registry())
            .unwrap();

    assert_eq!(
        track
            .clips
            .iter()
            .map(|clip| clip.id.as_str())
            .collect::<Vec<_>>(),
        vec!["new:dsl-1", "existing-id", "new:dsl-0", "new:dsl-2"]
    );
}

#[test]
fn exemplar_serialization_omits_only_clip_ids() {
    let track = TrackDocument {
        revision: "revision".to_owned(),
        clips: vec![TrackClip {
            id: "67b6b29f-6863-4889-91d7-058b590d91e4".to_owned(),
            pattern_id: "solid-id".to_owned(),
            start_time: 0.3333333333333333,
            end_time: 1.23456789012345,
            z_index: -3,
            blend_mode: BlendMode::Subtract,
            args: json!({"color": {"r": 12.5, "g": 34, "b": 56, "a": 0.5}}),
        }],
    };

    let exemplar = track_document_to_exemplar_dsl(&track, &beat_grid(), &registry()).unwrap();
    assert!(!exemplar.contains(&track.clips[0].id));
    assert_eq!(
        exemplar,
        concat!(
            "layer -3:\n",
            "solid_color[\"solid-id\"](all) @0.3333333333333333s-1.23456789012345s ",
            "color={\"a\":0.5,\"b\":56,\"g\":34,\"r\":12.5} blend=subtract"
        )
    );
    let (compiled, _) =
        compile_draft_track_document(&exemplar, "revision".to_owned(), &beat_grid(), &registry())
            .unwrap();
    assert_eq!(compiled.clips[0].pattern_id, track.clips[0].pattern_id);
    assert_eq!(
        compiled.clips[0].start_time.to_bits(),
        track.clips[0].start_time.to_bits()
    );
    assert_eq!(compiled.clips[0].args, track.clips[0].args);
}

#[test]
fn import_compile_allocates_missing_ids_and_emits_the_same_canonical_ast() {
    let context = ScoreDslContext {
        beat_grid: beat_grid(),
        registry: registry(),
    };
    let source = concat!(
        "# authored note\n",
        "solid_color[\"solid-id\"](all) @0s-1s color=#ff0000\n",
        "\"67b6b29f-6863-4889-91d7-058b590d91e4\": solid_color[\"solid-id\"](all) @1s-2s orphan=true",
    );
    let imported = compile_import_track_document(source, &context, true).unwrap();
    let track = &imported.document;

    assert_eq!(track.clips.len(), 2);
    assert!(uuid::Uuid::parse_str(&track.clips[0].id).is_ok());
    assert_eq!(track.clips[1].id, "67b6b29f-6863-4889-91d7-058b590d91e4");
    assert_eq!(
        imported.host_allocated_ids,
        std::collections::BTreeSet::from([track.clips[0].id.clone()])
    );
    assert!(!imported
        .host_allocated_ids
        .contains("67b6b29f-6863-4889-91d7-058b590d91e4"));
    assert!(imported
        .canonical_source
        .starts_with("# luma-score-schema: 1\nlayer 0:\n# authored note\n"));
    assert!(imported.canonical_source.contains(&track.clips[0].id));
    assert!(imported
        .warnings
        .iter()
        .any(|warning| warning.code == "unknown_arg"));

    let (strict, _) = decode_canonical_track_document(&imported.canonical_source).unwrap();
    assert_eq!(strict.clips, track.clips);
}

#[test]
fn cyberdrum_fixture_roundtrips_all_109_clips_and_is_string_stable() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/cyberdrum.json")).unwrap();
    let patterns: Vec<PatternSummary> = fixture["patterns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| PatternSummary {
            id: value["id"].to_string(),
            uid: None,
            name: value["name"].as_str().unwrap().to_owned(),
            description: None,
            category_name: None,
            created_at: String::new(),
            updated_at: String::new(),
            is_verified: false,
            author_name: None,
            forked_from_id: None,
        })
        .collect();
    let pattern_args: HashMap<String, Vec<PatternArgDef>> = fixture["patternArgs"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, value)| {
            (
                id.clone(),
                serde_json::from_value::<Vec<PatternArgDef>>(value.clone()).unwrap(),
            )
        })
        .collect();
    let clips: Vec<TrackClip> = fixture["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| TrackClip {
            id: value["id"].to_string(),
            pattern_id: value["patternId"].to_string(),
            start_time: value["startTime"].as_f64().unwrap(),
            end_time: value["endTime"].as_f64().unwrap(),
            z_index: value["zIndex"].as_i64().unwrap(),
            blend_mode: serde_json::from_value(value["blendMode"].clone()).unwrap(),
            args: value["args"].clone(),
        })
        .collect();
    assert_eq!(clips.len(), 109);
    assert_eq!(patterns.len(), 10);
    let registry = build_registry_with_unavailable(&patterns, &pattern_args, HashMap::new());
    let document = clips_to_canonical_document(&clips, &names_for(&registry)).unwrap();
    let source = serialize_canonical(&document).unwrap();
    let (compiled, _warnings) = decode_canonical_track_document(&source).unwrap();
    let expected: HashMap<&str, &TrackClip> =
        clips.iter().map(|clip| (clip.id.as_str(), clip)).collect();
    assert_eq!(compiled.clips.len(), expected.len());
    for clip in &compiled.clips {
        assert_eq!(Some(clip), expected.get(clip.id.as_str()).copied());
    }
    let source_again = track_document_to_canonical_dsl(&compiled, &names_for(&registry)).unwrap();
    assert_eq!(source_again, source);
}
