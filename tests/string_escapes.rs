#![allow(clippy::expect_used, clippy::panic)]

mod common;
use common::assert_text;
use optive::value::Value;

#[test]
fn all_text_literal_forms_share_escape_rules() {
    let body = r"\n\r\t\a\b\f\v\0\xAB\101\u0041\U0001F600\u{1F600}\N{SNOWMAN}";
    let expected = "\n\r\t\u{7}\u{8}\u{c}\u{b}\0«AA😀😀☃";
    for (open, close) in [
        ("\"", "\""),
        ("\"\"\"", "\"\"\""),
        ("f\"", "\""),
        ("f\"\"\"", "\"\"\""),
    ] {
        assert_text(&format!("{open}{body}{close}"), expected);
    }
    assert_text(r#""\"\'\\""#, "\"'\\");
}

#[test]
fn escape_widths_and_unicode_scalar_boundaries() {
    assert_text(r#""\x4142\u00411\U000000411\1012""#, "A42A1A1A2");
    assert_text(r#""\400\777""#, "Āǿ");
    assert_text(
        r#""\u0000\u007f\u0080\uD7FF\uE000\U0010FFFF""#,
        "\0\u{7f}\u{80}\u{d7ff}\u{e000}\u{10ffff}",
    );
}

#[test]
fn named_unicode_characters_include_algorithmic_names() {
    assert_text(
        r#""\N{LATIN CAPITAL LETTER A}\N{snowman}\N{CJK UNIFIED IDEOGRAPH-4E2D}\N{HANGUL SYLLABLE GA}""#,
        "A☃中가",
    );
    assert_text(r#""\N{NULL}\N{NEWLINE}\N{BACKSPACE}""#, "\0\n\u{8}");
}

#[test]
fn raw_literals_do_not_decode_or_validate_escapes() {
    let body = r"\n\r\t\a\b\f\v\0\xGG\uD800\UFFFFFFFF\u{}\N{NOT A NAME}";
    assert_text(&format!("r\"{body}\""), body);
    assert_text(&format!("r\"\"\"{body}\"\"\""), body);
}

#[test]
fn malformed_known_escapes_are_lexical_errors() {
    for body in [
        r"\x",
        r"\x1",
        r"\xGG",
        r"\u12",
        r"\uZZZZ",
        r"\U123",
        r"\U00110000",
        r"\UFFFFFFFF",
        r"\uD800",
        r"\uDFFF",
        r"\u{D800}",
        r"\u{}",
        r"\u{110000}",
        r"\u{1234567}",
        r"\u{41",
        r"\N",
        r"\N{}",
        r"\N{NOT A REAL CHARACTER}",
        r"\N{SNOWMAN",
    ] {
        for prefix in ["", "f", "b"] {
            let source = format!("{prefix}\"{body}\"");
            assert!(optive::tokenize(&source).is_err(), "{source}");
        }
    }
}

#[test]
fn unknown_escapes_are_preserved_in_text_and_bytes() {
    assert_text(r#""\q\8\z""#, r"\q\8\z");
    assert_text(r#"f"\q\8\z""#, r"\q\8\z");
    assert_bytes(r#"b"\q\8\z""#, br"\q\8\z");
}

fn assert_bytes(source: &str, expected: &[u8]) {
    let Value::Bytes(bytes) = optive::run_source(source).expect("bytes literal") else {
        panic!("expected bytes");
    };
    assert_eq!(bytes.as_slice(), expected);
}

#[test]
fn bytes_distinguish_raw_byte_escapes_from_utf8_unicode() {
    assert_bytes(r#"b"\xAB\253\0""#, &[0xab, 0xab, 0]);
    assert_bytes(
        r#"b"\u00AB\U0001F600\u{4E2D}\N{SNOWMAN}""#,
        "«😀中☃".as_bytes(),
    );
    assert_bytes(r#"b"\a\b\f\v\n\r\t\"\'\\""#, b"\x07\x08\x0c\x0b\n\r\t\"'\\");
    assert!(optive::tokenize(r#"b"\400""#).is_err());
    assert!(optive::tokenize(r#"b"\777""#).is_err());
}

#[test]
fn escaped_newlines_are_continuations_but_raw_triples_preserve_them() {
    for prefix in ["", "f", "b"] {
        for newline in ["\n", "\r\n"] {
            let source = format!("{prefix}\"left\\{newline}right\"");
            if prefix == "b" {
                assert_bytes(&source, b"leftright");
            } else {
                assert_text(&source, "leftright");
            }
        }
    }
    assert_text("r\"\"\"left\\\nright\"\"\"", "left\\\nright");
}

#[test]
fn fstring_escaped_braces_are_literal_and_expressions_decode_once() {
    assert_text(
        r#"f"\x7B\u007D\u{7B}\N{RIGHT CURLY BRACKET}\{\}={2}""#,
        "{}{}{}=2",
    );
    assert_text(
        r#"f"{len("\n")}:{len("{")}:{ {"x": "\u0041"}["x"] }""#,
        "1:1:A",
    );
    assert_text(r#"f"{len(r"\n")}:{len("\\n")}""#, "2:2");
    assert_text(r#"f"{1 /* } 中文 */ + 1}""#, "2");
    assert_text("f\"\"\"{1 // }\n+ 1}\"\"\"", "2");
}

#[test]
fn formatting_preserves_new_escapes_and_fstring_boundaries() {
    for source in [
        r#""\a\b\f\v\0\u0085\u{2028}\N{SNOWMAN}""#,
        r#"f"\a\b\f\v\0\u0085\r\t\x7B{2}\x7D""#,
        r#"f"{len("\n")}:{len("{")}""#,
    ] {
        let expected = optive::eval_text(source).expect("original source");
        let formatted = optive::fmt::format_source(source).expect("format");
        assert_text(&formatted, &expected);
        assert_eq!(
            optive::fmt::format_source(&formatted).expect("format twice"),
            formatted
        );
    }
    let source = r#"b"\xAB\u00AB\0""#;
    assert_bytes(
        &optive::fmt::format_source(source).expect("format bytes"),
        &[0xab, 0xc2, 0xab, 0],
    );
}
