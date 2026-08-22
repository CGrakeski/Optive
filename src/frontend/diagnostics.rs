//! 带源码上下文的词法/语法诊断格式化。

use std::sync::Arc;

use crate::error::{LexError, ParseError};
use crate::vm::ErrorStackFrame;

const CONTEXT_LINES: usize = 2;

/// 格式化语法错误：源码上下文 + 插入符行。
#[must_use]
pub fn format_parse_error(source: &str, file: &str, err: &ParseError) -> String {
    let ParseError::Message {
        line,
        column,
        message,
    } = err;
    format_located_error(source, file, *line, *column, message)
}

/// 格式化词法错误：源码上下文 + 插入符行。
#[must_use]
pub fn format_lex_error(source: &str, file: &str, err: &LexError) -> String {
    let LexError::Message {
        line,
        column,
        message,
    } = err;
    format_located_error(source, file, *line, *column, message)
}

fn format_located_error(
    source: &str,
    file: &str,
    line: usize,
    column: usize,
    message: &str,
) -> String {
    let pack = crate::custom::active_pack();
    format_source_error(
        source,
        file,
        line,
        column,
        pack.parse_label_error(),
        pack.parse_arrow(),
        message,
    )
}

/// 在已知行号时，带源码上下文格式化运行时错误。
#[must_use]
pub fn format_runtime_at_line(source: &str, file: &str, line: usize, message: &str) -> String {
    let pack = crate::custom::active_pack();
    format_source_error(
        source,
        file,
        line,
        1,
        pack.parse_label_error(),
        pack.parse_arrow(),
        message,
    )
}

/// 带调用栈与各帧源码上下文的运行时错误。
#[must_use]
pub fn format_runtime_with_stack(
    fallback_source: &str,
    fallback_file: &str,
    message: &str,
    stack: &[ErrorStackFrame],
) -> String {
    if stack.is_empty() {
        return format_runtime_at_line(fallback_source, fallback_file, 1, message);
    }

    let pack = crate::custom::active_pack();
    let mut out = String::new();
    out.push('\n');
    out.push_str(pack.traceback_header());
    out.push('\n');

    let bottom_up = pack.traceback_direction() == crate::custom::TraceDirection::BottomUp;
    let iter: Box<dyn Iterator<Item = (usize, &ErrorStackFrame)>> = if bottom_up {
        Box::new(stack.iter().enumerate().rev())
    } else {
        Box::new(stack.iter().enumerate())
    };

    for (i, frame) in iter {
        let is_innermost = i + 1 == stack.len();
        let line = if frame.line == 0 { 1 } else { frame.line };
        out.push_str(&pack.format_traceback_frame(&frame.file, line, &frame.func));
        out.push('\n');
        let src =
            frame
                .source
                .as_deref()
                .unwrap_or(if is_innermost { fallback_source } else { "" });
        if let Some(text) = source_line(src, line) {
            out.push_str(&format!("    {text}\n"));
            if is_innermost {
                out.push_str("    ");
                let caret_at = caret_display_col(text, frame.column);
                for _ in 0..caret_at {
                    out.push(' ');
                }
                out.push_str("^\n");
            }
        }
    }
    // Python 风格：末行直接 `TypeName: message`，不再包一层 `Error:`。
    out.push_str(message);
    if !message.ends_with('\n') {
        out.push('\n');
    }

    out
}

fn source_line(source: &str, line: usize) -> Option<&str> {
    if source.is_empty() || line == 0 {
        return None;
    }
    source.lines().nth(line - 1).map(str::trim_end)
}

/// 将 1-based 源码列号转为 `text` 上的显示偏移（并夹紧）。
fn caret_display_col(text: &str, column: usize) -> usize {
    let col_idx = column.saturating_sub(1);
    let mut display = 0usize;
    for (i, ch) in text.chars().enumerate() {
        if i >= col_idx {
            break;
        }
        display += char_display_width(ch);
    }
    display.min(
        text.chars()
            .map(char_display_width)
            .sum::<usize>()
            .saturating_sub(0),
    )
}

fn format_source_error(
    source: &str,
    file: &str,
    line: usize,
    column: usize,
    label: &str,
    arrow: &str,
    message: &str,
) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let line_idx = line.saturating_sub(1);
    let col_idx = column.saturating_sub(1);

    let mut out = String::new();
    out.push_str(label);
    out.push_str(message);
    out.push('\n');
    out.push_str(arrow);
    out.push_str(&format!("{file}:{line}:{column}\n"));
    // 与源码行 `{n:>3} |` 对齐，避免 `^` 相对文本左偏（看起来像指到中间）。
    out.push_str(&format!("{:>3} |\n", ""));

    let start = line_idx.saturating_sub(CONTEXT_LINES);
    let end = (line_idx + CONTEXT_LINES + 1).min(lines.len());

    for (i, text) in lines.iter().enumerate().take(end).skip(start) {
        let display_line = i + 1;
        out.push_str(&format!("{display_line:>3} | {text}\n"));
        if i == line_idx {
            out.push_str(&format!("{:>3} | ", ""));
            let caret_pad = text
                .chars()
                .take(col_idx)
                .map(char_display_width)
                .sum::<usize>();
            for _ in 0..caret_pad {
                out.push(' ');
            }
            out.push_str("^\n");
        }
    }

    out
}

const fn char_display_width(ch: char) -> usize {
    if ch == '\t' {
        4
    } else {
        1
    }
}

/// 为已编译程序中每个函数挂上定义处源码与文件名。
pub fn attach_function_sources(
    program: &mut crate::opcode::CompiledProgram,
    source: &str,
    file: &str,
) {
    let source_rc: Arc<str> = Arc::from(source);
    let updated: std::collections::HashMap<_, _> = program
        .functions
        .iter()
        .map(|(k, f)| {
            let mut func = (**f).clone();
            if func.source.is_none() {
                func.source = Some(source_rc.clone());
                func.source_file = file.to_string();
            }
            (k.clone(), Arc::new(func))
        })
        .collect();
    program.functions = updated;

    let generics: std::collections::HashMap<_, _> = program
        .generic_functions
        .iter()
        .map(|(k, t)| {
            let mut template = (**t).clone();
            if template.source.is_none() {
                template.source = Some(source_rc.clone());
                template.source_file = file.to_string();
            }
            (k.clone(), Arc::new(template))
        })
        .collect();
    program.generic_functions = generics;

    patch_pushes(
        &mut program.code,
        &program.functions,
        &program.generic_functions,
        &source_rc,
        file,
    );
    let names: Vec<String> = program.functions.keys().cloned().collect();
    for name in names {
        let Some(f) = program.functions.get(&name).cloned() else {
            continue;
        };
        let mut body = (*f.body).clone();
        if !patch_pushes(
            &mut body,
            &program.functions,
            &program.generic_functions,
            &source_rc,
            file,
        ) {
            continue;
        }
        let mut func = (*f).clone();
        func.body = Arc::new(body);
        func.hot = crate::hot_code::HotCode::encode(&func.body);
        program.functions.insert(name, Arc::new(func));
    }
}

fn patch_pushes(
    code: &mut [crate::opcode::Instruction],
    functions: &std::collections::HashMap<String, Arc<crate::opcode::FunctionObject>>,
    generics: &std::collections::HashMap<String, Arc<crate::opcode::GenericFunctionTemplate>>,
    source: &Arc<str>,
    file: &str,
) -> bool {
    use crate::opcode::Instruction;
    use crate::value::Value;
    let mut changed = false;
    for ins in code.iter_mut() {
        match ins {
            Instruction::Push(Value::Function(f)) => {
                if f.source.is_some() {
                    if let Some(u) = functions.get(&f.name) {
                        if !Arc::ptr_eq(f, u) {
                            *ins = Instruction::Push(Value::Function(u.clone()));
                            changed = true;
                        }
                    }
                    continue;
                }
                let replacement = if let Some(u) = functions.get(&f.name) {
                    u.clone()
                } else {
                    let mut func = (**f).clone();
                    func.source = Some(source.clone());
                    func.source_file = file.to_string();
                    Arc::new(func)
                };
                *ins = Instruction::Push(Value::Function(replacement));
                changed = true;
            }
            Instruction::Push(Value::GenericFunction(t)) => {
                let replacement = if let Some(u) = generics.get(&t.name) {
                    u.clone()
                } else if t.source.is_none() {
                    let mut template = (**t).clone();
                    template.source = Some(source.clone());
                    template.source_file = file.to_string();
                    Arc::new(template)
                } else {
                    continue;
                };
                if !Arc::ptr_eq(t, &replacement) {
                    *ins = Instruction::Push(Value::GenericFunction(replacement));
                    changed = true;
                }
            }
            _ => {}
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ParseError;

    #[test]
    fn format_shows_caret() {
        let src = "func fib(n) {\n    return fib(n-1)\n}";
        let err = ParseError::here(2, 15, "expected ')'");
        let msg = format_parse_error(src, "<repl>", &err);
        assert!(msg.contains("error: expected ')'"));
        assert!(msg.contains("--> <repl>:2:15"));
        assert!(msg.contains('^'));
        assert!(msg.contains("return fib(n-1)"));
    }

    #[test]
    fn format_caret_aligns_with_source_gutter() {
        let src = "handle";
        let err = ParseError::here(1, 7, "expected expression");
        let msg = format_parse_error(src, "<repl>", &err);
        let lines: Vec<&str> = msg.lines().collect();
        let src_line = lines
            .iter()
            .find(|l| l.contains("handle"))
            .copied()
            .unwrap();
        let caret = lines.iter().find(|l| l.contains('^')).copied().unwrap();
        let src_text_at = src_line.find('h').unwrap();
        let caret_at = caret.find('^').unwrap();
        // 列 7 = 词尾之后；相对源码文本起点偏移 6
        assert_eq!(
            caret_at,
            src_text_at + 6,
            "src={src_line:?} caret={caret:?}"
        );
    }

    #[test]
    fn format_stack_shows_frames() {
        // 列 12 是 `func b() { c }` 中的 `c`（1-based）。
        let stack = vec![
            ErrorStackFrame {
                func: "<module>".into(),
                file: "<repl>".into(),
                line: 1,
                column: 1,
                source: Some(Arc::from("a()")),
            },
            ErrorStackFrame {
                func: "a".into(),
                file: "<repl>".into(),
                line: 1,
                column: 12,
                source: Some(Arc::from("func a() { b() }")),
            },
            ErrorStackFrame {
                func: "b".into(),
                file: "<repl>".into(),
                line: 1,
                column: 12,
                source: Some(Arc::from("func b() { c }")),
            },
        ];
        let msg = format_runtime_with_stack("a()", "<repl>", "undefined name: c", &stack);
        assert!(msg.contains("Traceback"));
        assert!(msg.contains("in a"));
        assert!(msg.contains("in b"));
        assert!(msg.contains("b()"));
        assert!(msg.contains("undefined name: c"));
        let lines: Vec<&str> = msg.lines().collect();
        let caret = lines
            .iter()
            .rev()
            .find(|l| l.contains('^'))
            .copied()
            .unwrap();
        let src = lines
            .iter()
            .rev()
            .find(|l| l.contains("func b()"))
            .copied()
            .unwrap();
        let caret_col = caret.find('^').unwrap();
        // `"    "` 前缀 + 列 12 的显示偏移
        let expected = 4 + caret_display_col("func b() { c }", 12);
        assert_eq!(caret_col, expected, "caret={caret:?} src={src:?}");
    }

    #[test]
    fn caret_uses_recorded_column() {
        assert_eq!(caret_display_col("func b() { c }", 12), 11);
        assert_eq!(caret_display_col("  x", 3), 2);
    }
}
