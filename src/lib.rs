//! Optive programming language interpreter (Rust).
//!
//! 本地脚本拥有本机文件系统、进程与模块导入权限。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "lsp/catalog.rs"]
pub mod api_registry;
pub mod compiler;
pub mod custom;
pub mod dap;
pub mod embed;
pub mod frontend;
pub mod lsp;
pub mod rpc;
pub mod runtime;
pub mod semantic;
pub mod stdlib;
pub mod versions;

pub use compiler::{
    bc_cache, codegen, free_vars, hot_code, monomorph, opcode, protocol, specialize, stack_effect,
};
pub use frontend::{ast, diagnostics, error, fmt, lexer, parser, token};
pub use runtime::{
    builtins, c_types, caps, concurrency, coverage, debug, enum_variant, exceptions, ffi,
    ffi_extra, ffi_pool, gc, metrics, module, ptr_registry, runtime_ast, scheduler, shared, sized,
    traceback, type_registry, types, value, vm,
};
pub use stdlib as std_modules;

pub use error::{ExceptionKind, LexError, ParseError, RuntimeError};
pub use lexer::InputStatus;
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
    let program = P::parse(source)
        .map_err(|e| RuntimeError::msg(diagnostics::format_parse_error(source, "<compile>", &e)))?;
    Generator::new().compile(&program)
}

/// 使用运行时上下文编译源码，并在装载前补齐调试与覆盖率元数据。
///
/// 调用方仍负责设置 `Vm` 的当前源码上下文，并决定何时 `load_program`。
pub fn compile_with_context(
    vm: &vm::Vm,
    source: &str,
    file: &str,
) -> Result<opcode::CompiledProgram> {
    let mut compiled = if crate::bc_cache::should_use(file) {
        let mut dep_ids: Vec<String> = vm.dep_map.values().map(|d| d.id.clone()).collect();
        dep_ids.sort();
        let key = crate::bc_cache::key(
            &crate::versions::bytecode_cache_version(),
            file,
            source,
            &dep_ids.join(","),
        );
        let path = crate::bc_cache::cache_dir().join(format!("{key}.tivc"));
        if let Some(cached) = crate::bc_cache::load(&path) {
            crate::bc_cache::note_hit();
            cached
        } else {
            let program = P::parse(source).map_err(|e| {
                RuntimeError::msg(diagnostics::format_parse_error(source, file, &e))
            })?;
            let compiled = Generator::new().compile(&program)?;
            if crate::bc_cache::store(&path, &compiled) {
                crate::bc_cache::note_store();
            }
            compiled
        }
    } else {
        let program = P::parse(source)
            .map_err(|e| RuntimeError::msg(diagnostics::format_parse_error(source, file, &e)))?;
        Generator::new().compile(&program)?
    };
    diagnostics::attach_function_sources(&mut compiled, source, file);
    crate::coverage::note_compiled(vm, file, &compiled);
    Ok(compiled)
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
            vm.import_base =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        }
    }
    let compiled = compile_with_context(vm, source, file)?;
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

/// REPL 辅助：复用 lexer 的 incomplete-input 状态。
#[must_use]
pub fn repl_needs_continuation(source: &str) -> bool {
    lexer::input_status(source).is_incomplete()
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
