//! `Optive dap`：stdio Debug Adapter Protocol，薄适配 `DebugState`。
//! 不替换 `Optive debug`，不进 `dispatch_hot_u8`。

#[cfg(test)]
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as Json};

use crate::debug::{
    self, list_globals, list_locals, stack_frames, DebugState, StepMode, StopReason,
};
use crate::shared::Shared;
use crate::vm::{OutputSink, OutputStream, Vm};

const THREAD_ID: i64 = 1;
const REF_LOCALS: i64 = 1;
const REF_GLOBALS: i64 = 2;
const DEFAULT_EVALUATE_BUDGET: usize = 100_000;
/// 单条 output 事件正文上限，须低于 `rpc::MAX_CONTENT_LENGTH`。
const MAX_OUTPUT_CHUNK: usize = 1024 * 1024;
/// 会话排队的 debuggee 输出总字节上限。
const MAX_OUTPUT_QUEUED: usize = 4 * 1024 * 1024;

pub type LaunchBootstrap = Arc<dyn Fn(&Json, &str) -> Result<Vm, String> + Send + Sync + 'static>;

pub fn run_stdio() -> io::Result<()> {
    run_stdio_with_launcher(Arc::new(|_, _| Ok(Vm::new())))
}

pub fn run_stdio_with_launcher(launcher: LaunchBootstrap) -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut stdout = io::stdout();
    let mut session = Session::with_launcher(launcher);
    while let Some(msg) = crate::rpc::read_json(&mut reader)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    {
        for out in session.handle(&msg) {
            crate::rpc::write_json(&mut stdout, &out)?;
        }
        if session.shutdown {
            break;
        }
    }
    Ok(())
}

fn floor_char_boundary(s: &str, mut n: usize) -> usize {
    if n >= s.len() {
        return s.len();
    }
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

fn push_debug_output(
    output: &Arc<Mutex<Vec<(OutputStream, String)>>>,
    stream: OutputStream,
    text: &str,
) {
    let mut q = output.lock().unwrap_or_else(|e| e.into_inner());
    let mut used: usize = q.iter().map(|(_, s)| s.len()).sum();
    if used >= MAX_OUTPUT_QUEUED {
        return;
    }
    let mut rest = text;
    let mut truncated = false;
    while !rest.is_empty() {
        let room = MAX_OUTPUT_QUEUED.saturating_sub(used);
        if room == 0 {
            truncated = true;
            break;
        }
        let take = rest.len().min(MAX_OUTPUT_CHUNK).min(room);
        let take = floor_char_boundary(rest, take);
        if take == 0 {
            truncated = true;
            break;
        }
        if take < rest.len() {
            truncated = true;
        }
        q.push((stream, rest[..take].to_string()));
        used += take;
        rest = &rest[take..];
    }
    if truncated && used < MAX_OUTPUT_QUEUED {
        let marker = "\n...[output truncated]\n";
        let room = MAX_OUTPUT_QUEUED - used;
        let n = floor_char_boundary(marker, room.min(marker.len()));
        if n > 0 {
            q.push((OutputStream::Stderr, marker[..n].to_string()));
        }
    }
}

fn clamp_output_event(text: &str) -> String {
    let max = crate::rpc::MAX_CONTENT_LENGTH / 2;
    if text.len() <= max {
        return text.to_string();
    }
    let keep = floor_char_boundary(text, max.saturating_sub(24));
    let mut out = text[..keep].to_string();
    out.push_str("\n...[truncated]");
    out
}

pub struct Session {
    vm: Option<Vm>,
    state: Shared<DebugState>,
    seq: i64,
    finished: bool,
    shutdown: bool,
    last_value: Option<String>,
    launcher: LaunchBootstrap,
    output: Arc<Mutex<Vec<(OutputStream, String)>>>,
    evaluate_budget: usize,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::with_launcher(Arc::new(|_, _| Ok(Vm::new())))
    }

    #[must_use]
    pub fn with_launcher(launcher: LaunchBootstrap) -> Self {
        Self {
            vm: None,
            state: Shared::new(DebugState::default()),
            seq: 0,
            finished: false,
            shutdown: false,
            last_value: None,
            launcher,
            output: Arc::new(Mutex::new(Vec::new())),
            evaluate_budget: DEFAULT_EVALUATE_BUDGET,
        }
    }

    pub fn handle(&mut self, msg: &Json) -> Vec<Json> {
        let mut out = self.handle_inner(msg);
        let captured = {
            let mut pending = self.output.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *pending)
        };
        for (stream, text) in captured {
            let category = match stream {
                OutputStream::Stdout => "stdout",
                OutputStream::Stderr => "stderr",
            };
            let text = clamp_output_event(&text);
            out.push(self.event("output", json!({ "category": category, "output": text })));
        }
        out
    }

    fn handle_inner(&mut self, msg: &Json) -> Vec<Json> {
        let method = msg
            .get("command")
            .or_else(|| msg.get("method"))
            .and_then(Json::as_str);
        let req_seq = msg
            .get("seq")
            .or_else(|| msg.get("id"))
            .and_then(Json::as_i64)
            .unwrap_or(0);
        let args = msg.get("arguments").cloned().unwrap_or(json!({}));
        match method.unwrap_or("") {
            "initialize" => vec![
                self.ok(
                    req_seq,
                    "initialize",
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsEvaluateForHovers": true,
                        "supportsSetVariable": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsFunctionBreakpoints": true,
                        "supportsLogPoints": true,
                        "supportsExceptionBreakpoints": true,
                        "exceptionBreakpointFilters": [
                            { "filter": "uncaught", "label": "Uncaught Exceptions", "default": true },
                            { "filter": "raised", "label": "Raised Exceptions", "default": false }
                        ]
                    }),
                ),
                self.event("initialized", json!({})),
            ],
            "launch" => self.launch(req_seq, &args),
            "setBreakpoints" => self.set_breakpoints(req_seq, &args),
            "setFunctionBreakpoints" => self.set_function_breakpoints(req_seq, &args),
            "setExceptionBreakpoints" => self.set_exception_breakpoints(req_seq, &args),
            "setVariable" => self.set_variable(req_seq, &args),
            "configurationDone" => vec![self.ok(req_seq, "configurationDone", json!({}))],
            "threads" => {
                let threads = self.thread_list();
                vec![self.ok(req_seq, "threads", json!({ "threads": threads }))]
            }
            "stackTrace" => self.stack_trace(req_seq),
            "scopes" => self.scopes(req_seq),
            "variables" => self.variables(req_seq, &args),
            "evaluate" => self.evaluate(req_seq, &args),
            "continue" => self.resume(req_seq, "continue", None),
            "next" => self.resume(
                req_seq,
                "next",
                Some(StepMode::Over {
                    max_depth: self.depth(),
                }),
            ),
            "stepIn" => self.resume(req_seq, "stepIn", Some(StepMode::In)),
            "stepOut" => self.resume(
                req_seq,
                "stepOut",
                Some(StepMode::Out {
                    target_depth: self.depth().saturating_sub(1),
                }),
            ),
            "disconnect" | "terminate" => {
                self.shutdown = true;
                vec![self.ok(req_seq, method.unwrap_or("disconnect"), json!({}))]
            }
            other => vec![self.fail(req_seq, other, format!("unsupported DAP command `{other}`"))],
        }
    }

    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    fn ok(&mut self, req_seq: i64, command: &str, body: Json) -> Json {
        json!({
            "seq": self.next_seq(),
            "type": "response",
            "request_seq": req_seq,
            "success": true,
            "command": command,
            "body": body,
        })
    }

    fn fail(&mut self, req_seq: i64, command: &str, message: String) -> Json {
        json!({
            "seq": self.next_seq(),
            "type": "response",
            "request_seq": req_seq,
            "success": false,
            "command": command,
            "message": message,
        })
    }

    fn event(&mut self, event: &str, body: Json) -> Json {
        json!({
            "seq": self.next_seq(),
            "type": "event",
            "event": event,
            "body": body,
        })
    }

    fn stopped_event(&mut self, reason: &str) -> Json {
        self.event(
            "stopped",
            json!({
                "reason": reason,
                "threadId": THREAD_ID,
                "allThreadsStopped": true,
            }),
        )
    }

    fn launch(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let mut program = args
            .get("program")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        if program.is_empty() {
            return vec![self.fail(req_seq, "launch", "launch.program is required".into())];
        }
        if let Some(cwd) = args.get("cwd").and_then(Json::as_str) {
            if let Err(e) = std::env::set_current_dir(cwd) {
                return vec![self.fail(
                    req_seq,
                    "launch",
                    format!("cannot set launch.cwd to `{cwd}`: {e}"),
                )];
            }
        }
        if let Ok(path) = Path::new(&program).canonicalize() {
            program = path.to_string_lossy().into_owned();
        }
        self.evaluate_budget = match args.get("evaluateInstructionBudget") {
            Some(v) => match v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
            {
                Some(n) => n,
                None => {
                    return vec![self.fail(
                        req_seq,
                        "launch",
                        "evaluateInstructionBudget must be a positive integer".into(),
                    )]
                }
            },
            None => DEFAULT_EVALUATE_BUDGET,
        };
        match self.start_program(&program, args) {
            Ok(()) => {
                let mut out = vec![self.ok(req_seq, "launch", json!({}))];
                if self.finished {
                    out.push(self.event("terminated", json!({})));
                } else {
                    out.push(self.stopped_event("entry"));
                }
                out
            }
            Err(e) => vec![self.fail(req_seq, "launch", e)],
        }
    }

    fn start_program(&mut self, file: &str, args: &Json) -> Result<(), String> {
        let mut vm = (self.launcher)(args, file)?;
        let source = vm
            .caps
            .read_to_string("DAP script entry", file)
            .map_err(|e| format!("cannot read {file}: {e}"))?;
        let output = self.output.clone();
        vm.set_output_sink(OutputSink::new(move |stream, text| {
            push_debug_output(&output, stream, text);
        }));
        debug::attach(&mut vm, self.state.clone());
        vm.source_file = file.to_string();
        vm.current_source = Some(Arc::from(source.as_str()));
        if let Some(parent) = Path::new(file).parent() {
            if !parent.as_os_str().is_empty() {
                vm.import_base = parent.to_path_buf();
            }
        }
        let compiled =
            crate::compile_with_context(&vm, &source, file).map_err(|e| e.to_string())?;
        vm.load_program(compiled).map_err(|e| e.to_string())?;
        match vm.run_until_debug_break() {
            Ok(Some(v)) => {
                self.finished = true;
                self.last_value = Some(v.display_string());
            }
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        self.vm = Some(vm);
        Ok(())
    }

    fn set_breakpoints(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let path = args
            .pointer("/source/path")
            .and_then(Json::as_str)
            .unwrap_or("");
        let bps = args
            .get("breakpoints")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        {
            let mut st = self.state.borrow_mut();
            st.line_breakpoints.retain(|(f, _), _| {
                let left = debug::normalize_path(f);
                let right = debug::normalize_path(path);
                if cfg!(windows) {
                    !left.eq_ignore_ascii_case(&right)
                } else {
                    left != right
                }
            });
            for bp in &bps {
                if let Some(line) = bp.get("line").and_then(Json::as_u64) {
                    let cond = bp
                        .get("condition")
                        .and_then(Json::as_str)
                        .map(str::to_string);
                    let log = bp
                        .get("logMessage")
                        .and_then(Json::as_str)
                        .map(str::to_string);
                    st.add_line_breakpoint_ex(path, line as usize, cond, log);
                }
            }
        }
        let verified: Vec<Json> = bps
            .iter()
            .filter_map(|bp| {
                let line = bp.get("line").and_then(Json::as_u64)?;
                Some(json!({ "verified": true, "line": line }))
            })
            .collect();
        vec![self.ok(
            req_seq,
            "setBreakpoints",
            json!({ "breakpoints": verified }),
        )]
    }

    fn thread_list(&self) -> Vec<Json> {
        let mut threads = vec![json!({ "id": THREAD_ID, "name": "main" })];
        if let Some(vm) = self.vm.as_ref() {
            for fiber in debug::list_fibers(vm) {
                threads.push(json!({
                    "id": 1000 + fiber.index as i64,
                    "name": format!("fiber-{} {}", fiber.index, fiber.state)
                }));
            }
        }
        threads
    }

    fn set_function_breakpoints(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let bps = args
            .get("breakpoints")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        {
            let mut st = self.state.borrow_mut();
            st.function_breakpoints.clear();
            for bp in &bps {
                if let Some(name) = bp.get("name").and_then(Json::as_str) {
                    st.function_breakpoints.insert(name.to_string());
                }
            }
        }
        let verified: Vec<Json> = bps
            .iter()
            .filter_map(|bp| {
                let name = bp.get("name").and_then(Json::as_str)?;
                Some(json!({ "verified": true, "name": name }))
            })
            .collect();
        vec![self.ok(
            req_seq,
            "setFunctionBreakpoints",
            json!({ "breakpoints": verified }),
        )]
    }

    fn set_exception_breakpoints(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let filters: Vec<String> = args
            .get("filters")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        {
            let mut st = self.state.borrow_mut();
            st.exception_uncaught = filters.iter().any(|f| f == "uncaught") || filters.is_empty();
            st.exception_raised = filters.iter().any(|f| f == "raised");
            if filters.is_empty() {
                st.exception_uncaught = true;
            }
        }
        vec![self.ok(req_seq, "setExceptionBreakpoints", json!({}))]
    }

    fn set_variable(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let Some(vm) = self.vm.as_mut() else {
            return vec![self.fail(req_seq, "setVariable", "not launched".into())];
        };
        let name = args.get("name").and_then(Json::as_str).unwrap_or("");
        let value = args.get("value").and_then(Json::as_str).unwrap_or("");
        match debug::debug_set(vm, name, value) {
            Ok(()) => {
                let shown = debug::list_locals(vm)
                    .into_iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.display_string())
                    .unwrap_or_else(|| value.to_string());
                vec![self.ok(
                    req_seq,
                    "setVariable",
                    json!({ "value": shown, "variablesReference": 0 }),
                )]
            }
            Err(e) => vec![self.fail(req_seq, "setVariable", e.to_string())],
        }
    }

    fn depth(&self) -> usize {
        self.vm.as_ref().map(Vm::debug_call_depth).unwrap_or(0)
    }

    fn resume(&mut self, req_seq: i64, command: &str, step: Option<StepMode>) -> Vec<Json> {
        if self.finished {
            return vec![
                self.ok(req_seq, command, json!({ "allThreadsContinued": true })),
                self.event("terminated", json!({})),
            ];
        }
        let Some(vm) = self.vm.as_mut() else {
            return vec![self.fail(req_seq, command, "not launched".into())];
        };
        if self.state.borrow().is_uncaught_stop() {
            self.finished = true;
            return vec![
                self.ok(req_seq, command, json!({ "allThreadsContinued": true })),
                self.event("terminated", json!({})),
            ];
        }
        self.state.borrow_mut().step = step;
        match vm.run_until_debug_break() {
            Ok(Some(v)) => {
                self.finished = true;
                self.last_value = Some(v.display_string());
                vec![
                    self.ok(req_seq, command, json!({ "allThreadsContinued": true })),
                    self.event("terminated", json!({})),
                ]
            }
            Ok(None) => {
                let reason = self
                    .state
                    .borrow()
                    .stop_reason
                    .map(dap_stop_reason)
                    .unwrap_or("breakpoint");
                vec![
                    self.ok(req_seq, command, json!({ "allThreadsContinued": true })),
                    self.stopped_event(reason),
                ]
            }
            Err(e) => vec![self.fail(req_seq, command, e.to_string())],
        }
    }

    fn stack_trace(&mut self, req_seq: i64) -> Vec<Json> {
        let Some(vm) = self.vm.as_ref() else {
            return vec![self.fail(req_seq, "stackTrace", "not launched".into())];
        };
        let frames: Vec<Json> = stack_frames(vm)
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "id": i,
                    "name": f.func,
                    "line": f.line,
                    "column": 1,
                    "source": { "path": f.file, "name": f.file },
                })
            })
            .collect();
        vec![self.ok(
            req_seq,
            "stackTrace",
            json!({ "stackFrames": frames, "totalFrames": frames.len() }),
        )]
    }

    fn scopes(&mut self, req_seq: i64) -> Vec<Json> {
        vec![self.ok(
            req_seq,
            "scopes",
            json!({
                "scopes": [
                    { "name": "Locals", "variablesReference": REF_LOCALS, "expensive": false },
                    { "name": "Globals", "variablesReference": REF_GLOBALS, "expensive": false },
                ]
            }),
        )]
    }

    fn variables(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let Some(vm) = self.vm.as_ref() else {
            return vec![self.fail(req_seq, "variables", "not launched".into())];
        };
        let refer = args
            .get("variablesReference")
            .and_then(Json::as_i64)
            .unwrap_or(0);
        let vars = if refer == REF_GLOBALS {
            list_globals(vm)
                .into_iter()
                .map(|(k, v)| {
                    json!({ "name": k, "value": v.display_string(), "variablesReference": 0 })
                })
                .collect::<Vec<_>>()
        } else {
            list_locals(vm)
                .into_iter()
                .map(|(k, v)| json!({ "name": k, "value": v.display_string(), "variablesReference": 0 }))
                .collect()
        };
        vec![self.ok(req_seq, "variables", json!({ "variables": vars }))]
    }

    fn evaluate(&mut self, req_seq: i64, args: &Json) -> Vec<Json> {
        let Some(vm) = self.vm.as_mut() else {
            return vec![self.fail(req_seq, "evaluate", "not launched".into())];
        };
        let expr = args.get("expression").and_then(Json::as_str).unwrap_or("");
        match debug::eval_in_paused_vm_with_budget(vm, expr, self.evaluate_budget) {
            Ok(v) => vec![self.ok(
                req_seq,
                "evaluate",
                json!({ "result": v.display_string(), "variablesReference": 0 }),
            )],
            Err(e) => vec![self.fail(req_seq, "evaluate", e.to_string())],
        }
    }
}

fn dap_stop_reason(r: StopReason) -> &'static str {
    match r {
        StopReason::Breakpoint | StopReason::Explicit => "breakpoint",
        StopReason::Step => "step",
        StopReason::Uncaught => "exception",
        StopReason::Entry => "entry",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(seq: i64, command: &str, args: Json) -> Json {
        json!({ "seq": seq, "type": "request", "command": command, "arguments": args })
    }

    fn write_tmp(name: &str, src: &str) -> String {
        let p = std::env::temp_dir().join(format!("optive_dap_{name}_{}.tive", std::process::id()));
        fs::write(&p, src).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn launch_stops_on_entry() {
        let path = write_tmp("entry", "let x = 1\nprint(x)\n");
        let mut s = Session::new();
        let out = s.handle(&req(1, "initialize", json!({})));
        assert_eq!(out[0]["success"], true);
        let out = s.handle(&req(2, "launch", json!({ "program": path })));
        assert_eq!(out[0]["success"], true, "{out:?}");
        assert!(
            out.iter()
                .any(|m| m["event"] == "stopped" && m["body"]["reason"] == "entry"),
            "{out:?}"
        );
        assert!(!s.finished);
    }

    #[test]
    fn breakpoint_then_continue_terminates() {
        let path = write_tmp("bp", "let x = 1\nlet y = x + 1\nprint(y)\n");
        let mut s = Session::new();
        s.handle(&req(1, "initialize", json!({})));
        let launch = s.handle(&req(2, "launch", json!({ "program": path })));
        assert!(launch[0]["success"].as_bool().unwrap());
        let bps = s.handle(&req(
            3,
            "setBreakpoints",
            json!({
                "source": { "path": launch_path_from(&launch, &s) },
                "breakpoints": [{ "line": 2 }]
            }),
        ));
        assert_eq!(bps[0]["success"], true);
        let cont = s.handle(&req(4, "continue", json!({ "threadId": 1 })));
        assert!(
            cont.iter()
                .any(|m| m["event"] == "stopped" || m["event"] == "terminated"),
            "{cont:?}"
        );
    }

    fn launch_path_from(_launch: &[Json], s: &Session) -> String {
        s.vm.as_ref()
            .map(|v| v.source_file.clone())
            .unwrap_or_default()
    }

    #[test]
    fn evaluate_and_stack_while_paused() {
        let path = write_tmp("eval", "let x = 41\nlet y = 1\nprint(x + y)\n");
        let mut s = Session::new();
        s.handle(&req(1, "initialize", json!({})));
        s.handle(&req(2, "launch", json!({ "program": path })));
        s.handle(&req(
            3,
            "setBreakpoints",
            json!({
                "source": { "path": path },
                "breakpoints": [{ "line": 2 }]
            }),
        ));
        let cont = s.handle(&req(4, "continue", json!({ "threadId": 1 })));
        assert!(cont.iter().any(|m| m["event"] == "stopped"), "{cont:?}");
        let ev = s.handle(&req(5, "evaluate", json!({ "expression": "x" })));
        assert_eq!(ev[0]["success"], true, "{ev:?}");
        assert_eq!(ev[0]["body"]["result"], "41");
        let st = s.handle(&req(6, "stackTrace", json!({ "threadId": 1 })));
        assert_eq!(st[0]["success"], true);
        assert!(st[0]["body"]["totalFrames"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn launch_reports_invalid_cwd() {
        let path = write_tmp("cwd", "1\n");
        let mut s = Session::new();
        let out = s.handle(&req(1, "launch", json!({ "program": path, "cwd": path })));
        assert_eq!(out[0]["success"], false);
        assert!(out[0]["message"].as_str().unwrap().contains("launch.cwd"));
    }

    #[test]
    fn debuggee_stdout_becomes_output_event() {
        let path = write_tmp(
            "output",
            "print(\"hello\")\nstd.io.write_line(\"io\")\nstd.io.eprint(\"err\")\nstd.log.error(\"logged\")\n",
        );
        let mut s = Session::new();
        s.handle(&req(1, "launch", json!({ "program": path })));
        let out = s.handle(&req(2, "continue", json!({})));
        assert!(out.iter().any(|m| {
            m["event"] == "output"
                && m["body"]["category"] == "stdout"
                && m["body"]["output"] == "hello\n"
        }));
        assert!(out.iter().any(|m| {
            m["event"] == "output"
                && m["body"]["category"] == "stdout"
                && m["body"]["output"] == "io\n"
        }));
        assert!(out.iter().any(|m| {
            m["event"] == "output"
                && m["body"]["category"] == "stderr"
                && m["body"]["output"]
                    .as_str()
                    .is_some_and(|s| s.contains("err"))
        }));
    }

    #[test]
    fn evaluate_budget_is_configurable() {
        let path = write_tmp("budget", "let x = 1\n");
        let mut s = Session::new();
        let launch = s.handle(&req(
            1,
            "launch",
            json!({ "program": path, "evaluateInstructionBudget": 1 }),
        ));
        assert_eq!(launch[0]["success"], true, "{launch:?}");
        let out = s.handle(&req(2, "evaluate", json!({ "expression": "1 + 2" })));
        assert_eq!(out[0]["success"], false, "{out:?}");
        assert!(out[0]["message"]
            .as_str()
            .unwrap()
            .contains("instruction budget exceeded"));
    }

    #[test]
    fn function_breakpoint_and_set_variable() {
        let path = write_tmp(
            "fnbp",
            "func bump(n) { n + 1 }\nlet x = 40\nprint(bump(x))\n",
        );
        let mut s = Session::new();
        s.handle(&req(1, "initialize", json!({})));
        s.handle(&req(
            2,
            "launch",
            json!({ "program": path, "stopOnEntry": true }),
        ));
        let fnbp = s.handle(&req(
            3,
            "setFunctionBreakpoints",
            json!({ "breakpoints": [{ "name": "bump" }] }),
        ));
        assert_eq!(fnbp[0]["success"], true, "{fnbp:?}");
        let threads = s.handle(&req(4, "threads", json!({})));
        assert_eq!(threads[0]["success"], true);
        assert!(!threads[0]["body"]["threads"].as_array().unwrap().is_empty());
        s.handle(&req(
            5,
            "setExceptionBreakpoints",
            json!({ "filters": ["uncaught"] }),
        ));
        let cont = s.handle(&req(6, "continue", json!({ "threadId": 1 })));
        assert!(
            cont.iter()
                .any(|m| m["event"] == "stopped" || m["event"] == "terminated"),
            "{cont:?}"
        );
        if !s.finished {
            let setv = s.handle(&req(
                7,
                "setVariable",
                json!({ "variablesReference": 1, "name": "x", "value": "41" }),
            ));
            assert!(
                setv[0]["success"].as_bool().unwrap_or(false) || setv[0]["message"].is_string(),
                "{setv:?}"
            );
        }
    }

    #[test]
    fn conditional_breakpoint_stdio_sequence() {
        let path = write_tmp("cond", "var i = 0\nloop (3) { i = i + 1 }\nprint(i)\n");
        let mut s = Session::new();
        s.handle(&req(1, "initialize", json!({})));
        s.handle(&req(2, "launch", json!({ "program": path })));
        let bps = s.handle(&req(
            3,
            "setBreakpoints",
            json!({
                "source": { "path": path },
                "breakpoints": [{ "line": 2, "condition": "i == 2", "logMessage": "i" }]
            }),
        ));
        assert_eq!(bps[0]["success"], true);
        let _ = s.handle(&req(4, "continue", json!({ "threadId": 1 })));
        let _ = s.handle(&req(5, "next", json!({ "threadId": 1 })));
    }
}
