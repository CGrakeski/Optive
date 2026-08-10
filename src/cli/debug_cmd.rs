//! `Optive debug`：交互式调试器 REPL。

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use optive::codegen::Generator;
use optive::shared::Shared;
use optive::debug::{
    self, debug_set, eval_in_paused_vm, format_location, format_source_window, list_fibers,
    list_locals, parse_break_spec, reason_label, stack_frames, DebugState, StepFocus, StepMode,
    StopReason,
};
use optive::diagnostics;
use optive::opcode::Instruction;
use optive::parser::Parser;
use optive::value::Value;
use optive::vm::{DepPackage, Vm};

use super::color;
use super::lock::ROOT_PARENT;
use super::resolve::{DepBinding, EnsureResult};

pub fn cmd_debug(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (caps, rest) = super::caps::parse_caps(args)?;
    // `Optive debug [path]`：无参或目录 → 项目入口；`.tive` → 单文件。
    let target = rest.first().map(Path::new);
    match target {
        Some(p) if p.extension().and_then(|e| e.to_str()) == Some("tive") || p.is_file() => {
            debug_script_file(p, caps)
        }
        _ => debug_project(target, caps),
    }
}

fn debug_project(path: Option<&Path>, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let project = super::manifest::find_project(path)?;
    color::status_line(&format!("Debug project {}", project.root.display()));
    let ensured = super::deps::ensure_for_run(&project)?;
    std::env::set_current_dir(&project.root)?;
    let entry = project.entry_path()?;
    let source = fs::read_to_string(&entry)
        .map_err(|e| format!("cannot read {}: {e}", entry.display()))?;
    let file = entry.to_string_lossy().to_string();
    let mut vm = Vm::new();
    vm.caps = caps;
    inject_dep_map(&mut vm, &ensured, &project.root);
    run_debug_session(&mut vm, &source, &file)
}

fn debug_script_file(path: &Path, caps: optive::caps::Capabilities) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = path.to_string_lossy().to_string();
    let mut vm = Vm::new();
    vm.caps = caps;
    run_debug_session(&mut vm, &source, &file)
}

pub fn inject_dep_map(vm: &mut Vm, ensured: &EnsureResult, project_root: &Path) {
    vm.dep_map.clear();
    for ((parent, name), DepBinding { path, id }) in &ensured.dep_map {
        vm.dep_map.insert(
            (parent.clone(), name.clone()),
            DepPackage {
                path: path.clone(),
                id: id.clone(),
            },
        );
    }
    vm.current_package_id = ROOT_PARENT.to_string();
    vm.package_root = Some(project_root.to_path_buf());
}

fn run_debug_session(
    vm: &mut Vm,
    source: &str,
    file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = Shared::new(DebugState::default());
    debug::attach(vm, state.clone());

    vm.source_file = file.to_string();
    vm.current_source = Some(Arc::from(source));
    if let Some(parent) = Path::new(file).parent() {
        if !parent.as_os_str().is_empty() {
            vm.import_base = parent.to_path_buf();
        }
    }
    let program = Parser::parse(source).map_err(|e| {
        diagnostics::format_parse_error(source, file, &e)
    })?;
    let mut compiled = Generator::new().compile(&program)?;
    diagnostics::attach_function_sources(&mut compiled, source, file);
    vm.load_program(compiled)?;

    println!("Optive debugger — type `help` for commands");
    println!("(stop on entry)");

    let mut focus_frame: Option<usize> = None;
    let mut finished = false;
    let mut last_value: Option<Value> = None;

    // 先跑到入口停
    match vm.run_until_debug_break() {
        Ok(Some(v)) => {
            finished = true;
            last_value = Some(v);
        }
        Ok(None) => {
            print_stop(vm, &state.borrow());
        }
        Err(e) => return Err(e.to_string().into()),
    }

    let stdin = io::stdin();

    // --- 宏定义 ---
    macro_rules! resume_continue {
        ($vm:expr, $state:expr, $finished:ident, $last_value:ident) => {{
            if $state.borrow().is_uncaught_stop() {
                println!("[session ended after uncaught exception]");
                $finished = true;
                continue;
            }
            $state.borrow_mut().step = None;
            match resume($vm)? {
                Resume::Paused => print_stop($vm, &$state.borrow()),
                Resume::Done(v) => {
                    $finished = true;
                    $last_value = Some(v);
                }
            }
        }};
    }

    macro_rules! resume_step {
        ($vm:expr, $state:expr, $step:expr, $finished:ident, $last_value:ident, $on_paused:block) => {{
            if $state.borrow().is_uncaught_stop() {
                println!("cannot step after uncaught exception; use quit");
                continue;
            }
            $state.borrow_mut().step = Some($step);
            match resume($vm)? {
                Resume::Paused => $on_paused,
                Resume::Done(v) => {
                    $finished = true;
                    $last_value = Some(v);
                }
            }
        }};
    }


    loop {
        if finished {
            if let Some(v) = &last_value {
                if !matches!(v, Value::None) {
                    println!("[program finished] {}", v.display_string());
                } else {
                    println!("[program finished]");
                }
            }
            break;
        }

        print!("(dbg) ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            println!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();

        match cmd {
            "h" | "help" | "?" => print_help(),
            "q" | "quit" | "exit" => break,

            "c" | "continue" | "cont" => {
                resume_continue!(vm, state, finished, last_value);
            }

            "s" | "step" => {
                resume_step!(vm, state, StepMode::In, finished, last_value, {
                    print_stop(vm, &state.borrow());
                });
            }

            "si" | "stepi" => {
                resume_step!(vm, state, StepMode::Insn, finished, last_value, {
                    print_stop(vm, &state.borrow());
                    print_disasm_here(vm);
                });
            }

            "n" | "next" => {
                let depth = vm.debug_call_depth();
                resume_step!(vm, state, StepMode::Over { max_depth: depth }, finished, last_value, {
                    print_stop(vm, &state.borrow());
                });
            }

            "finish" | "out" => {
                let depth = vm.debug_call_depth();
                resume_step!(vm, state, StepMode::Out { target_depth: depth }, finished, last_value, {
                    print_stop(vm, &state.borrow());
                });
            }

            "b" | "break" => {
                if rest.is_empty() {
                    let st = state.borrow();
                    if st.line_breakpoints.is_empty() && st.function_breakpoints.is_empty() {
                        println!("(no breakpoints)");
                    } else {
                        for ((f, l), bp) in &st.line_breakpoints {
                            let mut extra = String::new();
                            if let Some(c) = &bp.condition {
                                extra.push_str(&format!(" if {c}"));
                            }
                            if let Some(lg) = &bp.log {
                                extra.push_str(&format!(" log {lg}"));
                            }
                            if f.is_empty() {
                                println!("  line {l}{extra}");
                            } else {
                                println!("  {f}:{l}{extra}");
                            }
                        }
                        for name in &st.function_breakpoints {
                            println!("  func {name}");
                        }
                    }
                } else {
                    let spec = rest.join(" ");
                    if let Some((file, line, cond, log)) = parse_break_spec(&spec) {
                        state
                            .borrow_mut()
                            .add_line_breakpoint_ex(&file, line, cond.clone(), log.clone());
                        let mut msg = if file.is_empty() {
                            format!("Breakpoint at line {line}")
                        } else {
                            format!("Breakpoint at {file}:{line}")
                        };
                        if let Some(c) = cond {
                            msg.push_str(&format!(" if {c}"));
                        }
                        if let Some(lg) = log {
                            msg.push_str(&format!(" log {lg}"));
                        }
                        println!("{msg}");
                    } else {
                        state.borrow_mut().function_breakpoints.insert(spec.clone());
                        println!("Function breakpoint on {spec}");
                    }
                }
            }

            "d" | "delete" | "clear" => {
                if rest.is_empty() {
                    state.borrow_mut().clear_breakpoints();
                    println!("Cleared all breakpoints");
                } else {
                    let spec = rest.join(" ");
                    if let Ok(line) = spec.parse::<usize>() {
                        if state.borrow_mut().remove_line_breakpoint("", line) {
                            println!("Deleted breakpoint at line {line}");
                        } else {
                            println!("No breakpoint at line {line}");
                        }
                    } else if let Some((file_part, line_s)) = spec.rsplit_once(':') {
                        if let Ok(line) = line_s.parse::<usize>() {
                            if state.borrow_mut().remove_line_breakpoint(file_part, line) {
                                println!("Deleted breakpoint at {file_part}:{line}");
                            } else {
                                println!("No breakpoint at {file_part}:{line}");
                            }
                        } else if state.borrow_mut().function_breakpoints.remove(&spec) {
                            println!("Deleted function breakpoint {spec}");
                        } else {
                            println!("No such breakpoint");
                        }
                    } else if state.borrow_mut().function_breakpoints.remove(&spec) {
                        println!("Deleted function breakpoint {spec}");
                    } else {
                        println!("No such breakpoint");
                    }
                }
            }

            "bt" | "backtrace" | "where" => {
                let frames = stack_frames(vm);
                for (i, fr) in frames.iter().enumerate().rev() {
                    let marker = match focus_frame {
                        Some(f) if f == i => ">",
                        None if i + 1 == frames.len() => ">",
                        _ => " ",
                    };
                    println!(
                        "{marker}#{i} {} at {}:{}:{}",
                        fr.func, fr.file, fr.line, fr.column
                    );
                }
            }

            "frame" => {
                if rest.is_empty() {
                    let frames = stack_frames(vm);
                    let i = focus_frame.unwrap_or(frames.len().saturating_sub(1));
                    if let Some(fr) = frames.get(i) {
                        println!("#{i} {} at {}:{}", fr.func, fr.file, fr.line);
                    }
                } else if let Ok(n) = rest[0].parse::<usize>() {
                    let frames = stack_frames(vm);
                    if n < frames.len() {
                        focus_frame = Some(n);
                        let fr = &frames[n];
                        println!("#{n} {} at {}:{}", fr.func, fr.file, fr.line);
                    } else {
                        println!("frame out of range");
                    }
                }
            }

            "p" | "print" | "eval" => {
                let expr = if cmd == "eval" || cmd == "print" || cmd == "p" {
                    line.split_once(char::is_whitespace)
                        .map(|(_, r)| r.trim())
                        .unwrap_or("")
                } else {
                    ""
                };
                if expr.is_empty() {
                    println!("usage: p <expr>");
                } else {
                    match eval_in_paused_vm(vm, expr) {
                        Ok(v) => println!("{}", v.display_string()),
                        Err(e) => println!("error: {}", e.message()),
                    }
                }
            }

            "locals" => {
                for (name, val) in list_locals(vm) {
                    println!("  {name} = {}", val.display_string());
                }
            }

            "globals" => {
                let mut names = vm.globals.keys();
                names.sort();
                for name in names {
                    if name.starts_with("__") {
                        continue;
                    }
                    if let Some(v) = vm.globals.get(&name) {
                        println!("  {name} = {}", v.display_string());
                    }
                }
            }

            "set" => {
                let body = line.strip_prefix("set").unwrap_or("").trim();
                if let Some((lhs, expr)) = body.split_once('=') {
                    match debug_set(vm, lhs, expr) {
                        Ok(()) => println!("ok"),
                        Err(e) => println!("error: {}", e.message()),
                    }
                } else {
                    println!("usage: set name = expr   or   set a.b = expr / set a[i] = expr");
                }
            }

            "fibers" => {
                let list = list_fibers(vm);
                let st = state.borrow();
                let focus_idx = st.focus_fiber_index;
                if list.is_empty() {
                    println!("(no tasks)");
                } else {
                    for f in &list {
                        let mark = if focus_idx == Some(f.index) { "*" } else { " " };
                        if f.detail.is_empty() {
                            println!("{mark}#{} {}", f.index, f.state);
                        } else {
                            println!("{mark}#{} {} — {}", f.index, f.state, f.detail);
                        }
                    }
                }
                let label = match &st.step_focus {
                    StepFocus::All => "all (default)".to_string(),
                    StepFocus::Main => "main".to_string(),
                    StepFocus::Fiber(_) => match focus_idx {
                        Some(i) => format!("fiber #{i}"),
                        None => "fiber".to_string(),
                    },
                };
                println!("  focus: {label}");
            }

            "fiber" => {
                if rest.is_empty() || rest[0] == "all" {
                    let mut st = state.borrow_mut();
                    st.step_focus = StepFocus::All;
                    st.focus_fiber_index = None;
                    println!("focus: all");
                } else if rest[0] == "main" {
                    let mut st = state.borrow_mut();
                    st.step_focus = StepFocus::Main;
                    st.focus_fiber_index = None;
                    println!("focus: main");
                } else if let Ok(n) = rest[0].parse::<usize>() {
                    let list = list_fibers(vm);
                    if let Some(f) = list.into_iter().find(|f| f.index == n) {
                        let mut st = state.borrow_mut();
                        st.step_focus = StepFocus::Fiber(f.task);
                        st.focus_fiber_index = Some(n);
                        println!("focus: fiber #{n}");
                    } else {
                        println!("fiber #{n} not found; use `fibers`");
                    }
                } else {
                    println!("usage: fiber all | fiber main | fiber N");
                }
            }

            "l" | "list" => {
                let ctx = rest
                    .first()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(3);
                let loc = format_location(vm);
                println!("  {loc}");
                for (n, is_cur, text) in format_source_window(vm, ctx) {
                    let mark = if is_cur { ">" } else { " " };
                    println!("{mark}{n:>5} | {text}");
                }
            }

            "disasm" => print_disasm_here(vm),
            other => println!("unknown command: {other} (try help)"),
        }
    }
    Ok(())
}

enum Resume {
    Paused,
    Done(Value),
}

fn resume(vm: &mut Vm) -> Result<Resume, Box<dyn std::error::Error>> {
    match vm.run_until_debug_break() {
        Ok(Some(v)) => Ok(Resume::Done(v)),
        Ok(None) => Ok(Resume::Paused),
        Err(e) => Err(e.to_string().into()),
    }
}

fn print_stop(vm: &Vm, state: &DebugState) {
    let reason = state.stop_reason.map(reason_label).unwrap_or("paused");
    println!("Stopped ({reason}) at {}", format_location(vm));
    for (n, is_cur, text) in format_source_window(vm, 1) {
        if is_cur {
            println!("{n:>6} | {text}");
        }
    }
    if state.stop_reason == Some(StopReason::Uncaught) {
        if let Some(msg) = &state.last_uncaught {
            println!("{msg}");
            println!("(inspect with bt/locals/p; continue or quit to end session)");
        }
    }
}

fn print_disasm_here(vm: &Vm) {
    let pc = vm.pc;
    let start = pc.saturating_sub(2);
    let end = (pc + 6).min(vm.code.len());
    for i in start..end {
        let mark = if i == pc { "->" } else { "  " };
        let line = vm.active_line_map.get(i).copied().unwrap_or(0);
        let col = vm.active_column_map.get(i).copied().unwrap_or(1);
        let op = &vm.code[i];
        println!("{mark} {i:>4}  L{line}:{col}  {}", format_insn(op));
    }
}

fn format_insn(op: &Instruction) -> String {
    format!("{op:?}")
}

fn print_help() {
    println!(
        "Commands:
  help / h              Show this help
  break / b [file:]N    Set or list line breakpoints
  break N if <expr>     Conditional breakpoint (stop when expr truthy)
  break N log <expr>    Log breakpoint (print expr, continue)
  break name            Function breakpoint (exact / suffix; *sub for contains)
  delete / d [file:]N   Delete line or function breakpoint (no arg: clear all)
  continue / c          Continue (after uncaught: end session)
  step / s              Step into (source line)
  next / n              Step over
  finish / out          Run until current frame returns
  stepi / si            Step one bytecode instruction (+ disasm)
  disasm                Disassemble around PC
  bt / backtrace        Call stack
  frame [N]             Select / show frame (display)
  p / print / eval E    Evaluate expression
  locals / globals      List variables
  set name = expr       Set local/global or deep path (a.b / a[i])
  fibers                List tasks (* = step focus)
  fiber all|main|N      Break/step focus (default: all)
  list / l [N]          Show source ±N lines (default 3)
  quit / q              Exit debugger"
    );
}
