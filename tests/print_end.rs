#![allow(clippy::unwrap_used)]

use optive::{
    value::Value,
    vm::{OutputSink, OutputStream, Vm},
};
use std::sync::{Arc, Mutex};

fn capture(source: &str, workers: usize) -> (Value, String) {
    let output = Arc::new(Mutex::new(String::new()));
    let sink = output.clone();
    let mut vm = Vm::with_workers(workers);
    vm.set_output_sink(OutputSink::new(move |stream, text| {
        assert_eq!(stream, OutputStream::Stdout);
        sink.lock().unwrap().push_str(text);
    }));
    let value = optive::run_source_in_vm(&mut vm, source, "<print-end>").unwrap();
    let text = output.lock().unwrap().clone();
    (value, text)
}

#[test]
fn default_end_and_return_value_are_unchanged() {
    let (value, output) = capture("print()\nprint(1, true, none, \"x\")", 1);
    assert!(matches!(value, Value::None));
    assert_eq!(output, "\n1 true none x\n");
}

#[test]
fn empty_end_joins_calls_on_one_line() {
    let (value, output) = capture(
        r#"print("Answer: ", end="")
print(42)"#,
        1,
    );
    assert!(matches!(value, Value::None));
    assert_eq!(output, "Answer: 42\n");
}

#[test]
fn custom_end_is_literal_text_and_can_be_the_only_argument() {
    let (_, output) = capture(
        r#"print(1, 2, end="!")
print(end="\r")
print(end="")
print("done", end="\t")"#,
        1,
    );
    assert_eq!(output, "1 2!\rdone\t");
}

#[test]
fn aliases_and_argument_expansion_keep_keyword_support() {
    let (_, output) = capture(
        r#"let write = print
let options = {"end": ""}
write(*["a", "b"], **options)
print(**{"end": "!"})"#,
        1,
    );
    assert_eq!(output, "a b!");
}

#[test]
fn end_is_keyword_only_and_does_not_consume_positional_text() {
    let (_, output) = capture(r#"print("a", "!")"#, 1);
    assert_eq!(output, "a !\n");
}

#[test]
fn invalid_keywords_and_end_types_fail_before_output() {
    for source in [
        r#"print("x", end=42)"#,
        r#"print(end=none)"#,
        r#"print("x", sep="-")"#,
        r#"print(**{1: ""})"#,
    ] {
        let output = Arc::new(Mutex::new(String::new()));
        let sink = output.clone();
        let mut vm = Vm::with_workers(1);
        vm.set_output_sink(OutputSink::new(move |_, text| {
            sink.lock().unwrap().push_str(text);
        }));
        assert!(optive::run_source_in_vm(&mut vm, source, "<bad-print-end>").is_err());
        assert!(output.lock().unwrap().is_empty());
    }
}

#[test]
fn print_end_works_in_tasks_on_one_or_many_workers() {
    for workers in [1, 4] {
        let (_, output) = capture(
            r#"let task = go do { print("Answer: ", end="")
return 42 }
print(await task)"#,
            workers,
        );
        assert_eq!(output, "Answer: 42\n");
    }
}

#[test]
fn signature_and_check_expose_end_without_rejecting_empty_print() {
    for source in [
        "print()",
        "print(end=\"\")",
        "print(1, end=\"!\")",
        "func print(end) {}\nprint(42)",
    ] {
        assert!(optive::semantic::names::analyze_program(
            &optive::parser::Parser::parse(source).unwrap()
        )
        .is_empty());
    }
    assert!(!optive::lsp::diagnostics("print(end=42)", "print.tive").is_empty());
    assert!(!optive::lsp::diagnostics("print(sep=\"\")", "print.tive").is_empty());
    let help = optive::lsp::signature_help("print(1, ", 0, 9);
    assert_eq!(
        help["signatures"][0]["label"],
        "print(*values, end: text = \"\\n\")"
    );
    assert_eq!(
        help["signatures"][0]["parameters"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn other_builtins_do_not_silently_accept_keywords() {
    assert!(optive::run_source("len(x=[1])").is_err());
}

#[test]
fn expanded_calls_keep_their_arguments_across_suspension() {
    for workers in [1, 4] {
        let (_, output) = capture(
            r#"let task = go do {
std.time.sleep(0.001, **{})
print("Answer: ", **{"end": ""})
return 42
}
print(await task)"#,
            workers,
        );
        assert_eq!(output, "Answer: 42\n");
    }
}
