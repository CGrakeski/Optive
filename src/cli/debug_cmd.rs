//! `Optive debug`：交互式调试器 REPL。

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use optive::codegen::Generator;
use optive::debug::{
    self, eval_in_paused_vm, format_location, format_source_window, list_fibers, list_locals,
    reason_label, set_local_or_global, stack_frames, DebugState, StepMode, StopReason,
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

fn inject_dep_map(vm: &mut Vm, ensured: &EnsureResult, project_root: &Path) {
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
    let state = Rc::new(RefCell::new(DebugState::default()));
    debug::attach(vm, state.clone());

    vm.source_file = file.to_string();
    vm.current_source = Some(std::rc::Rc::from(source));
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
                if state.borrow().is_uncaught_stop() {
                    println!("[session ended after uncaught exception]");
                    finished = true;
                    continue;
                }
                state.borrow_mut().step = None;
                match resume(vm)? {
                    Resume::Paused => print_stop(vm, &state.borrow()),
                    Resume::Done(v) => {
                        finished = true;
                        last_value = Some(v);
                    }
                }
            }
            "s" | "step" => {
                if state.borrow().is_uncaught_stop() {
                    println!("cannot step after uncaught exception; use quit");
                    continue;
                }
                state.borrow_mut().step = Some(StepMode::In);
                match resume(vm)? {
                    Resume::Paused => print_stop(vm, &state.borrow()),
                    Resume::Done(v) => {
                        finished = true;
                        last_value = Some(v);
                    }
                }
            }
            "si" | "stepi" => {
                if state.borrow().is_uncaught_stop() {
                    println!("cannot step after uncaught exception; use quit");
                    continue;
                }
                state.borrow_mut().step = Some(StepMode::Insn);
                match resume(vm)? {
                    Resume::Paused => {
                        print_stop(vm, &state.borrow());
                        print_disasm_here(vm);
                    }
                    Resume::Done(v) => {
                        finished = true;
                        last_value = Some(v);
                    }
                }
            }
            "n" | "next" => {
                if state.borrow().is_uncaught_stop() {
                    println!("cannot step after uncaught exception; use quit");
                    continue;
                }
                let depth = vm.debug_call_depth();
                state.borrow_mut().step = Some(StepMode::Over { max_depth: depth });
                match resume(vm)? {
                    Resume::Paused => print_stop(vm, &state.borrow()),
                    Resume::Done(v) => {
                        finished = true;
                        last_value = Some(v);
                    }
                }
            }
            "finish" | "out" => {
                if state.borrow().is_uncaught_stop() {
                    println!("cannot step after uncaught exception; use quit");
                    continue;
                }
                let depth = vm.debug_call_depth();
                state.borrow_mut().step = Some(StepMode::Out {
                    target_depth: depth,
                });
                match resume(vm)? {
                    Resume::Paused => print_stop(vm, &state.borrow()),
                    Resume::Done(v) => {
                        finished = true;
                        last_value = Some(v);
                    }
                }
            }
            "b" | "break" => {
                if rest.is_empty() {
                    let st = state.borrow();
                    if st.line_breakpoints.is_empty() && st.function_breakpoints.is_empty() {
                        println!("(no breakpoints)");
                    } else {
                        for (f, l) in &st.line_breakpoints {
                            if f.is_empty() {
                                println!("  line {l}");
                            } else {
                                println!("  {f}:{l}");
                            }
                        }
                        for name in &st.function_breakpoints {
                            println!("  func {name}");
                        }
                    }
                } else {
                    let spec = rest.join(" ");
                    if let Ok(line) = spec.parse::<usize>() {
                        state.borrow_mut().add_line_breakpoint("", line);
                        println!("Breakpoint at line {line}");
                    } else if let Some((file_part, line_s)) = spec.rsplit_once(':') {
                        if let Ok(line) = line_s.parse::<usize>() {
                            state.borrow_mut().add_line_breakpoint(file_part, line);
                            println!("Breakpoint at {file_part}:{line}");
                        } else {
                            state.borrow_mut().function_breakpoints.insert(spec.clone());
                            println!("Function breakpoint on {spec}");
                        }
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
                let mut names: Vec<_> = vm.globals.keys().cloned().collect();
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
                // set name = expr
                let body = line.strip_prefix("set").unwrap_or("").trim();
                if let Some((name, expr)) = body.split_once('=') {
                    let name = name.trim();
                    let expr = expr.trim();
                    match eval_in_paused_vm(vm, expr) {
                        Ok(v) => match set_local_or_global(vm, name, v) {
                            Ok(()) => println!("ok"),
                            Err(e) => println!("error: {}", e.message()),
                        },
                        Err(e) => println!("error: {}", e.message()),
                    }
                } else {
                    println!("usage: set name = expr");
                }
            }
            "fibers" => {
                let list = list_fibers(vm);
                if list.is_empty() {
                    println!("(no ready tasks)");
                } else {
                    for f in list {
                        if f.detail.is_empty() {
                            println!("  #{} {}", f.index, f.state);
                        } else {
                            println!("  #{} {} — {}", f.index, f.state, f.detail);
                        }
                    }
                }
            }
            "fiber" => {
                println!("fiber switch for stepping is deferred; use `fibers` to inspect");
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
  set name = expr       Set local or global
  fibers                List ready tasks
  list / l [N]          Show source ±N lines (default 3)
  quit / q              Exit debugger"
    );
}
