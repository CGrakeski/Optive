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
    bc_cache, bundle, codegen, const_effect, const_eval, const_imports, free_vars, hot_code,
    module_interface, module_resolve, monomorph, opcode, protocol, specialize, stack_effect,
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

pub fn compile_with_const_values(
    source: &str,
    values: std::collections::HashMap<String, value::Value>,
) -> Result<opcode::CompiledProgram> {
    compile_with_const_imports(
        source,
        const_eval::ConstImports {
            values,
            modules: Default::default(),
        },
    )
}

pub fn compile_with_const_imports(
    source: &str,
    imports: const_eval::ConstImports,
) -> Result<opcode::CompiledProgram> {
    let program = P::parse(source)
        .map_err(|e| RuntimeError::msg(diagnostics::format_parse_error(source, "<compile>", &e)))?;
    Generator::new().compile_with_const_imports(&program, imports)
}

/// 使用运行时上下文编译源码，并在装载前补齐调试与覆盖率元数据。
///
/// 调用方仍负责设置 `Vm` 的当前源码上下文，并决定何时 `load_program`。
pub fn compile_with_context(
    vm: &vm::Vm,
    source: &str,
    file: &str,
) -> Result<opcode::CompiledProgram> {
    let program = P::parse(source)
        .map_err(|e| RuntimeError::msg(diagnostics::format_parse_error(source, file, &e)))?;
    let imports = const_imports::collect(&program, &VmConstSource(vm), &mut Default::default())
        .map_err(RuntimeError::msg)?;
    let mut compiled = if bc_cache::should_use(file) && !vm.caps.fs_restricted() {
        let mut dep_ids: Vec<String> = vm.dep_map.values().map(|d| d.id.clone()).collect();
        dep_ids.sort();
        let key = bc_cache::key(
            &crate::versions::bytecode_cache_version(),
            file,
            source,
            &format!(
                "{}\0{}",
                dep_ids.join(","),
                const_imports::fingerprint(&imports)
            ),
        );
        let path = crate::bc_cache::cache_dir().join(format!("{key}.tivc"));
        if let Some(cached) = bc_cache::load(&path) {
            crate::bc_cache::note_hit();
            cached
        } else {
            let compiled = Generator::new().compile_with_const_imports(&program, imports)?;
            if crate::bc_cache::store(&path, &compiled) {
                crate::bc_cache::note_store();
            }
            compiled
        }
    } else {
        Generator::new().compile_with_const_imports(&program, imports)?
    };
    diagnostics::attach_function_sources(&mut compiled, source, file);
    crate::coverage::note_compiled(vm, file, &compiled);
    Ok(compiled)
}

struct VmConstSource<'a>(&'a vm::Vm);

impl const_imports::ConstImportSource for VmConstSource<'_> {
    fn package_id(&self) -> &str {
        &self.0.current_package_id
    }

    fn package_root(&self) -> Option<&std::path::Path> {
        self.0.package_root.as_deref()
    }

    fn import_base(&self) -> &std::path::Path {
        &self.0.import_base
    }

    fn is_builtin(&self, name: &str) -> bool {
        module_resolve::is_builtin_or_host_root(name, self.0.builtin_modules.keys())
    }

    fn dep(&self, parent: &str, name: &str) -> Option<(std::path::PathBuf, String)> {
        self.0
            .dep_map
            .get(&(parent.to_string(), name.to_string()))
            .map(|binding| (binding.path.clone(), binding.id.clone()))
    }

    fn is_file(&self, path: &std::path::Path) -> bool {
        self.0
            .caps
            .lookup_is_file("const import", path)
            .unwrap_or(false)
    }

    fn read(&self, path: &std::path::Path) -> std::result::Result<String, String> {
        self.0
            .caps
            .read_to_string("const import", path)
            .map_err(|error| error.to_string())
    }
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
            diagnostics::format_runtime_with_stack(source, file, kind, &message, stack),
        );
    }
    if line > 0 {
        RuntimeError::typed(
            kind,
            diagnostics::format_runtime_at_line(source, file, line, kind, &message),
        )
    } else {
        err.clone()
    }
}

/// REPL 辅助：复用 lexer 的 incomplete-input 状态。
#[must_use]
pub fn repl_needs_continuation(source: &str) -> bool {
    if lexer::input_status(source).is_incomplete() {
        return true;
    }
    let Err(ParseError::Message {
        line,
        column,
        message,
    }) = parser::Parser::parse(source)
    else {
        return false;
    };
    // A parser error at the end of the current cell usually means the user has
    // entered a valid prefix (`1 +`, `let x =`, `func f()`) and should receive
    // a continuation prompt instead of an immediate error. Errors before EOF
    // remain complete so malformed input is reported without trapping the REPL.
    let end_line = source.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let end_column = source
        .rsplit_once('\n')
        .map_or(source, |(_, tail)| tail)
        .chars()
        .count()
        + 1;
    line == end_line
        && column >= end_column
        && (message.starts_with("expected ") || message.contains("unterminated"))
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
