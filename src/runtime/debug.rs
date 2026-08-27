//! Optive debugger：CLI 会话、断点与暂停钩子。
//!
//! 热路径仅在 `Vm.debug.is_some()` 时多一次 Option 检查；未附加调试器时与原先一致。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::RuntimeError;
use crate::value::{TaskInner, TaskState, Value};
use crate::vm::{ErrorStackFrame, Vm};
use crate::Result;

use crate::shared::Shared;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    /// 步入：下一源码行即停（可进入调用）。
    In,
    /// 步过：回到深度 ≤ 起始深度时的下一行。
    Over { max_depth: usize },
    /// 跑到当前函数返回（深度 < 起始深度）。
    Out { target_depth: usize },
    /// 下一条字节码即停。
    Insn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Breakpoint,
    Step,
    Explicit,
    Uncaught,
    Entry,
}

#[derive(Clone, Debug, Default)]
pub struct LineBreakpoint {
    pub condition: Option<String>,
    pub log: Option<String>,
}

#[derive(Clone)]
pub struct DebugState {
    /// `(规范化路径, 行号)` → 可选条件 / 日志。
    pub line_breakpoints: HashMap<(String, usize), LineBreakpoint>,
    /// 函数名断点（精确名，或 `*substr` 子串）。
    pub function_breakpoints: HashSet<String>,
    pub pending_break: bool,
    pub stop_reason: Option<StopReason>,
    pub step: Option<StepMode>,
    /// 避免在同一 (file,line,pc) 上连续重断。
    pub last_stop: Option<(String, usize, usize)>,
    /// 函数断点已命中、尚未离开该函数时的模式串。
    pub armed_func_bp: Option<String>,
    /// 上一停点 / 检查点所见函数名（用于检测「进入」）。
    pub last_func_name: Option<String>,
    /// 行断点抑制：刚停在该行，直到离开该行前不再重断。
    pub bp_skip_line: Option<(String, usize)>,
    /// 上次检查到的 (file,line)，用于检测「已离开该行」以解除抑制。
    pub last_visited: Option<(String, usize)>,
    /// 启动后先在入口停一次。
    pub stop_on_entry: bool,
    pub started: bool,
    /// 未捕获异常停下时暂存错误文案。
    pub last_uncaught: Option<String>,
    /// DAP `exceptionBreakpointFilters`: 未捕获异常（默认开）。
    pub exception_uncaught: bool,
    /// 每次 `throw` 都停（`raised`）。
    pub exception_raised: bool,
    /// 步进/断点焦点；默认 `All`（任意纤程均可停）。
    pub step_focus: StepFocus,
    /// `fiber N` 列表下标（仅展示；`All`/`Main` 时为 `None`）。
    pub focus_fiber_index: Option<usize>,
}

/// 调试器步进与断点的纤程焦点。
#[derive(Clone, Default)]
pub enum StepFocus {
    /// 任意纤程（默认）。
    #[default]
    All,
    /// 仅主纤程。
    Main,
    /// 指定任务纤程。
    Fiber(Shared<TaskInner>),
}

impl Default for DebugState {
    fn default() -> Self {
        Self {
            line_breakpoints: HashMap::new(),
            function_breakpoints: HashSet::new(),
            pending_break: false,
            stop_reason: None,
            step: None,
            last_stop: None,
            armed_func_bp: None,
            last_func_name: None,
            bp_skip_line: None,
            last_visited: None,
            stop_on_entry: true,
            started: false,
            last_uncaught: None,
            exception_uncaught: true,
            exception_raised: false,
            step_focus: StepFocus::All,
            focus_fiber_index: None,
        }
    }
}

impl DebugState {
    pub const fn request_break(&mut self, reason: StopReason) {
        self.pending_break = true;
        self.stop_reason = Some(reason);
    }

    pub fn add_line_breakpoint(&mut self, file: &str, line: usize) {
        self.add_line_breakpoint_ex(file, line, None, None);
    }

    pub fn add_line_breakpoint_ex(
        &mut self,
        file: &str,
        line: usize,
        condition: Option<String>,
        log: Option<String>,
    ) {
        let key = if file.is_empty() {
            (String::new(), line)
        } else {
            (normalize_path(file), line)
        };
        self.line_breakpoints
            .insert(key, LineBreakpoint { condition, log });
    }

    pub fn remove_line_breakpoint(&mut self, file: &str, line: usize) -> bool {
        let key = if file.is_empty() {
            (String::new(), line)
        } else {
            (normalize_path(file), line)
        };
        self.line_breakpoints.remove(&key).is_some()
    }

    pub fn clear_breakpoints(&mut self) {
        self.line_breakpoints.clear();
        self.function_breakpoints.clear();
        self.armed_func_bp = None;
    }

    #[must_use]
    pub fn is_uncaught_stop(&self) -> bool {
        self.stop_reason == Some(StopReason::Uncaught) && self.last_uncaught.is_some()
    }
}

#[must_use]
pub fn normalize_path(p: &str) -> String {
    if p.is_empty() || p == "<test>" || p == "<script>" || p == "<dbg>" || p.starts_with('<') {
        return p.replace('\\', "/");
    }
    let path = Path::new(p);
    path.canonicalize()
        .unwrap_or_else(|_| PathBuf::from(p))
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn attach(vm: &mut Vm, state: Shared<DebugState>) {
    vm.debug = Some(state);
    vm.debug_active = true;
    // 强制当前热循环退出，以便重新进入时读取新 debug_active。
    vm.force_debug_recheck();
}

/// 当前执行位置的文件 / 源文本（优先函数定义处）。
pub fn current_location(vm: &Vm) -> (String, Option<Arc<str>>) {
    if let Some(f) = vm.func_stack.last() {
        if !f.source_file.is_empty() && f.source_file != "<script>" {
            return (f.source_file.clone(), f.source.clone());
        }
        if f.source.is_some() {
            return (f.source_file.clone(), f.source.clone());
        }
    }
    (vm.source_file.clone(), vm.current_source.clone())
}

fn paths_match(bp_file: &str, actual: &str) -> bool {
    if bp_file.is_empty() {
        return true;
    }
    let a = normalize_path(actual);
    let b = normalize_path(bp_file);
    a == b || a.ends_with(&b) || b.ends_with(&a)
}

fn find_line_bp<'a>(state: &'a DebugState, file: &str, line: usize) -> Option<&'a LineBreakpoint> {
    if line == 0 {
        return None;
    }
    state
        .line_breakpoints
        .iter()
        .find(|((f, l), _)| *l == line && paths_match(f, file))
        .map(|(_, bp)| bp)
}

fn func_bp_matches(bp: &str, name: &str) -> bool {
    if let Some(sub) = bp.strip_prefix('*') {
        !sub.is_empty() && name.contains(sub)
    } else {
        name == bp || name.ends_with(bp) || name.split('$').next() == Some(bp)
    }
}

fn fiber_focus_allows(vm: &Vm, state: &DebugState) -> bool {
    match &state.step_focus {
        StepFocus::All => true,
        StepFocus::Main => vm.debug_current_task().is_none(),
        StepFocus::Fiber(focus) => vm
            .debug_current_task()
            .as_ref()
            .is_some_and(|t| Shared::ptr_eq(t, focus)),
    }
}

/// 在即将执行 `pc` 处检查是否应暂停。
pub fn should_pause(vm: &mut Vm, state: &mut DebugState) -> bool {
    if state.pending_break {
        return true;
    }
    if !state.started {
        state.started = true;
        if state.stop_on_entry {
            state.stop_reason = Some(StopReason::Entry);
            return true;
        }
    }

    // 跨纤程步进：非焦点纤程忽略行/步停（显式 breakpoint()/pending 仍停）。
    let focus_ok = fiber_focus_allows(vm, state);

    let (file_raw, _) = current_location(vm);
    let file = normalize_path(&file_raw);
    let line = line_at_pc(vm);
    let pc = vm.pc;
    let depth = vm.debug_call_depth();
    let func_name = vm.debug_current_func_name();

    // 同一指令不重断
    if let Some((lf, ll, lp)) = &state.last_stop {
        if *lf == file && *ll == line && *lp == pc {
            return false;
        }
    }

    // 离开上次停下的行 → 解除行断点抑制（让循环下一轮能再断）
    if let Some((sf, sl)) = &state.bp_skip_line {
        let left = *sf != file || *sl != line;
        if left {
            state.bp_skip_line = None;
        }
    }
    // 也用 last_visited 兜底：只要执行过别的行就解除
    if let Some((vf, vl)) = &state.last_visited {
        if let Some((sf, sl)) = &state.bp_skip_line {
            if *sf != *vf || *sl != *vl {
                state.bp_skip_line = None;
            }
        }
    }
    state.last_visited = Some((file.clone(), line));

    let same_line_as_last_stop = state
        .last_stop
        .as_ref()
        .is_some_and(|(lf, ll, _)| *lf == file && *ll == line);

    // 离开武装函数后解除
    if let Some(armed) = &state.armed_func_bp {
        let still = func_name
            .as_ref()
            .is_some_and(|n| func_bp_matches(armed, n));
        if !still {
            state.armed_func_bp = None;
        }
    }

    let entered_func = match (&state.last_func_name, &func_name) {
        (Some(prev), Some(cur)) => prev != cur,
        (None, Some(_)) => true,
        _ => false,
    };

    if focus_ok {
        // 1) 指令级单步：下一 PC 即停
        if matches!(state.step, Some(StepMode::Insn)) {
            state.stop_reason = Some(StopReason::Step);
            state.step = None;
            state.last_func_name = func_name;
            return true;
        }

        // 2) 行级单步
        if let Some(mode) = state.step {
            let hit = match mode {
                StepMode::In => line > 0 && !same_line_as_last_stop,
                StepMode::Over { max_depth } => {
                    line > 0 && depth <= max_depth && !same_line_as_last_stop
                }
                StepMode::Out { target_depth } => depth < target_depth,
                StepMode::Insn => false,
            };
            if hit {
                state.stop_reason = Some(StopReason::Step);
                state.step = None;
                state.last_func_name = func_name;
                return true;
            }
        }

        // 3) 行断点：条件 / 日志
        if let Some(bp) = find_line_bp(state, &file, line) {
            if state.bp_skip_line.is_none() {
                let cond = bp.condition.clone();
                let log = bp.log.clone();
                if let Some(log_expr) = log {
                    match eval_in_paused_vm(vm, &log_expr) {
                        Ok(v) => println!("[blog {}:{}] {}", file, line, v.display_string()),
                        Err(e) => println!("[blog {}:{}] error: {}", file, line, e.message()),
                    }
                    // 纯日志断点：打印后继续（若同时有条件则仍可停）
                    if cond.is_none() {
                        state.bp_skip_line = Some((file.clone(), line));
                        state.last_func_name = func_name;
                        return false;
                    }
                }
                let cond_ok = if let Some(c) = cond {
                    match eval_in_paused_vm(vm, &c) {
                        Ok(v) => v.is_truthy(),
                        Err(e) => {
                            println!("[break {}:{}] condition error: {}", file, line, e.message());
                            false
                        }
                    }
                } else {
                    true
                };
                if cond_ok {
                    state.stop_reason = Some(StopReason::Breakpoint);
                    state.bp_skip_line = Some((file.clone(), line));
                    state.last_func_name = func_name;
                    return true;
                }
            }
        }

        // 4) 函数断点：刚进入匹配函数时停一次
        if entered_func && state.armed_func_bp.is_none() {
            if let Some(name) = &func_name {
                if let Some(bp) = state
                    .function_breakpoints
                    .iter()
                    .find(|bp| func_bp_matches(bp, name))
                    .cloned()
                {
                    state.armed_func_bp = Some(bp);
                    state.stop_reason = Some(StopReason::Breakpoint);
                    state.last_func_name.clone_from(&func_name);
                    return true;
                }
            }
        }
    }

    state.last_func_name = func_name;
    false
}

/// 即将执行的指令对应源码行（`line_map[pc]`）。
pub fn line_at_pc(vm: &Vm) -> usize {
    vm.active_line_map
        .get(vm.pc)
        .copied()
        .or_else(|| vm.active_line_map.last().copied())
        .unwrap_or(0)
}

pub fn column_at_pc(vm: &Vm) -> usize {
    vm.active_column_map
        .get(vm.pc)
        .copied()
        .or_else(|| vm.active_column_map.last().copied())
        .unwrap_or(1)
}

pub fn mark_stopped(vm: &Vm, state: &mut DebugState) {
    let (file_raw, _) = current_location(vm);
    let file = normalize_path(&file_raw);
    let line = line_at_pc(vm);
    state.last_stop = Some((file, line, vm.pc));
    state.pending_break = false;
}

pub struct FiberInfo {
    pub index: usize,
    pub state: String,
    pub detail: String,
    pub task: Shared<TaskInner>,
}

pub fn list_fibers(vm: &Vm) -> Vec<FiberInfo> {
    let mut out = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let mut push_task = |task: &Shared<TaskInner>| {
        let addr = task.as_ptr() as usize;
        if !seen.insert(addr) {
            return;
        }
        let st = task.borrow().state.clone();
        let (name, detail) = match &st {
            TaskState::Pending { callable, args } => (
                "pending".into(),
                format!("{} argc={}", callable_name(callable), args.len()),
            ),
            TaskState::Running => ("running".into(), String::new()),
            TaskState::Suspended => ("suspended".into(), String::new()),
            TaskState::Done(v) => ("done".into(), v.display_string()),
            TaskState::Failed(e) => ("failed".into(), e.display_string()),
        };
        let index = out.len();
        out.push(FiberInfo {
            index,
            state: name,
            detail,
            task: task.clone(),
        });
    };
    for task in &vm.ready_tasks {
        push_task(task);
    }
    for v in vm.mn.scheduled_task_values() {
        if let Value::Task(task) = v {
            push_task(&task);
        }
    }
    out
}

fn callable_name(v: &Value) -> String {
    match v {
        Value::Function(f) => f.name.clone(),
        Value::Builtin(_) => "<builtin>".into(),
        other => other.type_name_string(),
    }
}

pub fn format_location(vm: &Vm) -> String {
    let (file, _) = current_location(vm);
    let line = line_at_pc(vm);
    let col = column_at_pc(vm);
    format!("{file}:{line}:{col}")
}

pub fn format_source_line(vm: &Vm) -> String {
    let line = line_at_pc(vm);
    if line == 0 {
        return String::new();
    }
    let (_, src) = current_location(vm);
    let text = src.as_ref().map_or("", std::convert::AsRef::as_ref);
    text.lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .to_string()
}

/// 当前行上下各 `context` 行；返回 `(行号, 是否当前, 文本)`。
pub fn format_source_window(vm: &Vm, context: usize) -> Vec<(usize, bool, String)> {
    let cur = line_at_pc(vm);
    if cur == 0 {
        return Vec::new();
    }
    let (_, src) = current_location(vm);
    let text = src.as_ref().map_or("", std::convert::AsRef::as_ref);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let start = cur.saturating_sub(context).max(1);
    let end = (cur + context).min(lines.len());
    (start..=end)
        .map(|n| {
            let text = lines.get(n - 1).copied().unwrap_or("").to_string();
            (n, n == cur, text)
        })
        .collect()
}

#[must_use]
pub const fn reason_label(r: StopReason) -> &'static str {
    match r {
        StopReason::Breakpoint => "breakpoint",
        StopReason::Step => "step",
        StopReason::Explicit => "breakpoint()",
        StopReason::Uncaught => "uncaught exception",
        StopReason::Entry => "entry",
    }
}

pub fn stack_frames(vm: &Vm) -> Vec<ErrorStackFrame> {
    vm.debug_build_stack_frames()
}

/// 在当前 VM 上下文中求值表达式源码（调试器用）。
///
/// 把当前可见的局部名作为词法绑定注入到 snippet，使其能引用 `i` / `total` 等。
pub fn eval_in_paused_vm(vm: &mut Vm, expr_src: &str) -> Result<Value> {
    let budget = std::env::var("OPTIVE_DEBUG_EVAL_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(100_000);
    eval_in_paused_vm_with_budget(vm, expr_src, budget)
}

pub fn eval_in_paused_vm_with_budget(
    vm: &mut Vm,
    expr_src: &str,
    instruction_budget: usize,
) -> Result<Value> {
    let wrapped = format!("{expr_src}\n");
    let program = crate::parser::Parser::parse(&wrapped).map_err(|e| {
        RuntimeError::msg(crate::diagnostics::format_parse_error(
            &wrapped, "<dbg>", &e,
        ))
    })?;

    // 收集当前可见的局部名（快局部 + 词法作用域），作为 snippet 的词法绑定。
    let names = vm.debug_visible_local_names();

    let compiled = crate::codegen::Generator::compile_snippet(&program, &names)?;
    vm.eval_debug_snippet(&names, compiled, instruction_budget)
}

pub fn set_local_or_global(vm: &mut Vm, name: &str, value: Value) -> Result<()> {
    if vm.debug_store_local(name, value.clone()) {
        return Ok(());
    }
    vm.store_global_by_name(name, value);
    Ok(())
}

/// `set` 支持简单名或深路径（`a.b` / `a[i]`）：后者整句求值赋值。
pub fn debug_set(vm: &mut Vm, lhs: &str, expr: &str) -> Result<()> {
    let lhs = lhs.trim();
    let expr = expr.trim();
    if lhs.contains('.') || lhs.contains('[') {
        let stmt = format!("{lhs} = {expr}");
        eval_in_paused_vm(vm, &stmt)?;
        Ok(())
    } else {
        let v = eval_in_paused_vm(vm, expr)?;
        set_local_or_global(vm, lhs, v)
    }
}

pub fn list_locals(vm: &Vm) -> Vec<(String, Value)> {
    vm.debug_list_locals()
}

pub fn list_globals(vm: &Vm) -> Vec<(String, Value)> {
    vm.debug_list_globals()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod eval_budget_tests {
    use super::*;

    #[test]
    fn debug_evaluate_has_hard_instruction_budget() {
        let mut vm = Vm::with_workers(1);
        let err = eval_in_paused_vm_with_budget(&mut vm, "loop { }", 16).unwrap_err();
        assert!(
            err.message().contains("instruction budget exceeded"),
            "{err}"
        );
        assert_eq!(
            eval_in_paused_vm_with_budget(&mut vm, "1 + 2", 100)
                .unwrap()
                .display_string(),
            "3"
        );
    }
}

fn break_spec_next_kw(spec: &str) -> Option<usize> {
    let if_at = spec.find(" if ");
    let log_at = spec.find(" log ");
    match (if_at, log_at) {
        (Some(if_pos), Some(log_pos)) => Some(if_pos.min(log_pos)),
        (Some(if_pos), None) => Some(if_pos),
        (None, Some(log_pos)) => Some(log_pos),
        (None, None) => None,
    }
}

fn take_break_clause<'a>(trail: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    let rest = trail.strip_prefix(kw)?;
    match break_spec_next_kw(rest) {
        Some(e) => Some((rest[..e].trim(), rest[e..].trim())),
        None => Some((rest.trim(), "")),
    }
}

/// 解析 `break` 规格：`[file:]N [if <expr>] [log <expr>]`（顺序不限，可同时出现）。
#[must_use]
pub fn parse_break_spec(spec: &str) -> Option<(String, usize, Option<String>, Option<String>)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let (loc_part, mut trail) = match break_spec_next_kw(spec) {
        Some(c) => (spec[..c].trim(), spec[c..].trim()),
        None => (spec, ""),
    };
    let mut condition = None;
    let mut log = None;
    while !trail.is_empty() {
        if let Some((expr, after)) = take_break_clause(trail, "if ") {
            if expr.is_empty() || condition.is_some() {
                return None;
            }
            condition = Some(expr.to_string());
            trail = after;
        } else if let Some((expr, after)) = take_break_clause(trail, "log ") {
            if expr.is_empty() || log.is_some() {
                return None;
            }
            log = Some(expr.to_string());
            trail = after;
        } else {
            return None;
        }
    }
    let (file, line) = if let Ok(line) = loc_part.parse::<usize>() {
        (String::new(), line)
    } else {
        let (file_part, line_s) = loc_part.rsplit_once(':')?;
        (file_part.to_string(), line_s.parse().ok()?)
    };
    Some((file, line, condition, log))
}
