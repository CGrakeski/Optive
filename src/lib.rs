//! Optive programming language interpreter (Rust).
//!
//! 本地脚本拥有本机文件系统、进程与模块导入权限。

pub mod compiler;
pub mod custom;
pub mod frontend;
pub mod runtime;
pub mod stdlib;

pub use compiler::{
    codegen, free_vars, hot_code, monomorph, opcode, protocol, specialize, stack_effect,
};
pub use frontend::{ast, diagnostics, error, fmt, lexer, parser, token};
pub use runtime::{
    builtins, caps, concurrency, c_types, debug, enum_variant, exceptions, ffi, ffi_extra, ffi_pool, gc, module,
    ptr_registry, runtime_ast, scheduler, shared, sized, traceback, type_registry, types, value, vm,
};
pub use stdlib as std_modules;

pub use error::{ExceptionKind, LexError, ParseError, RuntimeError};
pub use parser::Parser;
pub use token::{Token, TokenKind};

use codegen::Generator;
use parser::Parser as P;

pub type Result<T> = std::result::Result<T, error::RuntimeError>;

pub fn tokenize(source: &str) -> std::result::Result<Vec<Token>, LexError> {
    lexer::Lexer::new(source).tokenize()
}

pub fn parse_program(source: &str) -> std::result::Result<ast::Program, ParseError> {
    parser::Parser::parse(source)
}

pub fn compile(source: &str) -> Result<opcode::CompiledProgram> {
    let program = P::parse(source).map_err(|e| {
        RuntimeError::msg(diagnostics::format_parse_error(source, "<compile>", &e))
    })?;
    Generator::new().compile(&program)
}

pub fn run_source(source: &str) -> Result<value::Value> {
    let mut vm = vm::Vm::new();
    run_source_in_vm(&mut vm, source, "<script>")
}

pub fn run_source_in_vm(vm: &mut vm::Vm, source: &str, file: &str) -> Result<value::Value> {
    vm.source_file = file.to_string();
    vm.current_source = Some(std::sync::Arc::from(source));
    if file != "<script>" && file != "<repl>" {
        let path = std::path::Path::new(file);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                vm.import_base = parent.to_path_buf();
            }
        } else if path.extension().is_some() || file.contains('/') || file.contains('\\') {
            vm.import_base = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        }
    }
    let program = P::parse(source).map_err(|e| {
        RuntimeError::msg(diagnostics::format_parse_error(source, file, &e))
    })?;
    let mut compiled = Generator::new().compile(&program)?;
    diagnostics::attach_function_sources(&mut compiled, source, file);
    vm.load_program(compiled)?;
    vm.run().map_err(|e| {
        let stack = vm.take_error_stack();
        format_runtime_error(source, file, &e, vm.current_line(), &stack)
    })
}

fn format_runtime_error(
    source: &str,
    file: &str,
    err: &RuntimeError,
    line: usize,
    stack: &[vm::ErrorStackFrame],
) -> RuntimeError {
    let kind = err.kind();
    let message = err.message().to_string();
    // 已格式化的诊断（解析风格）不应再次包装。
    if message.starts_with("error:") || message.starts_with("\nTraceback") {
        return err.clone();
    }
    if !stack.is_empty() {
        return RuntimeError::typed(
            kind,
            diagnostics::format_runtime_with_stack(source, file, &message, stack),
        );
    }
    if line > 0 {
        RuntimeError::typed(
            kind,
            diagnostics::format_runtime_at_line(source, file, line, &message),
        )
    } else {
        err.clone()
    }
}

/// REPL 辅助：检查源码是否有未闭合分隔符（忽略 `//` / `#` 行注释与字符串内容）。
pub fn repl_needs_continuation(source: &str) -> bool {
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut line_comment = false;
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '#' || (ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/') {
            line_comment = true;
            i += 1;
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            _ => {}
        }
        i += 1;
    }
    in_string || paren > 0 || bracket > 0 || brace > 0
}

/// 运行源码并返回数值结果字符串（测试用）。
pub fn eval_num(source: &str) -> Result<String> {
    match run_source(source)? {
        value::Value::Num(n) => Ok(n.to_string()),
        v => Err(RuntimeError::msg(format!(
            "expected num, got {}",
            v.display_string()
        ))),
    }
}

/// 运行源码并返回 bool 结果（测试用）。
pub fn eval_bool(source: &str) -> Result<bool> {
    match run_source(source)? {
        value::Value::Bool(b) => Ok(b),
        v => Err(RuntimeError::msg(format!(
            "expected bool, got {}",
            v.display_string()
        ))),
    }
}

/// 运行源码并返回文本结果（测试用）。
pub fn eval_text(source: &str) -> Result<String> {
    let v = run_source(source)?;
    match &v {
        value::Value::Text(s) => Ok(s.clone()),
        value::Value::TypeRef(s) => Ok(s.clone()),
        value::Value::TypeSpec(_) => Ok(v.display_string()),
        other => Err(RuntimeError::msg(format!(
            "expected text, got {}",
            other.display_string()
        ))),
    }
}
