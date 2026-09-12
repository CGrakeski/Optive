#![allow(clippy::expect_used, clippy::panic)]

mod common;

use common::{assert_num, run_err};
use optive::opcode::Instruction;
use optive::value::{FrozenValue, Num, Value};

#[test]
fn let_is_immutable_and_var_is_mutable() {
    run_err(
        r"
let answer = 41
answer = 42
",
    );
    assert_num(
        r"
var answer = 41
answer = answer + 1
answer
",
        "42",
    );
}

#[test]
fn const_func_call_is_replaced_by_a_value() {
    let source = r"
const func factorial(n) {
    if (n <= 1) { return 1 }
    return n * factorial(n - 1)
}
const ANSWER = factorial(6)
ANSWER
";
    assert_num(source, "720");

    let program = optive::compile(source).expect("compile const program");
    assert!(!program.functions.contains_key("factorial"));
    assert!(
        !program
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Call { .. })),
        "const call must not survive into runtime bytecode: {:?}",
        program.code
    );
}

#[test]
fn const_rejects_runtime_values_and_effects() {
    assert!(optive::compile("const X = read_file(\"x\")\n").is_err());
    assert!(optive::compile(
        r"
const func twice(x) { return x * 2 }
let runtime = 2
twice(runtime)
"
    )
    .is_err());
    assert!(optive::compile(
        r"
const func invalid(x) {
    let result = x
    result = result + 1
    return result
}
invalid(1)
"
    )
    .is_err());
}

#[test]
fn old_redundant_const_declaration_is_rejected() {
    assert!(optive::compile("const let X = 1\n").is_err());
}

#[test]
fn const_func_supports_local_loops_and_frozen_sequences() {
    let source = r#"
const VALUES = (2, 3, 5, 7)
const func sum_to(n) {
    var total = 0
    var i = 0
    while (i <= n) {
        total = total + i
        i = i + 1
    }
    loop (100) {
        if (total > 10) { break }
        total = total + 1
    }
    return total + VALUES[2] + len(VALUES)
}
const RESULT = sum_to(4)
RESULT
"#;
    assert_num(source, "20");
    let program = optive::compile(source).unwrap();
    assert!(!program
        .code
        .iter()
        .any(|op| matches!(op, Instruction::Call { .. })));
}

#[test]
fn const_func_effects_are_rejected_at_definition_even_when_unused() {
    let error = optive::compile(
        r#"
const func impure() {
    return read_file("secret")
}
1
"#,
    )
    .err()
    .expect("impure const func must fail at definition");
    assert!(error.to_string().contains("forbidden effect"), "{error}");
    assert!(error.to_string().contains("read_file"), "{error}");
}

#[test]
fn imported_interface_values_participate_in_ctfe() {
    let mut imported = std::collections::HashMap::new();
    imported.insert("BASE".to_string(), Value::Num(Num::from_i64(21)));
    let program = optive::compile_with_const_values(
        "use constants.{ BASE }\nconst ANSWER = BASE * 2\nANSWER\n",
        imported,
    )
    .expect("compile with imported const interface");
    assert!(program.code.iter().any(|op| matches!(
        op,
        Instruction::PushSmall(42) | Instruction::Push(Value::Num(Num::Small(42)))
    )));
}

#[test]
fn const_collections_are_deeply_frozen_without_freezing_runtime_literals() {
    let source = r#"
const func data() {
    return [[2, 3], {"answer": 42}, {2, 3, 2}]
}
const DATA = data()
DATA[0][1] + DATA[1]["answer"] + len(DATA[2])
"#;
    assert_num(source, "47");

    let program = optive::compile(source).expect("compile frozen constants");
    assert!(program.code.iter().any(|instruction| matches!(
        instruction,
        Instruction::Push(Value::Frozen(value))
            if matches!(value.as_ref(), FrozenValue::List(_))
    )));

    run_err(
        r#"
const DATA = [1, 2]
DATA[0] = 9
"#,
    );

    assert_num(
        r#"
var runtime = [1, 2]
runtime[0] = 9
runtime[0]
"#,
        "9",
    );
}

#[test]
fn imported_module_member_is_compile_time() {
    let mut values = std::collections::HashMap::new();
    values.insert("BASE".to_string(), Value::Num(Num::from_i64(21)));
    let mut modules = std::collections::HashMap::new();
    modules.insert(
        "config".to_string(),
        optive::compiler::const_eval::CompileTimeModule::new("config", values),
    );
    let program = optive::compile_with_const_imports(
        "import config\nconst ANSWER = config.BASE * 2\nANSWER\n",
        optive::compiler::const_eval::ConstImports {
            values: std::collections::HashMap::new(),
            modules,
        },
    )
    .expect("compile imported module member");
    assert!(program.code.iter().any(|op| matches!(
        op,
        Instruction::PushSmall(42) | Instruction::Push(Value::Num(Num::Small(42)))
    )));
}

#[test]
fn const_func_for_and_pure_builtins() {
    let source = r#"
const func total(xs) {
    var sum = 0
    for (item in xs) {
        if (item == 2) { continue }
        if (item > 8) { break }
        sum = sum + item
    }
    return sum + min(2, 9) + max(1, 3) + abs(-4) + len(range(3))
}
const RESULT = total((1, 2, 3, 10))
RESULT
"#;
    assert_num(source, "16");
}

#[test]
fn const_func_destructuring_and_stack_diagnostics() {
    let source = r#"
const func unpack(pair) {
    let (a, b) = pair
    return a + b
}
const RESULT = unpack((20, 22))
RESULT
"#;
    assert_num(source, "42");

    let error = optive::compile(
        r#"
const func boom(n) {
    return 1 / n
}
const BAD = boom(0)
"#,
    )
    .err()
    .expect("division by zero is a compile-time error");
    let text = error.to_string();
    assert!(
        text.contains("boom") || text.contains("const `BAD`"),
        "{text}"
    );
}

#[test]
fn compile_time_recursion_cycle_is_reported() {
    let error = optive::compile(
        r#"
const func loop_a(n) {
    return loop_a(n)
}
const BAD = loop_a(1)
"#,
    )
    .err()
    .expect("recursive cycle must fail");
    let text = error.to_string();
    assert!(
        text.contains("recursion") || text.contains("step limit") || text.contains("cycle"),
        "{text}"
    );
}
