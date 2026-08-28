use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::builtins;
use crate::error::RuntimeError;
use crate::exceptions;
use crate::module;
use crate::opcode::{CompiledProgram, FunctionObject, Instruction, MacroObject, ModuleGlobalEnv};
use crate::runtime_ast::{self, RuntimeAstNode};
use crate::traceback;
use crate::type_registry;
use crate::types::{self, type_value_display};
use crate::value::{
    values_identical, BuiltinFn, ChannelInner, DictMap, DispatchTable, IteratorKind, IteratorState,
    ModuleObject, MutexInner, Num, TaskInner, TaskState, Value, ValueKey,
};
use crate::Result;

use crate::scheduler::{self, MnScheduler};
use crate::shared::{Shared, SharedMap, SharedTable, SyncCell};
use crossbeam_deque::Worker;

mod debug_helpers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

type OutputCallback = dyn Fn(OutputStream, &str) + Send + Sync;

#[derive(Clone)]
pub struct OutputSink(Arc<OutputCallback>);

impl OutputSink {
    pub fn new(f: impl Fn(OutputStream, &str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    fn write(&self, stream: OutputStream, text: &str) {
        (self.0)(stream, text);
    }
}

impl Default for OutputSink {
    fn default() -> Self {
        Self::new(|stream, text| {
            use std::io::Write;
            match stream {
                OutputStream::Stdout => {
                    let _ = std::io::stdout().write_all(text.as_bytes());
                    let _ = std::io::stdout().flush();
                }
                OutputStream::Stderr => {
                    let _ = std::io::stderr().write_all(text.as_bytes());
                    let _ = std::io::stderr().flush();
                }
            }
        })
    }
}
/// 运行时可见的已安装依赖包。
#[derive(Debug, Clone)]
pub struct DepPackage {
    pub path: std::path::PathBuf,
    pub id: String,
}

/// 操作数栈紧凑槽：Int/Bool/Empty 内联存储，其余装箱；比完整 [Value] 更省拷贝与空间。
#[derive(Clone)]
enum StackVal {
    Empty,
    Bool(bool),
    Int(i64),
    /// 用户函数：避免每次 Load/Call 经 `Heap(Box<Value::Function>)` 分配。
    Func(Arc<FunctionObject>),
    Heap(Box<Value>),
}

/// 热循环控制流（模块级；不可放在 impl 内）。
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum HotFlow {
    Cont = 0,
    Cold = 1,
    PendingRet = 2,
    Fail = 3,
    /// 已切换 `hot_ops`（如轻量 `Call`），外层须 `continue 'outer` 刷新切片。
    Switched = 4,
}

/// `run_interpreter` 出口：正常值、任务已挂起、或调试器请求暂停。
enum InterpResult {
    Value(Option<Value>),
    Suspended,
    DebugBreak,
    /// 生成器 `yield` / `yield from` 产出一个值后挂起。
    Yielded(Value),
}

/// 当前正在跑的调度任务边界（挂起时裁剪到此水位）。
///
/// 同时保存**本 worker 宿主**在切入任务前的代码指针。纤程可能在 helper 上
/// `setup_user_call`（当时 `saved_code` 为空），再被主线程偷走；挂起/结束时必须
/// 恢复当前宿主状态，不能用另一线程帧里的 `saved_*`。
struct TaskRunCtx {
    task: Shared<TaskInner>,
    stop_ucf: usize,
    stop_locals: usize,
    stop_nts: usize,
    stop_stack: usize,
    stop_func_stack: usize,
    stop_func_frames: usize,
    stop_try: usize,
    stop_iters: usize,
    stop_fast_ret: usize,
    stop_lw_bases: usize,
    stop_lw_sp: usize,
    stop_lw_depth: usize,
    stop_lw_base: usize,
    stop_lw_entry_pc: usize,
    stop_lw_frame_slots: usize,
    host_code: Arc<Vec<Instruction>>,
    host_hot_ops: Arc<[u8]>,
    host_hot_args: Arc<[i64]>,
    host_pc: usize,
    host_line_map: Arc<Vec<usize>>,
    host_column_map: Arc<Vec<usize>>,
}

/// 挂起任务的纤程快照。
pub(crate) struct TaskFiber {
    code: Arc<Vec<Instruction>>,
    hot_ops: Arc<[u8]>,
    hot_args: Arc<[i64]>,
    pc: usize,
    active_line_map: Arc<Vec<usize>>,
    active_column_map: Arc<Vec<usize>>,
    stack: Vec<StackVal>,
    locals_stack: Vec<Vec<Value>>,
    name_to_slot: Vec<Option<FxHashMap<String, usize>>>,
    user_call_frames: Vec<UserCallFrame>,
    func_stack: Vec<Arc<FunctionObject>>,
    func_frames: Vec<FuncFrame>,
    try_stack: Vec<TryFrame>,
    iterators: Vec<ActiveIter>,
    fast_ret_pcs: Vec<usize>,
    lw_slots: Vec<StackVal>,
    lw_bases: Vec<usize>,
    lw_base: usize,
    /// 相对宿主 `stop_lw_depth` 的增量；重函数暂停脚本帧时可为负。
    lw_depth: isize,
    lw_entry_pc: usize,
    lw_frame_slots: usize,
    /// 卸荷 FFI pending（随纤程迁移，不能只挂在 Vm 上）。
    ffi_wait: Option<std::sync::Arc<crate::ffi_pool::FfiPending>>,
    /// `go builtin(...)` 卸荷后：恢复时再 poll 同一 Builtin，勿跑宿主字节码。
    retry_poll: Option<(Value, Vec<Value>)>,
    /// Barrier/Cond 挂起后重试：已登记等待，勿再次 +1。
    sync_wait_resume: Option<SyncWaitResume>,
}

/// 同步原语在 `block_suspend` + Call 重试之间保留的登记状态。
#[derive(Clone)]
enum SyncWaitResume {
    Barrier {
        id: usize,
        generation: u64,
    },
    Cond {
        id: usize,
    },
    /// Cond 已收到信号，正在重新获取 mutex；挂起后勿再次登记 waiter / unlock。
    CondRelock {
        id: usize,
    },
    /// `std.time.sleep` 协作切片：截止时刻，随纤程迁移。
    Sleep {
        until: std::time::Instant,
    },
}

/// 辅助宏：从环境变量读取正的 usize，带缓存或不带缓存。
macro_rules! env_usize {
    (cached $name:ident, $env:literal, $default:expr) => {
        fn $name() -> usize {
            static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            *CACHED.get_or_init(|| {
                std::env::var($env)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .filter(|&n| n > 0)
                    .unwrap_or($default)
            })
        }
    };
    ($name:ident, $env:literal, $default:expr) => {
        fn $name() -> usize {
            std::env::var($env)
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|&n| n > 0)
                .unwrap_or($default)
        }
    };
}

env_usize!(suspend_budget_default, "OPTIVE_SUSPEND_BUDGET", 8_192);
env_usize!(cached max_call_depth, "OPTIVE_MAX_CALL_DEPTH", 10_000);
env_usize!(cached gc_auto_threshold, "OPTIVE_GC_THRESHOLD", 8_192);

#[inline]
fn clamp_suspend_budget(n: usize) -> usize {
    // tick_budget 无检查递减；预算必须 ≥1。
    n.max(1)
}

// 预分配容量常量 — 避免热路径扩容，控制初始内存占用。
const STACK_INIT_CAP: usize = 256;
const CALL_ARGS_BUF_INIT_CAP: usize = 8;
const FAST_RET_PCS_INIT_CAP: usize = 128;
const LW_SLOTS_INIT_CAP: usize = 1024;
const LW_BASES_INIT_CAP: usize = 128;
/// M:N 死锁检测：连续「全局静默」轮次阈值（每轮 ≈ `wait_brief` 2ms）。
const MN_DEADLOCK_IDLE_ROUNDS: u32 = 50;
/// 任务内协作 sleep 的单次切片上限。
const COOP_SLEEP_SLICE: std::time::Duration = std::time::Duration::from_millis(10);
/// select 空转时单次睡到最近截止时间的上限。
const SELECT_IDLE_CAP_MS: u64 = 10;

impl StackVal {
    #[inline]
    fn from_value(v: Value) -> Self {
        match v {
            Value::None => Self::Empty,
            Value::Bool(b) => Self::Bool(b),
            Value::Num(Num::Small(n)) => Self::Int(n),
            Value::Function(f) => Self::Func(f),
            other => Self::Heap(Box::new(other)),
        }
    }

    #[inline]
    fn into_value(self) -> Value {
        match self {
            Self::Empty => Value::None,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(n) => Value::Num(Num::Small(n)),
            Self::Func(f) => Value::Function(f),
            Self::Heap(b) => *b,
        }
    }

    #[inline]
    fn to_value(&self) -> Value {
        match self {
            Self::Empty => Value::None,
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(n) => Value::Num(Num::Small(*n)),
            Self::Func(f) => Value::Function(f.clone()),
            Self::Heap(b) => (**b).clone(),
        }
    }

    /// 复制栈槽：内联变体按位复制；`Func` 只增 Arc 引用；堆变体 Clone。
    #[inline(always)]
    fn copy_imm(&self) -> Self {
        match self {
            Self::Empty => Self::Empty,
            Self::Bool(b) => Self::Bool(*b),
            Self::Int(n) => Self::Int(*n),
            Self::Func(f) => Self::Func(f.clone()),
            Self::Heap(v) => Self::Heap(Box::new((**v).clone())),
        }
    }

    #[inline(always)]
    fn is_truthy(&self) -> bool {
        match self {
            Self::Empty => false,
            Self::Bool(b) => *b,
            Self::Int(n) => *n != 0,
            Self::Func(_) => true,
            Self::Heap(b) => b.is_truthy(),
        }
    }
}

/// 加载期一次性校验热字节码结构。畸形字节码在此干净报错，
/// 让主循环的安全索引（`ops[pc]` 等）有了显式边界保证。
fn validate_function_hot(func: &crate::opcode::FunctionObject) -> Result<()> {
    if func.body.len() != func.hot.ops.len() {
        return Err(RuntimeError::msg(format!(
            "internal: function `{}` code/hot length mismatch ({} != {})",
            func.name,
            func.body.len(),
            func.hot.ops.len()
        )));
    }
    validate_hot_bytecode(&func.hot)
}

fn validate_hot_bytecode(hot: &crate::hot_code::HotCode) -> Result<()> {
    use crate::hot_code::{
        H_CALL, H_CALL_GLOBAL, H_CALL_SELF, H_GOTO, H_GOTO_IF, H_GOTO_IF_NOT, H_LOAD_FAST,
        H_LOAD_FAST_LE_IMM, H_LOAD_FAST_SUB_IMM, H_LOAD_GLOBAL, H_LOOP_COUNTDOWN, H_RET_FAST,
        H_STORE_FAST, H_STORE_GLOBAL,
    };
    let n = hot.ops.len();
    if hot.args.len() != n {
        return Err(RuntimeError::msg(format!(
            "internal: hot bytecode ops/args length mismatch ({} != {})",
            n,
            hot.args.len()
        )));
    }
    for pc in 0..n {
        let op = hot.ops[pc];
        let arg = hot.args[pc];
        // 跳转类指令的目标必须落在 [0, n)。
        let is_jump = matches!(op, H_GOTO | H_GOTO_IF | H_GOTO_IF_NOT | H_LOOP_COUNTDOWN);
        if is_jump {
            let target = arg;
            if target < 0 || (target as usize) >= n {
                return Err(RuntimeError::msg(format!(
                    "internal: hot bytecode jump at pc={pc} targets out-of-range {target} (len={n})"
                )));
            }
        }
        // 槽位 / 参数计数字段不得为负（否则 as usize 会变成巨大偏移）。
        let is_slot_or_argc = matches!(
            op,
            H_LOAD_FAST
                | H_STORE_FAST
                | H_RET_FAST
                | H_CALL_SELF
                | H_LOAD_FAST_SUB_IMM
                | H_LOAD_FAST_LE_IMM
                | H_LOAD_GLOBAL
                | H_STORE_GLOBAL
                | H_CALL
                | H_CALL_GLOBAL
        );
        if is_slot_or_argc && arg < 0 {
            return Err(RuntimeError::msg(format!(
                "internal: hot bytecode op at pc={pc} has negative arg {arg}"
            )));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct TryFrame {
    catch_pc: usize,
    else_pc: usize,
    end_pc: usize,
    /// 进入 try 时的 `user_call_frames` 深度；catch 时展开超出部分。
    user_call_depth: usize,
    /// 进入 try 时的操作数栈深度。
    stack_sp: usize,
    /// 进入 try 时的活动迭代器数量。
    iterators_len: usize,
    /// 进入 try 时的轻量 `CallSelf` 深度（`fast_ret_sp`）。
    fast_ret_sp: usize,
}

#[derive(Clone)]
struct UserCallFrame {
    saved_code: Arc<Vec<Instruction>>,
    saved_hot_ops: Arc<[u8]>,
    saved_hot_args: Arc<[i64]>,
    saved_pc: usize,
    saved_line_map: Arc<Vec<usize>>,
    saved_column_map: Arc<Vec<usize>>,
    func: Arc<FunctionObject>,
    pushed_func_stack: bool,
    /// 本帧是否压了 `locals_stack` / `name_to_slot`（轻量叶调用为 false）。
    pushed_name_frame: bool,
    /// 进入本帧前的轻量深度；重函数会把 `lw_depth` 置 0，返回时恢复。
    saved_lw_depth: usize,
    saved_lw_base: usize,
}

#[derive(Clone)]
pub struct FuncFrame {
    pub name: String,
    pub file: String,
    pub line: usize,
}

/// 运行时错误栈的一帧（最旧调用在前）。
#[derive(Clone, Debug)]
pub struct ErrorStackFrame {
    pub func: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: Option<Arc<str>>,
}

/// 字节码虚拟机（**非稳定**实现细节）。宿主应使用 [`crate::embed`]，不要依赖本结构字段。
pub struct Vm {
    pub code: Arc<Vec<Instruction>>,
    /// 与 code 等长的紧凑热操作码。
    hot_ops: Arc<[u8]>,
    hot_args: Arc<[i64]>,
    /// 主操作数栈存储区；有效元素个数由 `stack_sp` 限定（超出部分为复用缓冲）。
    stack: Vec<StackVal>,
    /// 逻辑栈顶下标；热路径用下标读写替代 `Vec::push/pop`。
    stack_sp: usize,
    pub globals: SharedMap,
    pub locals_stack: Vec<Vec<Value>>,
    pub name_to_slot: Vec<Option<FxHashMap<String, usize>>>,
    pub func_stack: Vec<Arc<FunctionObject>>,
    pub func_frames: Vec<FuncFrame>,
    pub pc: usize,
    pub active_line_map: Arc<Vec<usize>>,
    pub active_column_map: Arc<Vec<usize>>,
    pub struct_defs: SharedTable<Arc<crate::value::StructDef>>,
    pub enum_defs: SharedTable<Arc<crate::value::EnumDef>>,
    pub variant_defs: SharedTable<Arc<crate::value::VariantDef>>,
    pub functions: SharedTable<Arc<FunctionObject>>,
    pub macros: SharedTable<Arc<MacroObject>>,
    pub(crate) try_stack: Vec<TryFrame>,
    pub(crate) active_exception: Option<Value>,
    pub(crate) iterators: Vec<ActiveIter>,
    pub(crate) const_names: FxHashSet<String>,
    /// 已声明但尚未执行到对应 store 的 const 名（允许先引用后赋值）。
    pub(crate) pending_const: FxHashSet<String>,
    pub module_cache: FxHashMap<String, Shared<ModuleObject>>,
    pub builtin_modules: FxHashMap<String, Shared<ModuleObject>>,
    pub module_init_exports: Option<Shared<HashMap<String, Value>>>,
    macro_eval_scopes: Vec<EvalSnapshot>,
    convert_tables: FxHashMap<String, Shared<DispatchTable>>,
    pub source_file: String,
    /// 当前执行中的顶层代码块源文本（REPL / 脚本 / 调试器）。
    pub current_source: Option<Arc<str>>,
    /// 运行失败 unwind 前捕获；供错误格式化消费。
    pub(crate) last_error_stack: Vec<ErrorStackFrame>,
    pub import_base: std::path::PathBuf,
    /// 依赖可见性：`(parent_package_id, name) → 包根`
    pub dep_map: std::collections::HashMap<(String, String), DepPackage>,
    /// 当前执行模块所属包（`__root__` 或 content id）
    pub current_package_id: String,
    /// 当前包根目录（依赖包内模块解析用）
    pub package_root: Option<std::path::PathBuf>,
    pub overload_tables: SharedTable<Vec<Arc<FunctionObject>>>,
    pub(crate) primitive_methods: FxHashMap<String, FxHashMap<String, BuiltinFn>>,
    user_call_frames: Vec<UserCallFrame>,
    user_call_deferred: bool,
    /// 嵌套 `call_user_function` 已把任务纤程挂起；外层 Call 不得把 dummy None 当返回值。
    pub(crate) nested_user_call_suspended: bool,
    script_global_names: Vec<String>,
    script_globals: Vec<Value>,
    /// 脚本顶层快局部槽数；0 表示未启用。
    script_frame_slots: usize,
    /// 脚本快局部 → `script_global_names` 下标。
    script_local_to_global: Vec<(usize, usize)>,
    /// 本 OS 线程私有的全局函数 `Arc`（拷贝热字节码），避免跨核争用同一 `FunctionObject`。
    local_fn_hot: Vec<Option<Arc<FunctionObject>>>,
    /// 定义处绑定注解时优先从此模块环境取名（方法/导入后再绑定时用）。
    pub(crate) annotation_bind_env: Option<Arc<crate::opcode::ModuleGlobalEnv>>,
    local_frame_pool: Vec<Vec<Value>>,
    call_args_buf: Vec<Value>,
    /// 轻量 `CallSelf` 调用链保存的返回 PC（缓冲复用，见 `fast_ret_sp`）。
    fast_ret_pcs: Vec<usize>,
    /// `fast_ret_pcs` 已用深度；push/pop 不走 `Vec::push/pop`。
    fast_ret_sp: usize,
    /// 轻量 `CallSelf` 使用的快局部槽数组。
    lw_slots: Vec<StackVal>,
    /// 每层快路径帧在 `lw_slots` 中的起始下标栈（缓冲复用）。
    lw_bases: Vec<usize>,
    /// `lw_bases` 已用深度。
    lw_bases_sp: usize,
    /// `lw_slots` 已用长度；截断时只改此计数，避免对尾部槽位 drop/resize。
    lw_sp: usize,
    /// 当前帧在 `lw_slots` 中的基址；LoadFast 相对此偏移取槽。
    lw_base: usize,
    /// 嵌套 `CallSelf` 深度；为 0 表示未进入快路径局部帧。
    lw_depth: usize,
    /// `CallSelf` 进入时的入口 PC；返回时与 `func_stack` 等状态一并恢复。
    lw_entry_pc: usize,
    lw_frame_slots: usize,
    /// 缓存的 `max_call_depth()`，避免热路径每次读 `OnceLock`。
    cached_max_depth: usize,
    /// 热路径 Ret 延迟完成：保存 (`leave_scope`, result) 待外层解释循环处理。
    pending_ret: Option<(bool, StackVal)>,
    /// 热路径是否已失败；用 bool 避免每次错误路径都 `Option::take`。
    hot_failed: bool,
    /// 与 `hot_failed` 配套的详细错误；失败时由外层取出。
    hot_error: Option<RuntimeError>,
    /// 跨 worker 共享的环收集器（Weak 跟踪 + 并发标记状态）。
    pub gc: std::sync::Arc<crate::gc::SharedGc>,
    /// 自动 GC 跟踪表阈值（每 Vm 可覆盖，避免测试改进程环境）。
    gc_threshold: usize,
    /// `:: list[T]` 等强绑定后挂在列表对象上的元素契约（按 Rc 指针键）。
    pub(crate) list_element_contracts: FxHashMap<usize, Value>,
    pub(crate) dict_contracts: FxHashMap<usize, (Value, Value)>,
    pub(crate) set_element_contracts: FxHashMap<usize, Value>,
    /// 已编译程序中的协议定义（供运行时 `is_a` / `:: Protocol`）。
    pub(crate) protocols: SharedTable<Arc<crate::protocol::ProtocolDef>>,
    /// M:1 协作调度就绪队列（`OPTIVE_WORKERS<=1` 时使用，保留确定性顺序）。
    pub(crate) ready_tasks: VecDeque<Shared<TaskInner>>,
    /// 挂起任务的纤程快照（M:1 本地）；M:N 时改走 `mn.fibers`。
    task_fibers: FxHashMap<usize, TaskFiber>,
    /// M:N 调度器（始终存在；worker 数见 `OPTIVE_WORKERS`）。
    pub(crate) mn: Arc<MnScheduler>,
    /// 本 OS 线程的 work-stealing 本地队列。
    local_worker: Worker<Shared<TaskInner>>,
    /// `mn.worker_count > 1` 时为真并行。
    mn_parallel: bool,
    /// `Vm::new` 主实例为 true；helper fork 为 false（仅主实例负责 shutdown）。
    mn_primary: bool,
    /// M:N 死锁检测：连续「全局静默」轮次计数（仅主实例使用）。
    mn_idle_rounds: u32,
    /// 当前 select 轮次的伪随机 case 次序（`SelectBegin` / `SelectNextIndex`）。
    select_fair_order: Vec<usize>,
    select_fair_pos: usize,
    /// select 公平性 PRNG 状态。
    select_rng: u64,
    /// 任务纤程命中调试停点；主解释循环应返回 `DebugBreak`。
    debug_break_requested: bool,
    /// 因调试而停住的任务（不在就绪队列，待 continue 再入队）。
    debug_paused_tasks: Vec<Shared<TaskInner>>,
    /// STW 失败后的自动 GC 冷却：在此之前且 `tracked_count` 未超过记录值时跳过。
    gc_auto_cooldown_until: Option<std::time::Instant>,
    /// 冷却期内允许提前重试的下限：仅当 `tracked_count >` 此值时才无视时间冷却。
    gc_auto_cooldown_hold_count: usize,
    /// `scheduler_run_task` 嵌套深度；>0 时阻塞应挂起当前任务而非再入调度。
    sched_depth: u32,
    /// 同步原语请求「挂起并重试当前 Call」（由 `call_value` 武装栈/PC）。
    pub(crate) block_suspend: bool,
    /// `arm_call_retry` 已把 callee/args 压回并回绕 PC；Call 勿再压返回值。
    /// 与 `request_cooperative_yield` 区分：后者挂起前仍须压入本次真实返回值。
    call_retry_armed: bool,
    /// 与 helper worker 共享的 FFI 运行时开关（串行 / 卸荷线程数）。
    ffi_cfg: std::sync::Arc<crate::ffi::FfiRuntimeConfig>,
    /// 卸荷中的 pending（任务重试 Builtin 时取回）。
    ffi_wait: Option<std::sync::Arc<crate::ffi_pool::FfiPending>>,
    /// `call_value_poll` 里 Builtin 卸荷后，随 `capture_fiber` 写入 `retry_poll`。
    pending_poll_retry: Option<(Value, Vec<Value>)>,
    /// Barrier/Cond 挂起重试令牌（亦随 `TaskFiber` 迁移）。
    sync_wait_resume: Option<SyncWaitResume>,
    /// 当前调度任务上下文；`None` 表示主纤程。
    task_ctx: Option<TaskRunCtx>,
    /// 每片最多执行的字节码条数。
    suspend_budget: usize,
    /// 本片剩余预算。
    budget_left: usize,
    /// 任务请求挂起（显式 `suspend` 或预算耗尽）。
    pending_suspend: bool,
    /// 主纤程预算耗尽，需跑一轮就绪队列。
    pending_main_yield: bool,
    /// M:1 热 `StoreGlobal` 跳过了 `SharedMap` 同步；调度其它纤程前需 flush。
    script_globals_map_dirty: bool,
    /// 正在 `advance_iterator` 恢复生成器。
    generator_resuming: bool,
    /// 当前恢复的生成器状态（供 Yield/YieldFrom 写回）。
    active_generator: Option<Shared<IteratorState>>,
    /// Yield 刚产出的值，由 `run_interpreter` 转为 `InterpResult::Yielded`。
    pending_gen_yield: Option<Value>,
    /// 调试会话状态；`None` 时热路径仅多一次空检查。
    pub debug: Option<Shared<crate::debug::DebugState>>,
    /// `debug.is_some()` 的布尔缓存，热路径避免加载 Option 指针。
    pub(crate) debug_active: bool,
    /// `Optive test --cover`；与 debug 分开，不改热调用路径。
    pub cover: Option<Shared<crate::coverage::CoverageState>>,
    pub(crate) cover_active: bool,
    /// `std.test.each` 行结果，供测试运行器打印。
    pub test_case_log: Vec<String>,
    /// `std.test.tmp_dir` 创建的目录，用例结束后删。
    pub test_tmp_dirs: Vec<std::path::PathBuf>,
    /// `!const_names.is_empty()` 的布尔缓存，热路径 `StoreFast` 零额外加载。
    pub(crate) has_const_names: bool,
    /// `!pending_const.is_empty()` 缓存，避免热 `StoreGlobal` 每次查 HashSet。
    pub(crate) has_pending_const: bool,
    /// 运行时能力隔离：网络 / 文件系统 / 环境变量网关。默认全开。
    pub caps: crate::caps::Capabilities,
    /// 入口/项目宿主能力；依赖模块初始化会临时换成 `restrict_for_dependency`。
    pub host_caps: crate::caps::Capabilities,
    /// 若设置，`std.os.args()` 返回此列表（`run`/`up` 注入入口可见 argv）。
    pub argv_override: Option<Vec<String>>,
    output_sink: OutputSink,
    debug_eval_budget: Option<usize>,
    /// 宿主取消标志（embedding）；不进入热分派。
    pub(crate) host_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub metrics: Option<Shared<crate::metrics::Metrics>>,
    pub(crate) metrics_active: bool,
}

#[derive(Clone)]
pub(crate) struct EvalSnapshot {
    pub(crate) globals: SharedMap,
    pub(crate) locals_stack: Vec<Vec<Value>>,
    pub(crate) name_to_slot: Vec<Option<FxHashMap<String, usize>>>,
    code: Arc<Vec<Instruction>>,
    hot_ops: Arc<[u8]>,
    hot_args: Arc<[i64]>,
    active_line_map: Arc<Vec<usize>>,
    active_column_map: Arc<Vec<usize>>,
    pc: usize,
    stack: Vec<StackVal>,
    functions: FxHashMap<String, Arc<FunctionObject>>,
    macros: FxHashMap<String, Arc<MacroObject>>,
    struct_defs: FxHashMap<String, Arc<crate::value::StructDef>>,
    enum_defs: FxHashMap<String, Arc<crate::value::EnumDef>>,
    variant_defs: FxHashMap<String, Arc<crate::value::VariantDef>>,
    script_global_names: Vec<String>,
    script_globals: Vec<Value>,
    script_frame_slots: usize,
    script_local_to_global: Vec<(usize, usize)>,
    lw_slots: Vec<StackVal>,
    lw_bases: Vec<usize>,
    lw_bases_sp: usize,
    lw_sp: usize,
    lw_base: usize,
    lw_depth: usize,
}

pub(crate) struct ActiveIter {
    state: Shared<IteratorState>,
}

enum StepAction {
    Push(Value),
    PushSmall(i64),
    Pop,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    LShift,
    RShift,
    Neg,
    Invert,
    Not,
    TruthyNot,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Is,
    IsNot,
    Load(String),
    LoadGlobal(usize),
    LoadMacro(String),
    Store(String),
    StoreGlobal(usize),
    NewVar {
        name: String,
        is_const: bool,
    },
    NewVarOrLoad(String),
    LoadFast(usize),
    StoreFast(usize),
    LoadFastSubImm {
        slot: usize,
        imm: i64,
    },
    LoadFastLeImm {
        slot: usize,
        imm: i64,
    },
    LoadFastLtImm {
        slot: usize,
        imm: i64,
    },
    LoadFastGtImm {
        slot: usize,
        imm: i64,
    },
    LoadFastEqImm {
        slot: usize,
        imm: i64,
    },
    LoadFastAddImmStore {
        slot: usize,
        imm: i64,
    },
    LoadFastAddStore {
        dst: usize,
        src: usize,
    },
    LoadFastSqrGt {
        sqr_slot: usize,
        rhs_slot: usize,
    },
    LoadFastModEq0 {
        lhs_slot: usize,
        rhs_slot: usize,
    },
    BindFast {
        slot: usize,
        name: String,
        is_const: bool,
    },
    EnterScope,
    LeaveScope,
    Label,
    Goto(usize),
    GotoIf(usize),
    GotoIfNot(usize),
    LoopCountdown(usize),
    Call {
        argc: usize,
    },
    CallGlobal {
        global_idx: usize,
        argc: usize,
    },
    CallSelf {
        argc: usize,
    },
    CallList,
    CallEx,
    MacroCall {
        argc: usize,
    },
    ListAppend,
    ListExtend,
    DictSet,
    SetAdd,
    Ret,
    RetFast(usize),
    RetLeave,
    VecNew(usize),
    DictNew(usize),
    SetNew(usize),
    TupleNew(usize),
    Index,
    IndexSet,
    SliceGet,
    SliceSet,
    DelIndex,
    DelName(String),
    DelAttr(String),
    GetAttr(String),
    StructNew {
        name: String,
        argc: usize,
    },
    VariantNew {
        name: String,
    },
    SetField(String),
    IterNew,
    IterNext,
    IterEnd,
    Throw,
    Snap,
    PushExc,
    EnterTry {
        catch_label: usize,
        else_label: usize,
        end_label: usize,
    },
    EndTry,
    PopTry,
    ExcMatch(String),
    IsList,
    ListLen,
    IsInstance(String),
    MatchEq,
    UnpackExact(usize),
    UnpackRest {
        before: usize,
        after: usize,
    },
    Rethrow,
    TypeCheck,
    /// 栈顶 Function：定义处求值并缓存类型注解。
    ResolveFuncTypes,
    FindMod(Vec<String>),
    FindModFile(String),
    RegisterExport(String),
    GoCall {
        argc: usize,
    },
    GoValue,
    Await,
    Suspend,
    Yield,
    YieldFrom,
    SelectTryRecv,
    SelectTrySend,
    SelectPollTask,
    MakeDeadline,
    SelectPollDeadline,
    SelectIdle(usize),
    SelectBegin(usize),
    SelectNextIndex,
}

pub(crate) struct ModuleInitSnapshot {
    pub(crate) globals: SharedMap,
    pub(crate) functions: FxHashMap<String, Arc<FunctionObject>>,
    pub(crate) macros: FxHashMap<String, Arc<MacroObject>>,
    pub(crate) struct_defs: FxHashMap<String, Arc<crate::value::StructDef>>,
    pub(crate) overload_tables: FxHashMap<String, Vec<Arc<FunctionObject>>>,
    pub(crate) const_names: FxHashSet<String>,
    pub(crate) module_init_exports: Option<Shared<HashMap<String, Value>>>,
    pub(crate) code: Arc<Vec<Instruction>>,
    pub(crate) pc: usize,
    pub(crate) script_global_names: Vec<String>,
    pub(crate) script_globals: Vec<Value>,
    pub(crate) script_frame_slots: usize,
    pub(crate) script_local_to_global: Vec<(usize, usize)>,
    lw_slots: Vec<StackVal>,
    lw_bases: Vec<usize>,
    lw_bases_sp: usize,
    lw_sp: usize,
    lw_base: usize,
    lw_depth: usize,
}

impl Vm {
    #[must_use]
    pub fn new() -> Self {
        Self::with_workers(scheduler::configured_workers())
    }

    /// 构造指定 OS worker 数的 Vm（`1` = M:1；`>1` = M:N）。测试与嵌入宿主可绕过环境变量。
    #[must_use]
    pub fn with_workers(workers: usize) -> Self {
        Self::with_workers_gc(workers, crate::gc::GcMode::from_env())
    }

    /// 覆盖自动 GC 跟踪表阈值（测试用；不影响进程环境变量）。
    pub fn with_gc_threshold(mut self, threshold: usize) -> Self {
        self.gc_threshold = threshold.max(1);
        self
    }

    /// helper 线程认领任务的次数（M:1 为 0）。
    #[must_use]
    pub fn helper_runs(&self) -> usize {
        self.mn.helper_runs()
    }

    /// 覆盖协作切片预算（测试用；勿用进程级 `OPTIVE_SUSPEND_BUDGET`，以免污染并行测试）。
    pub fn with_suspend_budget(mut self, budget: usize) -> Self {
        let b = clamp_suspend_budget(budget);
        self.suspend_budget = b;
        self.budget_left = b;
        self
    }

    pub fn set_output_sink(&mut self, sink: OutputSink) {
        self.output_sink = sink;
    }

    pub fn set_host_cancel(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.host_cancel = Some(flag);
    }

    /// 安装入口能力，并记住宿主基线供依赖模块降权。
    pub fn install_caps(&mut self, caps: crate::caps::Capabilities) {
        self.host_caps = caps.clone();
        self.caps = caps;
    }

    pub fn write_output(&self, stream: OutputStream, text: &str) {
        self.output_sink.write(stream, text);
    }

    /// 指定 worker 数与 GC 模式（helper 与主线程共享同一 `SharedGc`）。
    #[must_use]
    pub fn with_workers_gc(workers: usize, gc_mode: crate::gc::GcMode) -> Self {
        Self::with_workers_gc_shared(
            workers,
            std::sync::Arc::new(crate::gc::SharedGc::with_mode(gc_mode)),
        )
    }

    /// 同 [`Self::with_workers_gc`]，并固定并行 marker 线程数（测试 / 基准）。
    #[must_use]
    pub fn with_workers_gc_markers(
        workers: usize,
        gc_mode: crate::gc::GcMode,
        markers: usize,
    ) -> Self {
        Self::with_workers_gc_shared(
            workers,
            std::sync::Arc::new(crate::gc::SharedGc::with_mode_markers(gc_mode, markers)),
        )
    }

    #[must_use]
    fn with_workers_gc_shared(workers: usize, gc: std::sync::Arc<crate::gc::SharedGc>) -> Self {
        let workers = workers.max(1);
        let mut vm = Self {
            code: Arc::new(Vec::new()),
            hot_ops: Arc::from([]),
            hot_args: Arc::from([]),
            stack: Vec::with_capacity(STACK_INIT_CAP),
            stack_sp: 0,
            globals: SharedMap::new(),
            locals_stack: Vec::new(),
            name_to_slot: Vec::new(),
            func_stack: Vec::new(),
            func_frames: Vec::new(),
            pc: 0,
            active_line_map: Arc::new(Vec::new()),
            active_column_map: Arc::new(Vec::new()),
            struct_defs: SharedTable::new(),
            enum_defs: SharedTable::new(),
            variant_defs: SharedTable::new(),
            functions: SharedTable::new(),
            macros: SharedTable::new(),
            try_stack: Vec::new(),
            active_exception: None,
            iterators: Vec::new(),
            const_names: FxHashSet::default(),
            pending_const: FxHashSet::default(),
            module_cache: FxHashMap::default(),
            builtin_modules: FxHashMap::default(),
            module_init_exports: None,
            macro_eval_scopes: Vec::new(),
            convert_tables: FxHashMap::default(),
            source_file: "<script>".into(),
            current_source: None,
            last_error_stack: Vec::new(),
            import_base: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            dep_map: std::collections::HashMap::new(),
            current_package_id: "__root__".into(),
            package_root: None,
            overload_tables: SharedTable::new(),
            primitive_methods: FxHashMap::default(),
            user_call_frames: Vec::new(),
            user_call_deferred: false,
            nested_user_call_suspended: false,
            script_global_names: Vec::new(),
            script_globals: Vec::new(),
            script_frame_slots: 0,
            script_local_to_global: Vec::new(),
            local_fn_hot: Vec::new(),
            annotation_bind_env: None,
            local_frame_pool: Vec::new(),
            call_args_buf: Vec::with_capacity(CALL_ARGS_BUF_INIT_CAP),
            fast_ret_pcs: Vec::with_capacity(FAST_RET_PCS_INIT_CAP),
            fast_ret_sp: 0,
            lw_slots: Vec::with_capacity(LW_SLOTS_INIT_CAP),
            lw_bases: Vec::with_capacity(LW_BASES_INIT_CAP),
            lw_bases_sp: 0,
            lw_sp: 0,
            lw_base: 0,
            lw_depth: 0,
            lw_entry_pc: 0,
            lw_frame_slots: 0,
            cached_max_depth: max_call_depth(),
            pending_ret: None,
            hot_failed: false,
            hot_error: None,
            gc,
            gc_threshold: gc_auto_threshold(),
            list_element_contracts: FxHashMap::default(),
            dict_contracts: FxHashMap::default(),
            set_element_contracts: FxHashMap::default(),
            protocols: SharedTable::new(),
            ready_tasks: VecDeque::new(),
            task_fibers: FxHashMap::default(),
            mn: {
                // placeholder; overwritten below after stealer registration
                MnScheduler::new(1)
            },
            local_worker: scheduler::new_local_worker(),
            mn_parallel: false,
            mn_primary: true,
            mn_idle_rounds: 0,
            select_fair_order: Vec::new(),
            select_fair_pos: 0,
            select_rng: 0x00C0_FFEE_u64 ^ std::time::Instant::now().elapsed().as_nanos() as u64,
            debug_break_requested: false,
            debug_paused_tasks: Vec::new(),
            gc_auto_cooldown_until: None,
            gc_auto_cooldown_hold_count: 0,
            sched_depth: 0,
            block_suspend: false,
            call_retry_armed: false,
            ffi_cfg: crate::ffi::FfiRuntimeConfig::from_env(),
            ffi_wait: None,
            pending_poll_retry: None,
            sync_wait_resume: None,
            task_ctx: None,
            suspend_budget: clamp_suspend_budget(suspend_budget_default()),
            budget_left: clamp_suspend_budget(suspend_budget_default()),
            pending_suspend: false,
            pending_main_yield: false,
            script_globals_map_dirty: false,
            generator_resuming: false,
            active_generator: None,
            pending_gen_yield: None,
            debug: None,
            debug_active: false,
            cover: None,
            cover_active: false,
            test_case_log: Vec::new(),
            test_tmp_dirs: Vec::new(),
            has_const_names: false,
            has_pending_const: false,
            caps: crate::caps::Capabilities::full(),
            host_caps: crate::caps::Capabilities::full(),
            argv_override: None,
            output_sink: OutputSink::default(),
            debug_eval_budget: None,
            host_cancel: None,
            metrics: None,
            metrics_active: false,
        };
        let mn = MnScheduler::new(workers);
        mn.register_stealer(vm.local_worker.stealer());
        vm.mn = mn;
        vm.mn_parallel = workers > 1;
        builtins::install_globals(&mut vm);
        type_registry::install_core_types(&mut vm);
        module::install_std(&mut vm);
        if vm.mn_parallel {
            let started = vm.spawn_helper_workers();
            if started + 1 < workers {
                eprintln!(
                    "optive: 仅 {started}/{} 个 helper worker 启动成功，并行能力下降",
                    workers.saturating_sub(1)
                );
            }
            if started == 0 {
                // 一个 helper 都起不来：回退 M:1，避免任务无人执行。
                eprintln!("optive: helper worker 全部启动失败，回退为单 worker（M:1）");
                vm.mn.shrink_to_single();
                vm.mn_parallel = false;
            }
        }
        crate::gc::install_current_gc(vm.gc.clone());
        vm
    }

    /// 强制全局 FFI 锁（等同 `OPTIVE_FFI_SERIAL=1`）。
    /// 写入与 helper 共享的配置，使已 fork 的 worker 立刻生效。
    pub fn with_ffi_serial(self, serial: bool) -> Self {
        self.ffi_cfg.set_serial(serial);
        self
    }

    /// FFI 卸荷池线程数；`0` 关闭卸荷（默认）。等同 `OPTIVE_FFI_THREADS`。
    pub fn with_ffi_threads(self, n: usize) -> Self {
        self.ffi_cfg.set_threads(n);
        self
    }

    pub(crate) fn ffi_serial(&self) -> bool {
        self.ffi_cfg.serial()
    }

    pub(crate) fn ffi_threads(&self) -> usize {
        self.ffi_cfg.threads()
    }

    /// 仅在任务纤程内卸荷（可 `block_suspend` 让出 worker）。
    pub(crate) const fn can_offload_ffi(&self) -> bool {
        self.sched_depth > 0 && self.task_ctx.is_some()
    }

    pub(crate) fn set_ffi_wait(&mut self, pending: std::sync::Arc<crate::ffi_pool::FfiPending>) {
        self.ffi_wait = Some(pending);
    }

    pub(crate) fn ffi_wait_still_pending(&self) -> bool {
        match &self.ffi_wait {
            Some(p) => !p.is_ready(),
            None => false,
        }
    }

    pub(crate) fn take_ready_ffi_wait(
        &mut self,
    ) -> Option<std::result::Result<(crate::ffi::RetStorage, i32), String>> {
        let Some(p) = &self.ffi_wait else {
            return None;
        };
        let ready = p.try_take()?;
        self.ffi_wait = None;
        Some(ready)
    }

    /// 将 v 中的堆容器（List / Dict / Iterator / Cell / Struct）登记到 GC 跟踪表。
    /// 新建容器后调用，以便后续 collect 能发现并清扫未达环。
    pub(crate) fn track_value(&mut self, v: &Value) {
        match v {
            Value::List(rc) => self.gc.track_list(rc),
            Value::Dict(rc) => self.gc.track_dict(rc),
            Value::Set(rc) => self.gc.track_set(rc),
            Value::Iterator(rc) => self.gc.track_iter(rc),
            Value::Cell(rc) => self.gc.track_cell(rc),
            Value::Struct(rc) => self.gc.track_struct(rc),
            _ => {}
        }
    }

    /// 跟踪表过大时自动清扫环，避免无人调用 `gc()` 时 Weak 表无限增长。
    fn maybe_auto_gc(&mut self) {
        let count = self.gc.tracked_count();
        let requested = self.mn_parallel && self.mn_primary && self.mn.take_gc_request();
        if count < self.gc_threshold && !requested {
            return;
        }
        // Helper 不发起 STW：只登记请求并修剪死 Weak，避免与 primary 双端握手死锁。
        if self.mn_parallel && !self.mn_primary {
            self.mn.request_gc();
            let _ = self.gc.prune_dead();
            return;
        }
        // STW 失败后冷却：避免 tracked_count 仍超阈时反复打满 begin_stw 超时。
        if let Some(until) = self.gc_auto_cooldown_until {
            if std::time::Instant::now() < until && count <= self.gc_auto_cooldown_hold_count {
                if requested {
                    self.mn.request_gc();
                }
                return;
            }
        }
        self.gc_collect();
    }

    /// 标记-清扫环收集：从根出发标记可达堆对象，再清空不可达容器内部。
    /// - `OPTIVE_GC_MODE=concurrent`（默认）：M:N 大堆走并发协议；M:1/小堆自适应 STW
    /// - `OPTIVE_GC_MODE=stw`：强制完整 STW 标记+清扫
    /// - M:N helper 调用时转为 `request_gc` + `prune_dead`（仅 primary 做 STW）
    pub fn gc_collect(&mut self) -> usize {
        use crate::gc::GcMode;
        use std::sync::atomic::Ordering;
        if self.mn_parallel && !self.mn_primary {
            self.mn.request_gc();
            return self.gc.prune_dead();
        }
        let t0 = std::time::Instant::now();
        let gc = self.gc.clone();
        let Some(_collect_guard) = gc.collect_lock.try_lock() else {
            // 已有收集在进行
            return 0;
        };
        let need_stw = self.mn_parallel;
        let stw_ok = if need_stw { self.mn.begin_stw() } else { true };

        let cleared = if stw_ok {
            let cleared = match gc.mode() {
                GcMode::Stw => gc.collect_stw_inner(self),
                GcMode::Concurrent => self.gc_collect_concurrent_phased(),
            };
            if need_stw {
                self.mn.end_stw();
            }
            self.gc_auto_cooldown_until = None;
            self.gc_auto_cooldown_hold_count = 0;
            cleared
        } else {
            // 握手失败：冷却后重试，不永久跳过环收集。
            let n = self.mn.note_stw_failure();
            gc.stw_fallback_count.fetch_add(1, Ordering::Relaxed);
            if n == 1 || n.is_multiple_of(64) {
                eprintln!("optive: GC stop-the-world 超时（累计 {n} 次），本轮回退冷却后重试");
            }
            let hold = gc.tracked_count();
            self.gc_auto_cooldown_hold_count = hold;
            self.gc_auto_cooldown_until = Some(std::time::Instant::now() + Self::gc_stw_cooldown());
            0
        };

        let collect_ns = t0.elapsed().as_nanos() as u64;
        let stw_ns = gc.last_stw_ns.load(Ordering::Relaxed);
        if stw_ok {
            gc.note_collect_stats(stw_ns, collect_ns, cleared);
        } else {
            gc.last_collect_ns.store(collect_ns, Ordering::Relaxed);
        }
        cleared
    }

    /// concurrent：Prepare(STW 中) → 放行 mutator 并并行标记 → Terminate+Sweep(再 STW)。
    /// `last_stw_ns` 只累计真实停顿（prepare + terminate/sweep），不含并发标记段。
    ///
    /// 自适应：无并行 mutator，或跟踪对象过少时，并发协议的握手/线程开销大于收益，
    /// 直接走与 `stw` 相同的 `collect_stw_inner`，使默认 `concurrent` 在常见负载下接近 STW。
    fn gc_collect_concurrent_phased(&mut self) -> usize {
        use std::sync::atomic::Ordering;

        // M:1：没有可与标记重叠的 mutator，STW 严格更优。
        if !self.mn_parallel {
            return self.gc.collect_stw_inner(self);
        }
        // 小堆：thread spawn + 双段握手主导墙钟。
        const CONCURRENT_MIN_TRACKED: usize = 256;
        if self.gc.tracked_count() < CONCURRENT_MIN_TRACKED {
            return self.gc.collect_stw_inner(self);
        }

        let mut stw_ns = 0u64;

        // Prepare（已在外层 STW 中）
        let prep_t0 = std::time::Instant::now();
        self.gc.concurrent_prepare_roots(self);
        stw_ns += prep_t0.elapsed().as_nanos() as u64;

        // 放行 mutator，进入并发标记（此段不算 STW）
        self.mn.end_stw();
        self.gc.concurrent_mark_drain();
        // 终止前在 mutator 仍跑时尽量排空脏卡，缩短第二段 STW。
        for _ in 0..8 {
            if self.gc.concurrent_flush_dirty_to_gray() == 0 {
                break;
            }
            self.gc.concurrent_mark_drain();
        }

        // 终止 + 清扫：再停世界
        let term_t0 = std::time::Instant::now();
        let stw2 = self.mn.begin_stw();
        if !stw2 {
            self.gc.set_marking(false);
            let n = self.mn.note_stw_failure();
            self.gc.stw_fallback_count.fetch_add(1, Ordering::Relaxed);
            if n == 1 || n.is_multiple_of(64) {
                eprintln!("optive: GC terminate STW 超时（累计 {n} 次），本轮回退冷却后重试");
            }
            let hold = self.gc.tracked_count();
            self.gc_auto_cooldown_hold_count = hold;
            self.gc_auto_cooldown_until = Some(std::time::Instant::now() + Self::gc_stw_cooldown());
            let stw3 = self.mn.begin_stw();
            let cleared = if stw3 {
                self.gc.collect_stw_inner(self)
            } else {
                0
            };
            stw_ns += term_t0.elapsed().as_nanos() as u64;
            self.gc.last_stw_ns.store(stw_ns, Ordering::Relaxed);
            return cleared;
        }
        let cleared = match self.gc.concurrent_terminate(self) {
            Some(marked) => self.gc.sweep_marked(&marked),
            None => {
                // 脏卡未收敛：完整 STW 收集，避免带着不完整 marked 清扫。
                self.gc.collect_stw_inner(self)
            }
        };
        stw_ns += term_t0.elapsed().as_nanos() as u64;
        self.gc.last_stw_ns.store(stw_ns, Ordering::Relaxed);
        // 保持 STW，由外层 end_stw
        cleared
    }

    fn gc_stw_cooldown() -> std::time::Duration {
        // 短冷却即可抑制风暴；勿与 STW 超时同量级（否则对照墙钟被冷却主导）。
        std::env::var("OPTIVE_GC_COOLDOWN_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n: &u64| n > 0)
            .map_or(
                std::time::Duration::from_millis(50),
                std::time::Duration::from_millis,
            )
    }

    /// 将本 Vm 局部根推入工作表（不含共享 fiber / parked / scheduled）。
    pub(crate) fn gc_push_local_roots(&self, worklist: &mut Vec<Value>) {
        for v in self.stack.get(..self.stack_sp).unwrap_or(&[]) {
            worklist.push(v.to_value());
        }
        for v in &self.lw_slots[..self.lw_sp.min(self.lw_slots.len())] {
            worklist.push(v.to_value());
        }
        for frame in &self.locals_stack {
            for v in frame {
                worklist.push(v.clone());
            }
        }
        for v in &self.script_globals {
            worklist.push(v.clone());
        }
        for v in self.globals.values() {
            worklist.push(v.clone());
        }
        let push_func = |worklist: &mut Vec<Value>, f: &Arc<FunctionObject>| {
            worklist.push(Value::Function(f.clone()));
            if let Some(cap) = &f.captured {
                worklist.extend(cap.values().cloned());
            }
        };
        for f in self.functions.values() {
            push_func(worklist, &f);
        }
        for f in &self.func_stack {
            push_func(worklist, f);
        }
        if let Some(exc) = &self.active_exception {
            worklist.push(exc.clone());
        }
        for it in &self.iterators {
            worklist.push(Value::Iterator(it.state.clone()));
        }
        for dt in self.convert_tables.values() {
            worklist.push(Value::Dispatch(dt.clone()));
        }
        for fns in self.overload_tables.values() {
            for f in fns {
                push_func(worklist, &f);
            }
        }
        for m in self.module_cache.values() {
            worklist.push(Value::Module(m.clone()));
        }
        for m in self.builtin_modules.values() {
            worklist.push(Value::Module(m.clone()));
        }
        if let Some(e) = &self.module_init_exports {
            for v in e.borrow().values() {
                worklist.push(v.clone());
            }
        }
        for snap in &self.macro_eval_scopes {
            for v in snap.globals.values() {
                worklist.push(v.clone());
            }
            for frame in &snap.locals_stack {
                for v in frame {
                    worklist.push(v.clone());
                }
            }
            for v in &snap.script_globals {
                worklist.push(v.clone());
            }
            for sv in &snap.stack {
                worklist.push(sv.to_value());
            }
            for f in snap.functions.values() {
                if let Some(cap) = &f.captured {
                    worklist.extend(cap.values().cloned());
                }
            }
        }
        for fr in &self.user_call_frames {
            if let Some(cap) = &fr.func.captured {
                worklist.extend(cap.values().cloned());
            }
        }
        if let Some(gen) = &self.active_generator {
            worklist.push(Value::Iterator(gen.clone()));
        }
        if let Some(v) = &self.pending_gen_yield {
            worklist.push(v.clone());
        }
        worklist.extend(self.gc.ffi_pins_snapshot());
    }

    fn gc_push_fiber_roots(fiber: &TaskFiber, worklist: &mut Vec<Value>) {
        for v in &fiber.stack {
            worklist.push(v.to_value());
        }
        for frame in &fiber.locals_stack {
            for v in frame {
                worklist.push(v.clone());
            }
        }
        for v in &fiber.lw_slots {
            worklist.push(v.to_value());
        }
        for f in &fiber.func_stack {
            worklist.push(Value::Function(f.clone()));
            if let Some(cap) = &f.captured {
                worklist.extend(cap.values().cloned());
            }
        }
        for it in &fiber.iterators {
            worklist.push(Value::Iterator(it.state.clone()));
        }
        for fr in &fiber.user_call_frames {
            if let Some(cap) = &fr.func.captured {
                worklist.extend(cap.values().cloned());
            }
        }
    }

    /// 完整根集：本地 + 就绪队列 + fiber 仓；M:N 再加 parked helper 根与已调度任务弱表。
    pub(crate) fn gc_push_all_roots(&self, worklist: &mut Vec<Value>) {
        self.gc_push_local_roots(worklist);
        for t in &self.ready_tasks {
            worklist.push(Value::Task(t.clone()));
        }
        for fiber in self.task_fibers.values() {
            Self::gc_push_fiber_roots(fiber, worklist);
        }
        if self.mn_parallel {
            {
                let fibers = self.mn.fibers.lock();
                for fiber in fibers.values() {
                    Self::gc_push_fiber_roots(fiber, worklist);
                }
            }
            worklist.extend(self.mn.take_parked_roots());
            worklist.extend(self.mn.scheduled_task_values());
        }
    }

    /// 兼容旧接口：推根并标记。
    #[allow(dead_code)]
    pub(crate) fn gc_mark_roots(&self, marked: &mut FxHashSet<usize>) {
        let mut worklist: Vec<Value> = Vec::new();
        self.gc_push_all_roots(&mut worklist);
        while let Some(v) = worklist.pop() {
            crate::gc::mark_value(&v, marked, &mut worklist);
        }
    }

    /// 安全点：发布本 Vm 根快照并响应 STW。
    pub(crate) fn poll_gc_safepoint(&mut self) {
        if !self.mn_parallel {
            return;
        }
        if !self
            .mn
            .stw_requested
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let mut roots = Vec::new();
        self.gc_push_local_roots(&mut roots);
        self.mn.poll_safepoint_with_roots(Some(roots));
    }

    pub fn register_builtin_module(&mut self, name: &str, module: Shared<ModuleObject>) {
        self.builtin_modules.insert(name.to_string(), module);
    }

    pub(crate) fn snapshot_for_module_init(&self) -> ModuleInitSnapshot {
        ModuleInitSnapshot {
            globals: self.globals.clone(),
            functions: self.functions.snapshot_map(),
            macros: self.macros.snapshot_map(),
            struct_defs: self.struct_defs.snapshot_map(),
            overload_tables: self.overload_tables.snapshot_map(),
            const_names: self.const_names.clone(),
            module_init_exports: self.module_init_exports.clone(),
            code: self.code.clone(),
            pc: self.pc,
            script_global_names: self.script_global_names.clone(),
            script_globals: self.script_globals.clone(),
            script_frame_slots: self.script_frame_slots,
            script_local_to_global: self.script_local_to_global.clone(),
            lw_slots: self.lw_slots.clone(),
            lw_bases: self.lw_bases.clone(),
            lw_bases_sp: self.lw_bases_sp,
            lw_sp: self.lw_sp,
            lw_base: self.lw_base,
            lw_depth: self.lw_depth,
        }
    }

    pub(crate) fn begin_module_init(
        &mut self,
        snap: &ModuleInitSnapshot,
        package_name: &str,
    ) -> Shared<HashMap<String, Value>> {
        // 模块 init 换新表，避免 clear 共享 Arc 误伤调用方 / 其他 worker
        self.globals = SharedMap::new();
        self.const_names.clear();
        self.has_const_names = false;
        self.pending_const.clear();
        self.has_pending_const = false;
        self.op_clear();
        self.locals_stack.clear();
        self.name_to_slot.clear();
        self.func_stack.clear();
        self.func_frames.clear();
        self.active_line_map = Arc::new(Vec::new());
        self.active_column_map = Arc::new(Vec::new());
        self.try_stack.clear();
        self.active_exception = None;
        self.iterators.clear();

        builtins::install_globals(self);
        type_registry::install_core_types(self);
        self.globals
            .insert("__package__".into(), Value::Text(package_name.to_string()));

        // 勿把调用方脚本快帧 flush 进模块 SharedMap；load_program 会重建模块帧。
        self.script_frame_slots = 0;
        self.script_local_to_global.clear();
        self.lw_bases_sp = 0;
        self.lw_sp = 0;
        self.lw_base = 0;
        self.lw_depth = 0;
        self.local_fn_hot.clear();

        let exports = Shared::new(HashMap::new());
        self.module_init_exports = Some(exports.clone());

        self.functions.replace_with(snap.functions.clone());
        self.macros.replace_with(snap.macros.clone());
        self.struct_defs.replace_with(snap.struct_defs.clone());
        self.overload_tables
            .replace_with(snap.overload_tables.clone());
        exports
    }

    pub(crate) fn finish_module_init(
        &mut self,
        snap: ModuleInitSnapshot,
        new_functions: HashMap<String, Arc<FunctionObject>>,
        new_macros: HashMap<String, Arc<MacroObject>>,
        new_struct_defs: HashMap<String, Arc<crate::value::StructDef>>,
        new_overloads: FxHashMap<String, Vec<Arc<FunctionObject>>>,
    ) {
        self.globals = snap.globals;
        self.functions.replace_with(snap.functions);
        self.macros.replace_with(snap.macros);
        self.struct_defs.replace_with(snap.struct_defs);
        self.overload_tables.replace_with(snap.overload_tables);
        self.const_names = snap.const_names;
        self.has_const_names = !self.const_names.is_empty();
        self.module_init_exports = snap.module_init_exports;
        self.code = snap.code;
        let hot = crate::hot_code::HotCode::encode(&self.code);
        self.hot_ops = hot.ops;
        self.hot_args = hot.args;
        self.pc = snap.pc;
        self.op_clear();
        self.locals_stack.clear();
        self.name_to_slot.clear();
        self.func_stack.clear();
        self.func_frames.clear();
        self.active_line_map = Arc::new(Vec::new());
        self.active_column_map = Arc::new(Vec::new());
        self.try_stack.clear();
        self.active_exception = None;
        self.iterators.clear();
        self.functions.extend(new_functions);
        self.macros.extend(new_macros);
        self.struct_defs.extend(new_struct_defs);
        self.overload_tables.extend(new_overloads);
        self.script_global_names = snap.script_global_names;
        self.script_globals = snap.script_globals;
        self.script_frame_slots = snap.script_frame_slots;
        self.script_local_to_global = snap.script_local_to_global;
        self.lw_slots = snap.lw_slots;
        self.lw_bases = snap.lw_bases;
        self.lw_bases_sp = snap.lw_bases_sp;
        self.lw_sp = snap.lw_sp;
        self.lw_base = snap.lw_base;
        self.lw_depth = snap.lw_depth;
        // 模块 run 填过 local_fn_hot；必须按调用方平行槽重建，否则 CallGlobal 会打到未挂 env 的模块函数。
        self.rebuild_local_fn_hot();
    }

    pub fn load_program(&mut self, program: CompiledProgram) -> Result<()> {
        // 先做一次性结构校验：畸形字节码在进入主循环前就干净报错，
        // 让热路径的安全索引有了显式保证（纵深防御）。
        if program.code.len() != program.hot.ops.len() {
            return Err(RuntimeError::msg(format!(
                "internal: program code/hot length mismatch ({} != {})",
                program.code.len(),
                program.hot.ops.len()
            )));
        }
        validate_hot_bytecode(&program.hot)?;
        for f in program.functions.values() {
            validate_function_hot(f)?;
        }
        for overloads in program.overload_tables.values() {
            for f in overloads {
                validate_function_hot(f)?;
            }
        }
        // 每个代码块使用全新操作数栈与调用状态——REPL 复用同一 Vm，
        // 不得把先前表达式的结果残留到后续语句之下。
        self.op_clear();
        self.user_call_frames.clear();
        self.func_stack.clear();
        self.func_frames.clear();
        self.locals_stack.clear();
        self.name_to_slot.clear();
        self.try_stack.clear();
        self.active_exception = None;
        self.user_call_deferred = false;
        self.fast_ret_sp = 0;
        self.lw_bases_sp = 0;
        self.lw_sp = 0;
        self.lw_base = 0;
        self.lw_depth = 0;
        self.pending_ret = None;
        self.hot_failed = false;
        self.hot_error = None;
        self.last_error_stack.clear();

        self.code = Arc::new(program.code);
        self.hot_ops = program.hot.ops.clone();
        self.hot_args = program.hot.args.clone();
        self.active_line_map = Arc::new(program.line_map);
        self.active_column_map = Arc::new(program.column_map);
        self.struct_defs.extend(program.struct_defs);
        self.enum_defs.extend(program.enum_defs);
        self.variant_defs.extend(program.variant_defs);
        self.functions.extend(program.functions);
        self.macros.extend(program.macros);
        self.overload_tables.extend(program.overload_tables);
        self.protocols.extend(program.protocols);
        self.globals
            .insert("__package__".into(), Value::Text("__main__".into()));
        self.pc = 0;
        // REPL 多次 load：热 Store 可能只写平行槽；重建表前刷入 SharedMap，
        // 否则下一行读到 NewVar 的 none（如 `acc = acc + 5`）。
        self.flush_script_globals_to_map();
        self.script_frame_slots = program.script_frame_slots;
        self.script_local_to_global = program.script_local_to_global;
        self.init_script_globals(program.global_names);
        self.prepare_script_fast_frame();
        type_registry::install_core_types(self);
        Ok(())
    }

    /// 重置解释器执行状态（保留已加载程序与全局定义），供 benchmark / REPL 复用同一 VM。
    pub fn reset_execution(&mut self) {
        self.pc = 0;
        self.op_clear();
        self.locals_stack.clear();
        self.name_to_slot.clear();
        self.user_call_frames.clear();
        self.func_stack.clear();
        self.func_frames.clear();
        self.try_stack.clear();
        self.active_exception = None;
        self.iterators.clear();
        self.user_call_deferred = false;
        self.fast_ret_sp = 0;
        self.lw_slots.clear();
        self.lw_bases_sp = 0;
        self.lw_sp = 0;
        self.lw_base = 0;
        self.lw_depth = 0;
        self.lw_entry_pc = 0;
        self.lw_frame_slots = 0;
        self.pending_ret = None;
        self.hot_failed = false;
        self.hot_error = None;
        self.local_fn_hot.clear();
        self.prepare_script_fast_frame();
    }

    /// 清掉顶层 `const` 绑定并回到入口，使同一已加载程序可再跑一遍。
    ///
    /// 保留 M:N worker 池。`reset_execution` 不够：`const let` 仍在 `const_names` 里。
    pub fn reset_script_bindings(&mut self) {
        self.reset_execution();
        self.const_names.clear();
        self.has_const_names = false;
        self.pending_const.clear();
        self.has_pending_const = false;
    }

    pub(crate) fn snapshot_module_global_env(&mut self) -> ModuleGlobalEnv {
        self.flush_script_fast_locals();
        self.flush_script_globals_to_map();
        let mut map = self.globals.deep_clone();
        // 顶层热路径可能只更新 script_globals；快照前合并。
        // 槽为 none 时勿覆盖 SharedMap 里已有的非 none（NewVar 初值 none + Store 只写了 map）。
        for (idx, name) in self.script_global_names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            if let Some(v) = self.script_globals.get(idx) {
                if matches!(v, Value::None)
                    && map.get(name).is_some_and(|x| !matches!(x, Value::None))
                {
                    continue;
                }
                map.insert(name.clone(), v.clone());
            }
        }
        ModuleGlobalEnv {
            global_names: self.script_global_names.clone(),
            globals: std::sync::Arc::new(crate::shared::SyncCell::new(map.into_iter().collect())),
            finalized: true,
        }
    }

    /// 将平行槽刷回 SharedMap（M:1 热 `StoreGlobal` 可延迟同步；调度其它纤程前必须刷）。
    #[inline]
    fn flush_script_globals_to_map(&mut self) {
        self.flush_script_fast_locals();
        if self.mn_parallel || !self.script_globals_map_dirty {
            return;
        }
        for (idx, name) in self.script_global_names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            let Some(v) = self.script_globals.get(idx).cloned() else {
                continue;
            };
            // `del` 已移除键且槽为 none：不得 insert 把绑定「复活」。
            if matches!(v, Value::None) && !self.globals.contains_key(name.as_str()) {
                continue;
            }
            if !self.globals.set_inplace(name.as_str(), v.clone()) {
                self.globals.insert(name.clone(), v);
            }
        }
        self.script_globals_map_dirty = false;
    }

    fn init_script_globals(&mut self, names: Vec<String>) {
        self.script_global_names = names;
        self.script_globals = self
            .script_global_names
            .iter()
            .map(|name| self.globals.get(name).unwrap_or(Value::None))
            .collect();
        self.publish_script_globals();
        self.rebuild_local_fn_hot();
    }

    fn prepare_script_fast_frame(&mut self) {
        let n = self.script_frame_slots;
        if n == 0 {
            return;
        }
        if self.lw_slots.len() < n {
            self.lw_slots.resize(n, StackVal::Empty);
        }
        for slot in self.lw_slots.iter_mut().take(n) {
            *slot = StackVal::Empty;
        }
        self.lw_sp = n;
        self.lw_base = 0;
        self.lw_depth = 1;
        self.lw_bases_sp = 0;
        self.push_lw_base(0);
    }

    fn script_fast_slot(&self, local: usize) -> Option<&StackVal> {
        if self.lw_bases_sp == 0 {
            return None;
        }
        let base = self.lw_bases[0];
        let idx = base + local;
        if idx < self.lw_sp {
            Some(&self.lw_slots[idx])
        } else {
            None
        }
    }

    fn live_script_fast_local(&self, name: &str) -> Option<Value> {
        let global = self.script_global_names.iter().position(|n| n == name)?;
        let local = self
            .script_local_to_global
            .iter()
            .find(|&&(_, g)| g == global)
            .map(|&(l, _)| l)?;
        match self.script_fast_slot(local)? {
            StackVal::Empty => None,
            other => Some(other.copy_imm().into_value()),
        }
    }

    pub(crate) fn flush_script_fast_locals(&mut self) {
        if self.script_local_to_global.is_empty() || self.lw_bases_sp == 0 {
            return;
        }
        let pairs: Vec<(usize, usize)> = self.script_local_to_global.clone();
        for (local, global) in pairs {
            let Some(sv) = self.script_fast_slot(local).map(StackVal::copy_imm) else {
                continue;
            };
            if global >= self.script_globals.len() {
                continue;
            }
            let val = sv.into_value();
            self.script_globals[global] = val.clone();
            if let Some(name) = self.script_global_names.get(global) {
                if !name.is_empty() && !self.globals.set_inplace(name.as_str(), val.clone()) {
                    self.globals.insert(name.clone(), val.clone());
                }
            }
            self.sync_local_fn_hot(global, &val);
        }
    }

    fn publish_script_globals(&self) {
        if !self.mn_parallel {
            return;
        }
        self.mn.publish_script_globals(
            self.script_global_names.clone(),
            self.script_globals.clone(),
        );
    }

    /// M:N：各 worker 的 `script_globals` 互不共享；SharedMap 才是跨线程权威源。
    fn overlay_script_globals_from_map(&mut self) {
        for (i, name) in self.script_global_names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            // 主线程未逃逸快槽（含尚未赋值的 Empty）以 lw 为准，勿被 map 盖掉。
            if self.mn_primary
                && self.lw_bases_sp > 0
                && self.script_local_to_global.iter().any(|&(_, g)| g == i)
            {
                continue;
            }
            if let Some(v) = self.globals.get(name) {
                if i < self.script_globals.len() {
                    self.script_globals[i] = v;
                }
            }
        }
    }

    fn pull_script_globals_if_helper(&mut self) {
        if !self.mn_parallel || self.mn_primary {
            return;
        }
        let (names, vals) = self.mn.snapshot_script_globals();
        if names.is_empty() {
            return;
        }
        self.script_global_names = names;
        self.script_globals = vals;
        // 发布快照可能早于 helper 上的 StoreGlobal；随后以 SharedMap 覆盖。
        self.overlay_script_globals_from_map();
        self.rebuild_local_fn_hot();
    }

    fn localize_function(f: &FunctionObject) -> Arc<FunctionObject> {
        let mut c = f.clone();
        c.hot.ops = Arc::from(f.hot.ops.as_ref());
        c.hot.args = Arc::from(f.hot.args.as_ref());
        c.body = Arc::new((*f.body).clone());
        c.line_map = Arc::new((*f.line_map).clone());
        c.column_map = Arc::new((*f.column_map).clone());
        Arc::new(c)
    }

    fn rebuild_local_fn_hot(&mut self) {
        self.local_fn_hot.clear();
        self.local_fn_hot.reserve(self.script_globals.len());
        for v in &self.script_globals {
            let slot = match v {
                Value::Function(f) => Some(Self::localize_function(f)),
                Value::Cell(c) => match &*c.borrow() {
                    Value::Function(f) => Some(Self::localize_function(f)),
                    _ => None,
                },
                _ => None,
            };
            self.local_fn_hot.push(slot);
        }
    }

    fn sync_local_fn_hot(&mut self, idx: usize, val: &Value) {
        if idx >= self.local_fn_hot.len() {
            return;
        }
        self.local_fn_hot[idx] = match val {
            Value::Function(f) => Some(Self::localize_function(f)),
            Value::Cell(c) => match &*c.borrow() {
                Value::Function(f) => Some(Self::localize_function(f)),
                _ => None,
            },
            _ => None,
        };
    }

    /// 按名字写入全局；若该名在 script 表中则同步槽位。
    pub(crate) fn store_global_by_name(&mut self, name: &str, val: Value) {
        if let Some(Value::Cell(cell)) = self.globals.get(name) {
            *cell.borrow_mut() = val.clone();
        } else {
            self.globals.insert(name.to_string(), val.clone());
        }
        if let Some(script_idx) = self.script_global_names.iter().position(|n| n == name) {
            if script_idx < self.script_globals.len() {
                self.script_globals[script_idx] = val.clone();
            }
            self.sync_local_fn_hot(script_idx, &val);
        }
    }

    fn active_module_global_env(&self) -> Option<&crate::opcode::ModuleGlobalEnv> {
        self.user_call_frames
            .iter()
            .rev()
            .find_map(|frame| frame.func.module_env.as_deref())
    }

    fn load_script_global(&self, idx: usize) -> Result<Value> {
        if let Some(env) = self.active_module_global_env() {
            if idx < self.script_globals.len()
                && env.global_names.get(idx) == self.script_global_names.get(idx)
            {
                if let Some(v) = self.script_globals.get(idx) {
                    if !matches!(v, Value::None) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
            }
            let Some(name) = env.global_names.get(idx) else {
                return Err(RuntimeError::msg(format!(
                    "internal: LoadGlobal({idx}) out of range for function global table (len {})",
                    env.global_names.len()
                )));
            };
            if name.is_empty() {
                return Err(RuntimeError::msg(format!(
                    "internal: LoadGlobal({idx}) resolves to empty global name"
                )));
            }
            // M:N helper：先读任务开始时拉下的本地槽，避免每条 LoadGlobal 抢 SharedMap。
            if let Some(script_idx) = self.script_global_names.iter().position(|n| n == name) {
                if let Some(v) = self.script_globals.get(script_idx) {
                    if !matches!(v, Value::None) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
            }
            // 优先用模块快照；否则回退到活动 globals，以便 REPL 前向引用
            //（先定义 `a` 再定义 `b`）仍能解析，并使 LoadGlobal 下标绑定到
            // 函数编译期的名字，即使后来 `load_program` 替换了 `script_global_names`。
            if let Some(v) = env.globals.borrow().get(name.as_str()) {
                // 快照里可能是 NewVar 的 none，而顶层热 Store 只写了平行槽。
                if !matches!(v, Value::None) {
                    return Ok(match v {
                        Value::Cell(c) => c.borrow().clone(),
                        other => other.clone(),
                    });
                }
            }
            // 热路径平行槽优先于 NewVar 的 none / 过期快照。
            if let Some(script_idx) = self.script_global_names.iter().position(|n| n == name) {
                if let Some(v) = self.script_globals.get(script_idx) {
                    if !matches!(v, Value::None) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
            }
            return match self.globals.get(name.as_str()) {
                Some(Value::Cell(c)) => Ok(c.borrow().clone()),
                Some(v) => Ok(v),
                None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
            };
        }
        // 顶层：非 none 平行槽是热 Store 的权威来源；none 槽回退 SharedMap
        //（friend/`__register_dispatch__` 可能只写 map；`del` 后无键 → NameError）。
        if let Some(v) = self.script_globals.get(idx) {
            let name = self
                .script_global_names
                .get(idx)
                .map_or("", std::string::String::as_str);
            if name.is_empty() {
                return Err(RuntimeError::msg(format!(
                    "internal: LoadGlobal({idx}) resolves to empty global name"
                )));
            }
            if !matches!(v, Value::None) {
                return Ok(match v {
                    Value::Cell(c) => c.borrow().clone(),
                    other => other.clone(),
                });
            }
            return match self.globals.get(name) {
                Some(Value::Cell(c)) => Ok(c.borrow().clone()),
                Some(v) => Ok(v),
                None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
            };
        }
        let Some(name) = self.script_global_names.get(idx) else {
            return Err(RuntimeError::msg(format!(
                "internal: LoadGlobal({idx}) out of range for script global table (len {})",
                self.script_global_names.len()
            )));
        };
        if name.is_empty() {
            return Err(RuntimeError::msg(format!(
                "internal: LoadGlobal({idx}) resolves to empty global name"
            )));
        }
        match self.globals.get(name.as_str()) {
            Some(Value::Cell(c)) => Ok(c.borrow().clone()),
            Some(v) => Ok(v),
            None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
        }
    }

    /// 顶层脚本全局槽写入（无 `module_env）。同步` `script_globals` 与 `globals`。
    #[inline(always)]
    fn store_script_global_top(&mut self, idx: usize, val: Value) -> Result<()> {
        if idx >= self.script_global_names.len() {
            return Err(RuntimeError::msg(format!(
                "internal: StoreGlobal({idx}) out of range for script global table (len {})",
                self.script_global_names.len()
            )));
        }
        if self.script_global_names[idx].is_empty() {
            return Err(RuntimeError::msg(format!(
                "internal: StoreGlobal({idx}) resolves to empty global name"
            )));
        }
        if self.has_const_names
            && self
                .const_names
                .contains(self.script_global_names[idx].as_str())
        {
            return Err(RuntimeError::msg(format!(
                "cannot assign to const binding: {}",
                self.script_global_names[idx]
            )));
        }
        if idx < self.script_globals.len() {
            self.script_globals[idx] = val.clone();
        }
        self.sync_local_fn_hot(idx, &val);
        if !self
            .globals
            .set_inplace(self.script_global_names[idx].as_str(), val.clone())
        {
            self.globals
                .insert(self.script_global_names[idx].clone(), val);
        }
        let name = self.script_global_names[idx].clone();
        self.finalize_const_init(&name);
        Ok(())
    }

    /// `StoreGlobal` 完整语义（含 `module_env）；热/冷路径共用`。
    fn exec_store_global(&mut self, idx: usize, val: Value) -> Result<()> {
        if self.user_call_frames.is_empty() {
            return self.store_script_global_top(idx, val);
        }
        // 与 LoadGlobal 一致：有活动函数时按该函数 module_env 的名字解析下标，
        // 避免 REPL/二次 load_program 替换 script_global_names 后写错槽。
        let name = if let Some(env) = self.active_module_global_env() {
            env.global_names.get(idx).cloned().ok_or_else(|| {
                RuntimeError::msg(format!(
                    "internal: StoreGlobal({idx}) out of range for function global table (len {})",
                    env.global_names.len()
                ))
            })?
        } else {
            self.script_global_names.get(idx).cloned().ok_or_else(|| {
                RuntimeError::msg(format!(
                    "internal: StoreGlobal({idx}) out of range for script global table (len {})",
                    self.script_global_names.len()
                ))
            })?
        };
        if name.is_empty() {
            return Err(RuntimeError::msg(format!(
                "internal: StoreGlobal({idx}) resolves to empty global name"
            )));
        }
        if self.const_names.contains(name.as_str()) {
            return Err(RuntimeError::msg(format!(
                "cannot assign to const binding: {name}"
            )));
        }
        // 克隆 Rc，避免与 `finalize_const_init` 的 &mut self 借权冲突。
        let module_env = self
            .user_call_frames
            .iter()
            .rev()
            .find_map(|frame| frame.func.module_env.clone());
        if let Some(env) = module_env {
            // 写入模块快照，使导入后模块函数的赋值留在模块内。
            {
                let mut g = env.globals.borrow_mut();
                if let Some(Value::Cell(cell)) = g.get(name.as_str()) {
                    *cell.borrow_mut() = val.clone();
                } else {
                    g.insert(name.clone(), val.clone());
                }
            }
            // 主脚本/REPL：名字在 live globals 或 script 表里时必须同步，
            // 否则函数内 StoreGlobal 只改快照，顶层读到旧值。
            if self.globals.contains_key(name.as_str())
                || self.script_global_names.iter().any(|n| n == &name)
            {
                self.store_global_by_name(&name, val);
            }
            self.finalize_const_init(&name);
        } else {
            self.store_global_by_name(&name, val);
            self.finalize_const_init(&name);
        }
        Ok(())
    }

    fn load_script_global_by_name(&self, name: &str) -> Option<Value> {
        self.script_global_names
            .iter()
            .position(|n| n == name)
            .and_then(|idx| self.load_script_global(idx).ok())
    }

    fn alloc_local_frame(&mut self, size: usize) -> Vec<Value> {
        if let Some(mut frame) = self.local_frame_pool.pop() {
            if frame.capacity() >= size {
                frame.clear();
                frame.resize(size, Value::None);
                return frame;
            }
        }
        vec![Value::None; size]
    }

    fn recycle_local_frame(&mut self, mut frame: Vec<Value>) {
        if self.local_frame_pool.len() < 64 {
            frame.clear();
            self.local_frame_pool.push(frame);
        }
    }

    #[inline]
    const fn jump_to_pc(&mut self, pc: usize) {
        self.pc = pc;
    }

    /// 返回栈顶元素为 [Value]；空栈时返回 None。
    pub fn stack_top(&self) -> Value {
        if self.stack_sp == 0 {
            Value::None
        } else {
            self.stack[self.stack_sp - 1].to_value()
        }
    }

    #[inline(always)]
    fn ensure_op_stack(&mut self, min_cap: usize) {
        if self.stack.len() < min_cap {
            self.stack.resize(min_cap, StackVal::Empty);
        }
    }

    #[inline(always)]
    fn op_push(&mut self, v: StackVal) {
        let sp = self.stack_sp;
        if sp < self.stack.len() {
            // SAFETY: sp < stack.len() 刚检查。
            unsafe {
                *self.stack.get_unchecked_mut(sp) = v;
            }
        } else {
            self.stack.push(v);
        }
        self.stack_sp = sp + 1;
    }

    #[inline(always)]
    fn op_push_int(&mut self, n: i64) {
        let sp = self.stack_sp;
        if sp < self.stack.len() {
            // SAFETY: sp < stack.len() 刚检查。
            unsafe {
                *self.stack.get_unchecked_mut(sp) = StackVal::Int(n);
            }
        } else {
            self.stack.push(StackVal::Int(n));
        }
        self.stack_sp = sp + 1;
    }

    #[inline(always)]
    fn op_push_bool(&mut self, b: bool) {
        let sp = self.stack_sp;
        if sp < self.stack.len() {
            // SAFETY: sp < stack.len() 刚检查。
            unsafe {
                *self.stack.get_unchecked_mut(sp) = StackVal::Bool(b);
            }
        } else {
            self.stack.push(StackVal::Bool(b));
        }
        self.stack_sp = sp + 1;
    }

    /// 弹出：Int/Bool/Empty 用 `mem::replace` 取出（编译器可优化为寄存器搬运）；
    /// 仅 Heap 需清空槽位以防双重释放。
    #[inline(always)]
    fn op_pop(&mut self) -> StackVal {
        debug_assert!(self.stack_sp > 0);
        let sp = self.stack_sp - 1;
        self.stack_sp = sp;
        // SAFETY: sp < stack_sp（旧值）<= stack.len()，故 sp 在界内。
        // 调用方保证 stack_sp > 0（debug_assert 校验；release 下由上层 pop_hot / 冷路径守卫）。
        unsafe { std::mem::replace(self.stack.get_unchecked_mut(sp), StackVal::Empty) }
    }

    /// 二元 Int 运算就地完成：读 TOS/TOS1，写回 TOS1，sp-=1。成功返回 true。
    #[inline(always)]
    fn binop_ints_inplace(&mut self, f: impl FnOnce(i64, i64) -> Option<i64>) -> bool {
        let sp = self.stack_sp;
        if sp < 2 {
            return false;
        }
        // SAFETY: sp >= 2 且 sp <= stack.len()（栈不变量：stack_sp <= stack.len()），
        // 故 sp-2、sp-1 均在界内。先把 i64 拷出来（Copy），结束对 stack 的不可变借用，
        // 再写回，避免借用冲突。
        let (xr, yr) = unsafe {
            match (
                self.stack.get_unchecked(sp - 2),
                self.stack.get_unchecked(sp - 1),
            ) {
                (StackVal::Int(x), StackVal::Int(y)) => (*x, *y),
                _ => return false,
            }
        };
        if let Some(r) = f(xr, yr) {
            // SAFETY: 同上，sp-2 在界内。
            unsafe {
                *self.stack.get_unchecked_mut(sp - 2) = StackVal::Int(r);
            }
            self.stack_sp = sp - 1;
            true
        } else {
            false
        }
    }

    /// 二元 Int 比较就地：结果 Bool 写在 TOS1，sp-=1。
    #[inline(always)]
    fn cmp_ints_inplace(&mut self, f: impl FnOnce(i64, i64) -> bool) -> bool {
        let sp = self.stack_sp;
        if sp < 2 {
            return false;
        }
        // SAFETY: sp >= 2 且 sp <= stack.len()，故 sp-2、sp-1 在界内。
        let (xr, yr) = unsafe {
            match (
                self.stack.get_unchecked(sp - 2),
                self.stack.get_unchecked(sp - 1),
            ) {
                (StackVal::Int(x), StackVal::Int(y)) => (*x, *y),
                _ => return false,
            }
        };
        // SAFETY: 同上。
        unsafe {
            *self.stack.get_unchecked_mut(sp - 2) = StackVal::Bool(f(xr, yr));
        }
        self.stack_sp = sp - 1;
        true
    }

    #[inline(always)]
    fn op_clear(&mut self) {
        self.op_truncate(0);
    }

    #[inline(always)]
    fn op_truncate(&mut self, len: usize) {
        if len < self.stack_sp {
            for i in len..self.stack_sp {
                self.stack[i] = StackVal::Empty;
            }
            self.stack_sp = len;
        }
    }

    #[inline(always)]
    fn op_last_value(&self) -> Option<Value> {
        if self.stack_sp == 0 {
            None
        } else {
            Some(self.stack[self.stack_sp - 1].to_value())
        }
    }

    #[inline]
    fn push_value(&mut self, v: Value) {
        self.track_value(&v);
        self.op_push(StackVal::from_value(v));
        self.maybe_auto_gc();
    }

    #[inline(always)]
    fn push_int(&mut self, n: i64) {
        self.op_push_int(n);
    }

    #[inline(always)]
    fn push_bool(&mut self, b: bool) {
        self.op_push_bool(b);
    }

    #[inline]
    fn push_none(&mut self) {
        self.op_push(StackVal::Empty);
    }

    #[inline(always)]
    fn set_hot_error(&mut self, e: RuntimeError) {
        self.hot_error = Some(e);
        self.hot_failed = true;
    }

    #[inline(always)]
    fn push_fast_ret(&mut self, pc: usize) {
        let sp = self.fast_ret_sp;
        if sp < self.fast_ret_pcs.len() {
            // SAFETY: sp < fast_ret_pcs.len() 刚检查。
            self.fast_ret_pcs[sp] = pc;
        } else {
            self.fast_ret_pcs.push(pc);
        }
        self.fast_ret_sp = sp + 1;
    }

    #[inline(always)]
    fn pop_fast_ret(&mut self) -> Option<usize> {
        let sp = self.fast_ret_sp;
        if sp == 0 {
            return None;
        }
        let sp = sp - 1;
        self.fast_ret_sp = sp;
        // SAFETY: sp < fast_ret_sp（旧值）<= fast_ret_pcs.len()。
        Some(self.fast_ret_pcs[sp])
    }

    #[inline(always)]
    fn push_lw_base(&mut self, base: usize) {
        let sp = self.lw_bases_sp;
        if sp < self.lw_bases.len() {
            // SAFETY: sp < lw_bases.len() 刚检查。
            self.lw_bases[sp] = base;
        } else {
            self.lw_bases.push(base);
        }
        self.lw_bases_sp = sp + 1;
    }

    #[inline(always)]
    fn call_self_lightweight(&mut self, argc: usize, entry_pc: usize, frame_slots: usize) {
        if self.lw_depth >= self.cached_max_depth {
            self.set_hot_error(RuntimeError::recursion_err(
                "maximum recursion depth exceeded",
            ));
            return;
        }
        self.push_fast_ret(self.pc);
        let base = self.lw_sp;
        self.push_lw_base(base);
        self.lw_base = base;
        self.lw_depth += 1;
        let nslots = frame_slots.max(argc);
        let need = base + nslots;
        if need > self.lw_slots.len() {
            self.lw_slots.resize(need, StackVal::Empty);
        }
        for i in (0..argc).rev() {
            self.lw_slots[base + i] = self.pop_hot();
        }
        for i in argc..nslots {
            self.lw_slots[base + i] = StackVal::Empty;
        }
        self.lw_sp = need;
        self.pc = entry_pc;
    }

    fn local_set(&mut self, slot: usize, val: Value) {
        self.track_value(&val);
        if self.lw_depth != 0 {
            let idx = self.lw_base + slot;
            if idx >= self.lw_slots.len() {
                self.lw_slots.resize(idx + 1, StackVal::Empty);
            }
            if idx >= self.lw_sp {
                self.lw_sp = idx + 1;
            }
            self.lw_slots[idx] = StackVal::from_value(val);
            return;
        }
        if let Some(frame) = self.locals_stack.last_mut() {
            if slot >= frame.len() {
                frame.resize(slot + 1, Value::None);
            }
            frame[slot] = val;
        }
    }

    fn scope_name_map_mut(&mut self, frame: usize) -> &mut FxHashMap<String, usize> {
        self.name_to_slot[frame].get_or_insert_with(FxHashMap::default)
    }

    #[inline(always)]
    fn store_fast_sv(&mut self, slot: usize, val: StackVal) {
        if self.lw_depth != 0 {
            let idx = self.lw_base + slot;
            if idx >= self.lw_slots.len() {
                self.lw_slots.resize(idx + 1, StackVal::Empty);
            }
            if idx >= self.lw_sp {
                self.lw_sp = idx + 1;
            }
            // SAFETY: 上面已保证 idx < lw_slots.len()。
            unsafe {
                *self.lw_slots.get_unchecked_mut(idx) = val;
            }
            return;
        }
        if let Some(frame) = self.locals_stack.last_mut() {
            if slot < frame.len() {
                if let Value::Cell(cell) = &frame[slot] {
                    *cell.borrow_mut() = val.into_value();
                    return;
                }
            }
        }
        self.local_set(slot, val.into_value());
    }

    #[inline(always)]
    fn lw_slot_int(&self, slot: usize) -> Option<i64> {
        if self.lw_depth == 0 {
            return None;
        }
        let idx = self.lw_base + slot;
        if idx >= self.lw_sp {
            return None;
        }
        match unsafe { self.lw_slots.get_unchecked(idx) } {
            StackVal::Int(n) => Some(*n),
            _ => None,
        }
    }

    fn push_fast_cmp_imm(
        &mut self,
        slot: usize,
        imm: i64,
        pred: impl Fn(std::cmp::Ordering) -> bool,
    ) -> Result<()> {
        let sv = self.load_fast_sv(slot);
        if let StackVal::Int(n) = sv {
            self.op_push_bool(pred(n.cmp(&imm)));
            return Ok(());
        }
        let a = sv.into_value();
        let ord = match &a {
            Value::Num(n) => n.cmp_num(&Num::Small(imm)),
            _ => {
                return Err(RuntimeError::type_err(format!(
                    "cannot compare {} with number",
                    a.type_name()
                )));
            }
        };
        self.op_push_bool(pred(ord));
        Ok(())
    }

    #[inline(always)]
    fn load_fast_sv(&self, slot: usize) -> StackVal {
        if self.lw_depth != 0 {
            let idx = self.lw_base + slot;
            if idx < self.lw_sp {
                // SAFETY: idx < lw_sp <= lw_slots.len()。
                return unsafe { self.lw_slots.get_unchecked(idx).copy_imm() };
            }
            return StackVal::Empty;
        }
        if let Some(frame) = self.locals_stack.last() {
            if slot < frame.len() {
                // SAFETY: slot < frame.len()。
                return match &frame[slot] {
                    Value::Cell(c) => match &*c.borrow() {
                        Value::Num(Num::Small(n)) => StackVal::Int(*n),
                        Value::None => StackVal::Empty,
                        Value::Bool(b) => StackVal::Bool(*b),
                        v => StackVal::from_value(v.clone()),
                    },
                    Value::Num(Num::Small(n)) => StackVal::Int(*n),
                    Value::None => StackVal::Empty,
                    Value::Bool(b) => StackVal::Bool(*b),
                    v => StackVal::from_value(v.clone()),
                };
            }
        }
        StackVal::Empty
    }

    #[inline(always)]
    fn pop_lightweight_frame(&mut self) {
        let n = self.lw_bases_sp;
        if n == 0 {
            return;
        }
        let n = n - 1;
        self.lw_bases_sp = n;
        // SAFETY: n < lw_bases_sp（旧值）<= lw_bases.len()。
        let base = self.lw_bases[n];
        // 只清理 Heap 槽（防止 Rc/Box 延迟释放）；Int/Bool/Empty 无析构，
        // 下次 call_self 会覆盖，跳过可省去 ~2.7M 次/帧的空写。
        for i in base..self.lw_sp {
            if matches!(self.lw_slots[i], StackVal::Heap(_) | StackVal::Func(_)) {
                self.lw_slots[i] = StackVal::Empty;
            }
        }
        self.lw_sp = base;
        self.lw_depth -= 1;
        self.lw_base = if n == 0 {
            0
        } else {
            // SAFETY: n >= 1 且 n < lw_bases.len()。
            self.lw_bases[n - 1]
        };
    }

    #[inline(always)]
    fn pop_sv(&mut self) -> Result<StackVal> {
        if self.stack_sp == 0 {
            return Err(RuntimeError::msg("stack underflow"));
        }
        Ok(self.op_pop())
    }

    /// 热路径弹出：不返回 Result；下溢时标记热失败并返回 Empty。
    #[inline(always)]
    fn pop_hot(&mut self) -> StackVal {
        if self.stack_sp == 0 {
            debug_assert!(false, "stack underflow on hot path");
            self.set_hot_error(RuntimeError::msg("stack underflow"));
            return StackVal::Empty;
        }
        self.op_pop()
    }

    /// 通用加法慢路径：快路径类型不匹配时分发到运算符重载。
    #[inline]
    fn exec_add_slow(&mut self, a: StackVal, b: StackVal) -> Result<()> {
        let av = a.into_value();
        let bv = b.into_value();
        let result = match (&av, &bv) {
            (Value::Num(_), Value::Num(_)) => av.add(&bv)?,
            (Value::Text(_), Value::Text(_)) => av.add(&bv)?,
            (Value::List(_), Value::List(_)) => av.add(&bv)?,
            _ => self.dispatch_binary_arith(
                &av,
                &bv,
                "__add__",
                "__radd__",
                super::value::Value::add,
            )?,
        };
        self.push_value(result);
        Ok(())
    }

    #[inline]
    fn exec_sub_slow(&mut self, a: StackVal, b: StackVal) -> Result<()> {
        let av = a.into_value();
        let bv = b.into_value();
        let result = match (&av, &bv) {
            (Value::Num(_), Value::Num(_)) => av.sub(&bv)?,
            _ => self.dispatch_binary_arith(
                &av,
                &bv,
                "__sub__",
                "__rsub__",
                super::value::Value::sub,
            )?,
        };
        self.push_value(result);
        Ok(())
    }

    #[inline]
    fn exec_add_text_text(&mut self) -> Result<()> {
        let b = self.pop()?;
        let a = self.pop()?;
        match (a, b) {
            (Value::Text(x), Value::Text(y)) => {
                self.push_value(Value::Text(format!("{x}{y}")));
            }
            (a, b) => {
                let result = a.add(&b)?;
                self.push_value(result);
            }
        }
        Ok(())
    }

    #[inline]
    fn exec_add_list_list(&mut self) -> Result<()> {
        let b = self.pop()?;
        let a = self.pop()?;
        match (a, b) {
            (Value::List(x), Value::List(y)) => {
                let mut out = x.borrow().clone();
                out.extend(y.borrow().iter().cloned());
                let val = Value::List(Shared::new(out));
                self.track_value(&val);
                self.push_value(val);
            }
            (a, b) => {
                let result = a.add(&b)?;
                self.push_value(result);
            }
        }
        Ok(())
    }

    #[inline]
    fn exec_mul_num(&mut self) -> Result<()> {
        let rhs = self.pop_hot();
        let lhs = self.pop_hot();
        match (lhs, rhs) {
            (StackVal::Int(left), StackVal::Int(right)) => match left.checked_mul(right) {
                Some(product) => self.op_push(StackVal::Int(product)),
                None => self.push_value(Value::Num(Num::from_bigint(
                    num_bigint::BigInt::from(left) * num_bigint::BigInt::from(right),
                ))),
            },
            (lhs, rhs) => {
                let av = lhs.into_value();
                let bv = rhs.into_value();
                let result = match (&av, &bv) {
                    (Value::Num(_), Value::Num(_)) => av.mul(&bv)?,
                    _ => self
                        .dispatch_binary_arith(&av, &bv, "__mul__", "__rmul__", |x, y| x.mul(y))?,
                };
                self.push_value(result);
            }
        }
        Ok(())
    }

    #[inline]
    fn exec_div_num(&mut self) -> Result<()> {
        let b = self.pop()?;
        let a = self.pop()?;
        let result = match (&a, &b) {
            (Value::Num(_), Value::Num(_)) => a.div(&b)?,
            _ => self.dispatch_binary_arith(&a, &b, "__div__", "__rdiv__", |x, y| x.div(y))?,
        };
        self.push_value(result);
        Ok(())
    }

    /// 取模慢路径：`H_MOD_NUM` 热路径 Int 快路径失败（除零 / 非 Int）时调用。
    /// 语义与冷路径 `StepAction::Mod` 一致：Int 除零报错，非 Int 走 `__mod__`/`__rmod__`。
    #[inline]
    fn exec_mod_num(&mut self) -> Result<()> {
        let rhs = self.pop_hot();
        let lhs = self.pop_hot();
        match (lhs, rhs) {
            (StackVal::Int(left), StackVal::Int(right)) => {
                if right == 0 {
                    return Err(RuntimeError::zero_div_diag());
                }
                if left == i64::MIN && right == -1 {
                    // 数学上 MIN % -1 == 0；避免 Rust 溢出 panic（BigInt 路径无此问题）。
                    self.op_push(StackVal::Int(0));
                    return Ok(());
                }
                // Python-style：余数符号跟随除数（与 rem_num 一致）。
                let mut rem = left % right;
                if rem != 0 && ((rem < 0) != (right < 0)) {
                    rem += right;
                }
                self.op_push(StackVal::Int(rem));
                Ok(())
            }
            (lhs, rhs) => {
                let av = lhs.into_value();
                let bv = rhs.into_value();
                let result = match (&av, &bv) {
                    (Value::Num(_), Value::Num(_)) => av.rem(&bv)?,
                    _ => self
                        .dispatch_binary_arith(&av, &bv, "__mod__", "__rmod__", |x, y| x.rem(y))?,
                };
                self.push_value(result);
                Ok(())
            }
        }
    }

    #[inline]
    fn exec_cmp_num(
        &mut self,
        method: &str,
        pred: impl Fn(std::cmp::Ordering) -> bool,
    ) -> Result<()> {
        let b = self.pop_hot();
        let a = self.pop_hot();
        let result = if let (StackVal::Int(x), StackVal::Int(y)) = (&a, &b) {
            pred(x.cmp(y))
        } else {
            let av = a.to_value();
            let bv = b.to_value();
            match (&av, &bv) {
                (Value::Num(_), Value::Num(_)) => {
                    let c = compare_num(&av, &bv)?;
                    pred(if c < 0 {
                        std::cmp::Ordering::Less
                    } else if c > 0 {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    })
                }
                (Value::Text(x), Value::Text(y)) => pred(x.as_str().cmp(y.as_str())),
                _ => {
                    self.dispatch_compare(&av, &bv, method, |x, y| Ok(pred(compare_values(x, y)?)))?
                }
            }
        };
        self.op_push(StackVal::Bool(result));
        Ok(())
    }

    /// 紧凑 u8 热分派。Int 热路径就地改栈，轻量 Ret 不搬返回值。
    /// 必须 `inline(always)`：每指令一次调用的开销在空循环上远大于 I-cache 压力。
    #[inline(always)]
    fn dispatch_hot_u8(&mut self, ops: &[u8], hot_args: &[i64], pc: usize) -> HotFlow {
        use crate::hot_code::{
            H_ADD_LIST, H_ADD_NUM, H_ADD_TEXT, H_CALL, H_CALL_GLOBAL, H_CALL_SELF, H_DIV_NUM, H_EQ,
            H_GE, H_GOTO, H_GOTO_IF, H_GOTO_IF_NOT, H_GT, H_LABEL, H_LE, H_LOAD_FAST,
            H_LOAD_FAST_ADD_IMM_STORE, H_LOAD_FAST_ADD_STORE, H_LOAD_FAST_EQ_IMM,
            H_LOAD_FAST_GT_IMM, H_LOAD_FAST_LE_IMM, H_LOAD_FAST_LT_IMM, H_LOAD_FAST_MOD_EQ0,
            H_LOAD_FAST_SQR_GT, H_LOAD_FAST_SUB_IMM, H_LOAD_GLOBAL, H_LOOP_COUNTDOWN, H_LT,
            H_MOD_NUM, H_MUL_NUM, H_NE, H_PUSH_BOOL, H_PUSH_SMALL, H_RET, H_RET_FAST, H_RET_LEAVE,
            H_STORE_FAST, H_STORE_GLOBAL, H_SUB_NUM,
        };
        // SAFETY（已由外层 `'hot` 循环 `if pc >= code_len { break }` 保证）：
        // 进入本函数时 `pc < ops.len()`，且 `HotCode::encode` 保证 `ops.len() == hot_args.len()`。
        let op = unsafe { *ops.get_unchecked(pc) };
        let arg = unsafe { *hot_args.get_unchecked(pc) };
        match op {
            H_PUSH_SMALL => {
                self.pc = pc + 1;
                self.op_push_int(arg);
                HotFlow::Cont
            }
            H_PUSH_BOOL => {
                self.pc = pc + 1;
                self.op_push_bool(arg != 0);
                HotFlow::Cont
            }
            H_LOAD_FAST => {
                self.pc = pc + 1;
                // 轻量帧：绝大多数为 Int，避免 copy_imm 总匹配。
                if self.lw_depth != 0 {
                    let idx = self.lw_base + (arg as usize);
                    if idx < self.lw_sp {
                        // SAFETY: idx < lw_sp <= lw_slots.len()（lw_sp 仅在 resize 后推进）。
                        return unsafe {
                            match self.lw_slots.get_unchecked(idx) {
                                StackVal::Int(n) => {
                                    self.op_push_int(*n);
                                    HotFlow::Cont
                                }
                                other => {
                                    self.op_push(other.copy_imm());
                                    HotFlow::Cont
                                }
                            }
                        };
                    }
                    self.op_push(StackVal::Empty);
                } else {
                    self.op_push(self.load_fast_sv(arg as usize));
                }
                HotFlow::Cont
            }
            H_STORE_FAST => {
                self.pc = pc + 1;
                let slot = arg as usize;
                if let Err(e) = self.reject_const_fast_store(slot) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                let v = self.pop_hot();
                self.store_fast_sv(slot, v);
                HotFlow::Cont
            }
            H_LOAD_FAST_SUB_IMM => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                // 快路径：lw_depth > 0 且槽为 Int → 就地减法无溢出检查。
                if self.lw_depth != 0 {
                    let idx = self.lw_base + slot;
                    if idx < self.lw_sp {
                        // SAFETY: idx < lw_sp <= lw_slots.len()。
                        if let StackVal::Int(n) = unsafe { self.lw_slots.get_unchecked(idx) } {
                            self.pc = pc + 1;
                            let (r, ov) = n.overflowing_sub(imm);
                            if ov {
                                // 溢出 → 慢路径 BigInt 提升。
                                let v = Value::Num(Num::Small(*n));
                                self.op_push(StackVal::from_value(v));
                                self.op_push_int(imm);
                                let b = self.op_pop();
                                let a = self.op_pop();
                                if let Err(e) = self.exec_sub_slow(a, b) {
                                    self.set_hot_error(e);
                                    return HotFlow::Fail;
                                }
                                HotFlow::Cont
                            } else {
                                self.op_push_int(r);
                                HotFlow::Cont
                            }
                        } else {
                            // 非 Int（Heap）→ 冷路径（pc 未推进，step 会处理）。
                            HotFlow::Cold
                        }
                    } else {
                        HotFlow::Cold
                    }
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_LE_IMM => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                if let Some(n) = self.lw_slot_int(slot) {
                    self.pc = pc + 1;
                    self.op_push_bool(n <= imm);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_LT_IMM => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                if let Some(n) = self.lw_slot_int(slot) {
                    self.pc = pc + 1;
                    self.op_push_bool(n < imm);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_GT_IMM => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                if let Some(n) = self.lw_slot_int(slot) {
                    self.pc = pc + 1;
                    self.op_push_bool(n > imm);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_EQ_IMM => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                if let Some(n) = self.lw_slot_int(slot) {
                    self.pc = pc + 1;
                    self.op_push_bool(n == imm);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_ADD_IMM_STORE => {
                let (slot, imm) = crate::hot_code::decode_slot_imm(arg);
                if let Some(n) = self.lw_slot_int(slot) {
                    let (r, ov) = n.overflowing_add(imm);
                    if !ov {
                        if let Err(e) = self.reject_const_fast_store(slot) {
                            self.set_hot_error(e);
                            return HotFlow::Fail;
                        }
                        self.pc = pc + 1;
                        self.store_fast_sv(slot, StackVal::Int(r));
                        return HotFlow::Cont;
                    }
                }
                HotFlow::Cold
            }
            H_LOAD_FAST_ADD_STORE => {
                let (dst, src) = crate::hot_code::decode_two_slots(arg);
                if let (Some(a), Some(b)) = (self.lw_slot_int(dst), self.lw_slot_int(src)) {
                    let (r, ov) = a.overflowing_add(b);
                    if !ov {
                        if let Err(e) = self.reject_const_fast_store(dst) {
                            self.set_hot_error(e);
                            return HotFlow::Fail;
                        }
                        self.pc = pc + 1;
                        self.store_fast_sv(dst, StackVal::Int(r));
                        return HotFlow::Cont;
                    }
                }
                HotFlow::Cold
            }
            H_LOAD_FAST_SQR_GT => {
                let (dslot, nslot) = crate::hot_code::decode_two_slots(arg);
                if let (Some(d), Some(n)) = (self.lw_slot_int(dslot), self.lw_slot_int(nslot)) {
                    self.pc = pc + 1;
                    let gt = match d.checked_mul(d) {
                        Some(sq) => sq > n,
                        None => (d as i128).saturating_mul(d as i128) > i128::from(n),
                    };
                    self.op_push_bool(gt);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_LOAD_FAST_MOD_EQ0 => {
                let (lhs, rhs) = crate::hot_code::decode_two_slots(arg);
                if let (Some(a), Some(b)) = (self.lw_slot_int(lhs), self.lw_slot_int(rhs)) {
                    if b == 0 {
                        self.set_hot_error(RuntimeError::zero_div_diag());
                        return HotFlow::Fail;
                    }
                    self.pc = pc + 1;
                    self.op_push_bool(a % b == 0);
                    HotFlow::Cont
                } else {
                    HotFlow::Cold
                }
            }
            H_ADD_NUM => {
                self.pc = pc + 1;
                if self.binop_ints_inplace(|x, y| {
                    let (r, ov) = x.overflowing_add(y);
                    if ov {
                        None
                    } else {
                        Some(r)
                    }
                }) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                if let Err(e) = self.exec_add_slow(a, b) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_SUB_NUM => {
                self.pc = pc + 1;
                if self.binop_ints_inplace(|x, y| {
                    let (r, ov) = x.overflowing_sub(y);
                    if ov {
                        None
                    } else {
                        Some(r)
                    }
                }) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                if let Err(e) = self.exec_sub_slow(a, b) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_MUL_NUM => {
                self.pc = pc + 1;
                if self.binop_ints_inplace(|x, y| {
                    let (r, ov) = x.overflowing_mul(y);
                    if ov {
                        None
                    } else {
                        Some(r)
                    }
                }) {
                    return HotFlow::Cont;
                }
                if let Err(e) = self.exec_mul_num() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_DIV_NUM => {
                self.pc = pc + 1;
                if let Err(e) = self.exec_div_num() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_MOD_NUM => {
                self.pc = pc + 1;
                // Int 快路径：除零时返回 None → 走慢路径报错；MIN % -1 特殊处理避免溢出 panic。
                if self.binop_ints_inplace(|x, y| {
                    if y == 0 {
                        None
                    } else if x == i64::MIN && y == -1 {
                        Some(0)
                    } else {
                        // Python-style：余数符号跟随除数（与 rem_num 一致）。
                        let mut r = x % y;
                        if r != 0 && ((r < 0) != (y < 0)) {
                            r += y;
                        }
                        Some(r)
                    }
                }) {
                    return HotFlow::Cont;
                }
                if let Err(e) = self.exec_mod_num() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_LE => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x <= y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                let av = a.to_value();
                let bv = b.to_value();
                let result = match (&av, &bv) {
                    (Value::Num(_), Value::Num(_)) | (Value::Text(_), Value::Text(_)) => {
                        compare_values(&av, &bv).map(|c| c != std::cmp::Ordering::Greater)
                    }
                    _ => self.dispatch_compare(&av, &bv, "__le__", |x, y| {
                        Ok(compare_values(x, y)? != std::cmp::Ordering::Greater)
                    }),
                };
                match result {
                    Ok(r) => {
                        self.op_push_bool(r);
                        HotFlow::Cont
                    }
                    Err(e) => {
                        self.set_hot_error(e);
                        HotFlow::Fail
                    }
                }
            }
            H_LT => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x < y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                self.op_push(a);
                self.op_push(b);
                if let Err(e) = self.exec_cmp_num("__lt__", |c| c == std::cmp::Ordering::Less) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_GT => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x > y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                self.op_push(a);
                self.op_push(b);
                if let Err(e) = self.exec_cmp_num("__gt__", |c| c == std::cmp::Ordering::Greater) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_GE => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x >= y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                self.op_push(a);
                self.op_push(b);
                if let Err(e) = self.exec_cmp_num("__ge__", |c| c != std::cmp::Ordering::Less) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_EQ => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x == y) {
                    return HotFlow::Cont;
                }
                // 非 Int 就地比较：原实现 push 回再 exec_eq_num 会重复 pop，
                // 这里直接 pop 后走 dispatch_eq（保留 __eq__ 魔术语义）。
                let b = self.pop_hot();
                let a = self.pop_hot();
                let result = if let (StackVal::Int(x), StackVal::Int(y)) = (&a, &b) {
                    Ok(x == y)
                } else {
                    let av = a.to_value();
                    let bv = b.to_value();
                    self.dispatch_eq(&av, &bv)
                };
                match result {
                    Ok(r) => {
                        self.op_push(StackVal::Bool(r));
                        HotFlow::Cont
                    }
                    Err(e) => {
                        self.set_hot_error(e);
                        HotFlow::Fail
                    }
                }
            }
            H_NE => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x != y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                let result = if let (StackVal::Int(x), StackVal::Int(y)) = (&a, &b) {
                    Ok(x != y)
                } else {
                    let av = a.to_value();
                    let bv = b.to_value();
                    self.dispatch_eq(&av, &bv).map(|eq| !eq)
                };
                match result {
                    Ok(r) => {
                        self.op_push(StackVal::Bool(r));
                        HotFlow::Cont
                    }
                    Err(e) => {
                        self.set_hot_error(e);
                        HotFlow::Fail
                    }
                }
            }
            H_GOTO => {
                self.pc = arg as usize;
                self.tick_budget();
                HotFlow::Cont
            }
            H_LOOP_COUNTDOWN => {
                let sp = self.stack_sp;
                if sp > 0 {
                    // SAFETY: sp > 0 且 sp <= stack.len()，故 sp-1 合法。
                    if let StackVal::Int(n) = unsafe { self.stack.get_unchecked_mut(sp - 1) } {
                        if *n <= 0 {
                            self.stack_sp = sp - 1;
                            self.pc = arg as usize;
                        } else {
                            // wrapping：release 也曾开 overflow-checks；热循环避免检查开销。
                            *n = n.wrapping_sub(1);
                            self.pc = pc + 1;
                        }
                        return HotFlow::Cont;
                    }
                }
                // 非 Int（如 BigInt）走冷路径；勿推进 pc。
                HotFlow::Cold
            }
            H_GOTO_IF_NOT => {
                // 常见：比较结果 Bool 在 TOS
                self.pc = pc + 1;
                let sp = self.stack_sp;
                if sp > 0 {
                    // SAFETY: sp > 0 且 sp <= stack.len()，故 sp-1 合法。
                    match unsafe { self.stack.get_unchecked(sp - 1) } {
                        StackVal::Bool(b) => {
                            self.stack_sp = sp - 1;
                            if !*b {
                                self.pc = arg as usize;
                            }
                            return HotFlow::Cont;
                        }
                        StackVal::Int(n) => {
                            self.stack_sp = sp - 1;
                            if *n == 0 {
                                self.pc = arg as usize;
                            }
                            return HotFlow::Cont;
                        }
                        _ => {}
                    }
                }
                let cond = self.pop_hot();
                if !cond.is_truthy() {
                    self.pc = arg as usize;
                }
                HotFlow::Cont
            }
            H_GOTO_IF => {
                self.pc = pc + 1;
                let sp = self.stack_sp;
                if sp > 0 {
                    // SAFETY: sp > 0 且 sp <= stack.len()，故 sp-1 合法。
                    match unsafe { self.stack.get_unchecked(sp - 1) } {
                        StackVal::Bool(b) => {
                            self.stack_sp = sp - 1;
                            if *b {
                                self.pc = arg as usize;
                            }
                            return HotFlow::Cont;
                        }
                        StackVal::Int(n) => {
                            self.stack_sp = sp - 1;
                            if *n != 0 {
                                self.pc = arg as usize;
                            }
                            return HotFlow::Cont;
                        }
                        _ => {}
                    }
                }
                let cond = self.pop_hot();
                if cond.is_truthy() {
                    self.pc = arg as usize;
                }
                HotFlow::Cont
            }
            H_LABEL => {
                self.pc = pc + 1;
                HotFlow::Cont
            }
            H_CALL_SELF => {
                let argc = arg as usize;
                if self.lw_depth != 0 {
                    self.pc = pc + 1;
                    if argc == 1 {
                        self.call_self_lw1(self.lw_entry_pc);
                    } else {
                        self.call_self_lightweight(argc, self.lw_entry_pc, self.lw_frame_slots);
                    }
                } else {
                    let Some(func) = self.func_stack.last() else {
                        self.pc = pc + 1;
                        self.set_hot_error(RuntimeError::msg("CallSelf outside function"));
                        return HotFlow::Fail;
                    };
                    if !func.lightweight() || self.debug_active {
                        return HotFlow::Cold;
                    }
                    self.lw_entry_pc = func.entry_pc;
                    self.lw_frame_slots = func.frame_slots;
                    self.pc = pc + 1;
                    if argc == 1 {
                        self.call_self_lw1(func.entry_pc);
                    } else {
                        self.call_self_lightweight(argc, func.entry_pc, func.frame_slots);
                    }
                }
                HotFlow::Cont
            }
            H_RET | H_RET_LEAVE => {
                let leave = op == H_RET_LEAVE;
                self.pc = pc + 1;
                if let Some(ret_pc) = self.pop_fast_ret() {
                    // 返回值已在操作数栈顶，轻量返回只需拆帧，禁止 pop/push。
                    self.pop_lightweight_frame();
                    self.pc = ret_pc;
                    HotFlow::Cont
                } else {
                    let result_sv = if self.stack_sp == 0 {
                        StackVal::Empty
                    } else {
                        self.op_pop()
                    };
                    self.pending_ret = Some((leave, result_sv));
                    HotFlow::PendingRet
                }
            }
            H_RET_FAST => {
                self.pc = pc + 1;
                if let Some(ret_pc) = self.pop_fast_ret() {
                    // 从局部取返回值压栈，再拆帧（拆帧不碰操作数栈）。
                    let result_sv = self.load_fast_sv(arg as usize);
                    self.pop_lightweight_frame();
                    self.pc = ret_pc;
                    self.op_push(result_sv);
                    HotFlow::Cont
                } else {
                    let result_sv = self.load_fast_sv(arg as usize);
                    self.pending_ret = Some((false, result_sv));
                    HotFlow::PendingRet
                }
            }
            H_ADD_TEXT => {
                self.pc = pc + 1;
                if let Err(e) = self.exec_add_text_text() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_ADD_LIST => {
                self.pc = pc + 1;
                if let Err(e) = self.exec_add_list_list() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_LOAD_GLOBAL => {
                self.pc = pc + 1;
                let idx = arg as usize;
                // Fast: top-level Small Int from parallel slot.
                if self.user_call_frames.is_empty() && idx < self.script_globals.len() {
                    // SAFETY: idx < script_globals.len().
                    if let Value::Num(Num::Small(n)) =
                        unsafe { self.script_globals.get_unchecked(idx) }
                    {
                        self.op_push_int(*n);
                        return HotFlow::Cont;
                    }
                }
                self.dispatch_hot_load_global_slow(idx)
            }
            H_STORE_GLOBAL => {
                self.pc = pc + 1;
                let idx = arg as usize;
                let v = self.pop_hot();
                // Fast: top-level Int into parallel slot (M:1 dirty bit; no SharedMap lock).
                if self.user_call_frames.is_empty()
                    && !self.has_const_names
                    && !self.has_pending_const
                    && idx < self.script_globals.len()
                    && self
                        .script_global_names
                        .get(idx)
                        .is_some_and(|n| !n.is_empty())
                {
                    if let StackVal::Int(n) = v {
                        // SAFETY: idx < script_globals.len(); name non-empty checked above.
                        unsafe {
                            *self.script_globals.get_unchecked_mut(idx) = Value::Num(Num::Small(n));
                        }
                        if self.mn_parallel {
                            return self.dispatch_hot_store_global_mn(idx, n);
                        }
                        self.sync_local_fn_hot(idx, &Value::Num(Num::Small(n)));
                        self.script_globals_map_dirty = true;
                        return HotFlow::Cont;
                    }
                }
                if let Err(e) = self.exec_store_global(idx, v.into_value()) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_CALL => self.dispatch_hot_call(pc, arg as usize),
            H_CALL_GLOBAL => {
                let (global_idx, argc_i) = crate::hot_code::decode_slot_imm(arg);
                self.dispatch_hot_call_global(pc, global_idx, argc_i as usize)
            }
            _ => HotFlow::Cold,
        }
    }

    /// Call / CallGlobal 体较大；`inline(never)` 避免撑爆热分派 I-cache。
    #[inline(never)]
    fn dispatch_hot_call(&mut self, pc: usize, argc: usize) -> HotFlow {
        if self.debug_active {
            return HotFlow::Cold;
        }
        // 栈顶为 callee，其下为 argc 个参数（最后一参最靠近顶）。
        if self.stack_sp < argc + 1 {
            self.set_hot_error(RuntimeError::msg("stack underflow"));
            return HotFlow::Fail;
        }
        let callee_idx = self.stack_sp - 1;
        // SAFETY: callee_idx < stack_sp <= stack.len()。
        let is_lw = unsafe {
            match self.stack.get_unchecked(callee_idx) {
                StackVal::Func(f) => Self::func_is_hot_callable(f, argc),
                StackVal::Heap(v) => match v.as_ref() {
                    Value::Function(f) => Self::func_is_hot_callable(f, argc),
                    _ => false,
                },
                _ => false,
            }
        };
        if !is_lw {
            return HotFlow::Cold;
        }
        self.pc = pc + 1;
        let func = match self.pop_hot() {
            StackVal::Func(f) => f,
            StackVal::Heap(b) => {
                if let Value::Function(f) = *b {
                    f
                } else {
                    self.set_hot_error(RuntimeError::msg("internal: H_CALL lightweight mismatch"));
                    return HotFlow::Fail;
                }
            }
            _ => {
                self.set_hot_error(RuntimeError::msg("internal: H_CALL lightweight mismatch"));
                return HotFlow::Fail;
            }
        };
        if self.try_elide_ret_fast_call(&func, argc) {
            return HotFlow::Cont;
        }
        if let Err(e) = self.setup_lightweight_user_call_stack(func, argc) {
            self.set_hot_error(e);
            return HotFlow::Fail;
        }
        HotFlow::Switched
    }

    #[inline(never)]
    fn dispatch_hot_call_global(&mut self, pc: usize, global_idx: usize, argc: usize) -> HotFlow {
        if self.debug_active {
            return HotFlow::Cold;
        }
        if self.stack_sp < argc {
            self.set_hot_error(RuntimeError::msg("stack underflow"));
            return HotFlow::Fail;
        }
        if let Some(Some(f)) = self.local_fn_hot.get(global_idx) {
            if Self::func_is_hot_callable(f, argc) {
                let func = f.clone();
                self.pc = pc + 1;
                if self.try_elide_ret_fast_call(&func, argc) {
                    return HotFlow::Cont;
                }
                if let Err(e) = self.setup_lightweight_user_call_stack(func, argc) {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                return HotFlow::Switched;
            }
        }
        let func = match self.resolve_global_function_hot(global_idx) {
            Ok(Some(f)) if Self::func_is_hot_callable(&f, argc) => f,
            Ok(Some(_) | None) => return HotFlow::Cold,
            Err(e) => {
                self.set_hot_error(e);
                return HotFlow::Fail;
            }
        };
        self.pc = pc + 1;
        // `return x` / `RetFast(0)` 单指令体：就地整理参数栈，不切代码。
        if self.try_elide_ret_fast_call(&func, argc) {
            return HotFlow::Cont;
        }
        if let Err(e) = self.setup_lightweight_user_call_stack(func, argc) {
            self.set_hot_error(e);
            return HotFlow::Fail;
        }
        HotFlow::Switched
    }

    #[inline(never)]
    fn dispatch_hot_load_global_slow(&mut self, idx: usize) -> HotFlow {
        let aligned = self.user_call_frames.is_empty()
            || self
                .active_module_global_env()
                .is_none_or(|env| env.global_names.get(idx) == self.script_global_names.get(idx));
        if aligned && idx < self.script_globals.len() {
            // SAFETY: idx < len
            let sv = match unsafe { self.script_globals.get_unchecked(idx) } {
                Value::None => {
                    return match self.load_script_global(idx) {
                        Ok(v) => {
                            self.op_push(StackVal::from_value(v));
                            HotFlow::Cont
                        }
                        Err(e) => {
                            self.set_hot_error(e);
                            HotFlow::Fail
                        }
                    };
                }
                Value::Bool(b) => StackVal::Bool(*b),
                Value::Function(f) => StackVal::Func(f.clone()),
                Value::Cell(c) => StackVal::from_value(c.borrow().clone()),
                other => StackVal::from_value(other.clone()),
            };
            self.op_push(sv);
            return HotFlow::Cont;
        }
        match self.load_script_global(idx) {
            Ok(v) => {
                self.op_push(StackVal::from_value(v));
                HotFlow::Cont
            }
            Err(e) => {
                self.set_hot_error(e);
                HotFlow::Fail
            }
        }
    }

    #[inline(never)]
    fn dispatch_hot_store_global_mn(&mut self, idx: usize, n: i64) -> HotFlow {
        let num = Value::Num(Num::Small(n));
        let Some(name) = self.script_global_names.get(idx) else {
            self.set_hot_error(RuntimeError::msg(format!(
                "internal: StoreGlobal({idx}) out of range for script global table (len {})",
                self.script_global_names.len()
            )));
            return HotFlow::Fail;
        };
        if name.is_empty() {
            self.set_hot_error(RuntimeError::msg(format!(
                "internal: StoreGlobal({idx}) resolves to empty global name"
            )));
            return HotFlow::Fail;
        }
        let name = name.clone();
        if idx < self.script_globals.len() {
            self.script_globals[idx] = num.clone();
        }
        self.sync_local_fn_hot(idx, &num);
        if !self.globals.set_inplace(name.as_str(), num.clone()) {
            self.globals.insert(name, num);
        }
        HotFlow::Cont
    }

    #[inline(always)]
    fn func_is_hot_callable(f: &FunctionObject, argc: usize) -> bool {
        // 构造/闭包捕获时已缓存；热路径一次 u16 比较。
        (f.hot_call_argc as usize) == argc
    }

    /// 体为单条 `RetFast(slot)` 时：把栈上参数收成返回值，跳过调用帧。
    #[inline(always)]
    fn try_elide_ret_fast_call(&mut self, func: &FunctionObject, argc: usize) -> bool {
        use crate::hot_code::H_RET_FAST;
        let ops = func.hot.ops.as_ref();
        let imms = func.hot.args.as_ref();
        if ops.len() != 1 || imms.len() != 1 || ops[0] != H_RET_FAST {
            return false;
        }
        let slot = imms[0] as usize;
        if slot >= argc || self.stack_sp < argc {
            return false;
        }
        let base = self.stack_sp - argc;
        // SAFETY: base+slot < stack_sp <= stack.len()。
        let result = unsafe { self.stack.get_unchecked(base + slot).copy_imm() };
        for i in base..self.stack_sp {
            // SAFETY: i < stack_sp <= len。
            unsafe {
                *self.stack.get_unchecked_mut(i) = StackVal::Empty;
            }
        }
        self.stack_sp = base;
        self.op_push(result);
        true
    }

    /// 热路径解析全局函数：顶层走平行槽，避免 `SharedMap`。
    #[inline(always)]
    fn resolve_global_function_hot(&self, idx: usize) -> Result<Option<Arc<FunctionObject>>> {
        if self.user_call_frames.is_empty() && idx < self.script_globals.len() {
            // SAFETY: idx 已界检。
            return Ok(match unsafe { self.script_globals.get_unchecked(idx) } {
                Value::Function(f) => Some(f.clone()),
                Value::Cell(c) => match &*c.borrow() {
                    Value::Function(f) => Some(f.clone()),
                    _ => None,
                },
                _ => None,
            });
        }
        match self.load_script_global(idx)? {
            Value::Function(f) => Ok(Some(f)),
            _ => Ok(None),
        }
    }

    #[inline(always)]
    fn call_self_lw1(&mut self, entry_pc: usize) {
        if self.lw_depth >= self.cached_max_depth {
            self.set_hot_error(RuntimeError::recursion_err(
                "maximum recursion depth exceeded",
            ));
            return;
        }
        self.push_fast_ret(self.pc);
        let base = self.lw_sp;
        self.push_lw_base(base);
        self.lw_base = base;
        self.lw_depth += 1;
        if base >= self.lw_slots.len() {
            self.lw_slots.resize(base + 1, StackVal::Empty);
        }
        self.lw_slots[base] = self.pop_hot();
        self.lw_sp = base + 1;
        self.pc = entry_pc;
    }

    pub fn run(&mut self) -> Result<Value> {
        self.fail_if_host_cancelled()?;
        match self.run_interpreter(None)? {
            InterpResult::Value(Some(v)) => Ok(v),
            InterpResult::Value(None) => Ok(self.stack_top()),
            InterpResult::Suspended => Err(RuntimeError::msg(
                "internal error: main fiber suspended unexpectedly",
            )),
            InterpResult::DebugBreak => Err(RuntimeError::msg(
                "internal error: debug break without debugger session",
            )),
            InterpResult::Yielded(_) => Err(RuntimeError::msg(
                "internal error: generator yield outside iterator",
            )),
        }
    }

    /// 调试会话：跑到断点/步进停点，或程序结束。
    pub fn run_until_debug_break(&mut self) -> Result<Option<Value>> {
        self.resume_debug_paused_tasks();
        match self.run_interpreter(None)? {
            InterpResult::Value(Some(v)) => Ok(Some(v)),
            InterpResult::Value(None) => Ok(Some(self.stack_top())),
            InterpResult::DebugBreak => Ok(None),
            InterpResult::Suspended => Err(RuntimeError::msg(
                "internal error: main fiber suspended unexpectedly",
            )),
            InterpResult::Yielded(_) => Err(RuntimeError::msg(
                "internal error: generator yield outside iterator",
            )),
        }
    }

    fn resume_debug_paused_tasks(&mut self) {
        let paused = std::mem::take(&mut self.debug_paused_tasks);
        self.debug_break_requested = false;
        for task in paused {
            {
                let mut inner = task.borrow_mut();
                inner.debug_paused = false;
            }
            self.enqueue_task(task);
        }
    }

    /// 任务命中调试停点：挂起纤程且不入就绪队列，并通知主循环。
    fn park_task_for_debug(&mut self) {
        let Some(ctx) = self.task_ctx.take() else {
            self.debug_break_requested = true;
            return;
        };
        let task = ctx.task.clone();
        let key = Self::task_ptr_key(&task);
        let fiber = self.capture_fiber(&ctx);
        self.fiber_insert(key, fiber);
        {
            let mut inner = task.borrow_mut();
            inner.state = TaskState::Suspended;
            inner.debug_paused = true;
        }
        if !self
            .debug_paused_tasks
            .iter()
            .any(|t| Shared::ptr_eq(t, &task))
        {
            self.debug_paused_tasks.push(task);
        }
        self.debug_break_requested = true;
    }

    #[inline]
    fn check_debug_pause(&mut self) -> Option<InterpResult> {
        let dbg = self.debug.clone()?;
        let mut state = dbg.borrow_mut();
        if crate::debug::should_pause(self, &mut state) {
            crate::debug::mark_stopped(self, &mut state);
            return Some(InterpResult::DebugBreak);
        }
        None
    }

    fn run_interpreter(&mut self, until_depth: Option<usize>) -> Result<InterpResult> {
        self.ensure_op_stack(256);
        'outer: loop {
            // 仅在外层刷新切片；CallSelf/Cont 热路径不碰 Rc / ptr_eq。
            let hot_ops = Arc::clone(&self.hot_ops);
            let hot_args = Arc::clone(&self.hot_args);
            let ops = hot_ops.as_ref();
            let args = hot_args.as_ref();
            let code_len = ops.len();

            'hot: loop {
                // Skip debug branches when inactive (empty/nested loop microbench path).
                if self.debug_active {
                    if self.debug_break_requested && self.task_ctx.is_none() {
                        self.debug_break_requested = false;
                        return Ok(InterpResult::DebugBreak);
                    }
                    if let Some(r) = self.check_debug_pause() {
                        return Ok(r);
                    }
                }
                if self.cover_active {
                    crate::coverage::record_hit(self);
                }
                if self.pending_suspend || self.nested_user_call_suspended {
                    return self.take_task_suspend();
                }
                let pc = self.pc;
                if pc >= code_len {
                    break 'hot;
                }
                if let Some(remaining) = self.debug_eval_budget.as_mut() {
                    if *remaining == 0 {
                        return Err(RuntimeError::msg(
                            "debug evaluate instruction budget exceeded",
                        ));
                    }
                    *remaining -= 1;
                }
                match self.dispatch_hot_u8(ops, args, pc) {
                    // Cont first, no guard: empty/nested loops hit this every insn.
                    HotFlow::Cont => continue 'hot,
                    HotFlow::Fail => {
                        self.hot_failed = false;
                        let e = self
                            .hot_error
                            .take()
                            .unwrap_or_else(|| RuntimeError::msg("hot path error"));
                        if self.handle_or_promote_error(&e)? {
                            continue 'outer;
                        }
                        self.record_error_stack();
                        self.unwind_user_calls_on_error()?;
                        return self.finish_uncaught(e);
                    }
                    HotFlow::PendingRet => {
                        if self.user_call_frames.is_empty() {
                            self.flush_script_fast_locals();
                        }
                        if self.lw_depth > 0 && self.fast_ret_sp == 0 {
                            self.pop_lightweight_frame();
                        }
                        let (leave, result_sv) = self.pending_ret.take().expect(
                            "pending_ret set under HotFlow::PendingRet (theoretically unreachable)",
                        );
                        let result = result_sv.into_value();
                        if let Some(ret) = self.complete_user_return_instruction(leave, result)? {
                            return Ok(InterpResult::Value(Some(ret)));
                        }
                        if until_depth.is_some_and(|d| self.user_call_frames.len() == d) {
                            return Ok(InterpResult::Value(self.op_last_value()));
                        }
                        continue 'outer;
                    }
                    HotFlow::Switched => {
                        self.tick_budget();
                        if self.pending_suspend || self.nested_user_call_suspended {
                            return self.take_task_suspend();
                        }
                        if self.pending_main_yield {
                            self.pending_main_yield = false;
                            if let Err(e) = self.scheduler_yield() {
                                if self.handle_or_promote_error(&e)? {
                                    continue 'outer;
                                }
                                self.record_error_stack();
                                self.unwind_user_calls_on_error()?;
                                return self.finish_uncaught(e);
                            }
                        }
                        continue 'outer;
                    }
                    HotFlow::Cold => {
                        let ops_ptr = Arc::as_ptr(&self.hot_ops).cast::<u8>();
                        let args_ptr = Arc::as_ptr(&self.hot_args).cast::<i64>();
                        if let Err(e) = self.step() {
                            if self.handle_or_promote_error(&e)? {
                                continue 'outer;
                            }
                            self.record_error_stack();
                            self.unwind_user_calls_on_error()?;
                            return self.finish_uncaught(e);
                        }
                        self.tick_budget();
                        if self.pending_suspend || self.nested_user_call_suspended {
                            return self.take_task_suspend();
                        }
                        if self.pending_main_yield {
                            self.pending_main_yield = false;
                            if let Err(e) = self.scheduler_yield() {
                                if self.handle_or_promote_error(&e)? {
                                    continue 'outer;
                                }
                                self.record_error_stack();
                                self.unwind_user_calls_on_error()?;
                                return self.finish_uncaught(e);
                            }
                        }
                        if let Some(v) = self.pending_gen_yield.take() {
                            return Ok(InterpResult::Yielded(v));
                        }
                        if std::ptr::eq(Arc::as_ptr(&self.hot_ops).cast::<u8>(), ops_ptr)
                            && std::ptr::eq(Arc::as_ptr(&self.hot_args).cast::<i64>(), args_ptr)
                        {
                            continue 'hot;
                        }
                        continue 'outer;
                    }
                }
            }

            // pc 已越界（pc >= code_len）
            if until_depth.is_none() && self.user_call_frames.is_empty() {
                return Ok(InterpResult::Value(self.op_last_value()));
            }
            if until_depth.is_some_and(|d| self.user_call_frames.len() <= d) {
                return Ok(InterpResult::Value(self.op_last_value()));
            }
            if !self.user_call_frames.is_empty() {
                if let Some(ret) = self.complete_user_return_instruction(false, Value::None)? {
                    return Ok(InterpResult::Value(Some(ret)));
                }
                continue 'outer;
            }
            break;
        }
        Ok(InterpResult::Value(None))
    }

    /// 若已有脚本异常则分发；否则把**任意**宿主错误提升为语言异常再 `throw`。
    /// 返回 `true` 表示已跳到处理器，应继续执行。
    fn handle_or_promote_error(&mut self, e: &RuntimeError) -> Result<bool> {
        if self.active_exception.is_some() {
            return self.dispatch_to_handler();
        }
        let message = e.message();
        // 已格式化的诊断 / 回溯不再二次提升。
        if message.starts_with("error:") || message.starts_with('\n') {
            return Ok(false);
        }
        let exc = exceptions::make_exception_kind(self, e.kind(), message)?;
        match self.throw_value(exc) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// 未捕获脚本异常：保留 `kind` + 正文。人读 `TypeName: msg` 只在 traceback 末行拼一次。
    fn host_error_from_active(&self) -> RuntimeError {
        let Some(exc) = self.active_exception.as_ref() else {
            return RuntimeError::msg(String::new());
        };
        let kind = exceptions::kind_of_value(exc).unwrap_or(crate::error::ExceptionKind::Runtime);
        let msg = exceptions::exception_message(exc).unwrap_or_default();
        RuntimeError::typed(kind, msg)
    }

    fn finalize_runtime_error(&self, fallback: RuntimeError) -> RuntimeError {
        if self.active_exception.is_some() {
            self.host_error_from_active()
        } else {
            fallback
        }
    }

    fn dispatch_to_handler(&mut self) -> Result<bool> {
        // 任务纤程内不得跳到宿主 try（否则 leave_scope 会卸掉 stop_* 以下的帧）。
        let stop_try = self.task_ctx.as_ref().map_or(0, |c| c.stop_try);
        if self.try_stack.len() <= stop_try {
            return Ok(false);
        }
        let Some(frame) = self.try_stack.last().cloned() else {
            return Ok(false);
        };
        let stop_ucf = self.task_ctx.as_ref().map_or(0, |c| c.stop_ucf);
        let stop_fast_ret = self.task_ctx.as_ref().map_or(0, |c| c.stop_fast_ret);
        // 先展开轻量 CallSelf，再展开完整用户调用帧，恢复 try 所在代码对象。
        let fast_floor = frame.fast_ret_sp.max(stop_fast_ret);
        while self.fast_ret_sp > fast_floor {
            self.fast_ret_sp -= 1;
            self.pop_lightweight_frame();
        }
        let ucf_floor = frame.user_call_depth.max(stop_ucf);
        while self.user_call_frames.len() > ucf_floor {
            let Some(ucf) = self.user_call_frames.pop() else {
                break;
            };
            self.leave_scope();
            if ucf.func.track_frames() {
                self.func_frames.pop();
            }
            if ucf.pushed_func_stack {
                self.func_stack.pop();
            }
            self.restore_user_call_frame(ucf);
        }
        let stop_stack = self.task_ctx.as_ref().map_or(0, |c| c.stop_stack);
        let stop_iters = self.task_ctx.as_ref().map_or(0, |c| c.stop_iters);
        self.op_truncate(frame.stack_sp.max(stop_stack));
        self.iterators.truncate(frame.iterators_len.max(stop_iters));
        self.user_call_deferred = false;
        self.pending_ret = None;
        self.jump_to_pc(frame.catch_pc);
        Ok(true)
    }

    pub(crate) fn throw_value(&mut self, exc: Value) -> Result<()> {
        if !exceptions::is_exception(self, &exc) {
            return Err(RuntimeError::type_err("can only throw exception"));
        }
        let tb = traceback::capture_traceback(self);
        let exc = traceback::set_exception_traceback(&exc, tb);
        self.active_exception = Some(exc.clone());
        if let Some(dbg) = &self.debug {
            if dbg.borrow().exception_raised {
                let mut st = dbg.borrow_mut();
                st.request_break(crate::debug::StopReason::Breakpoint);
                crate::debug::mark_stopped(self, &mut st);
                self.debug_break_requested = true;
            }
        }
        if self.dispatch_to_handler()? {
            return Ok(());
        }
        Err(self.host_error_from_active())
    }

    fn decode_step_action(ins: &Instruction) -> StepAction {
        use Instruction as I;
        match ins {
            I::Push(v) => StepAction::Push(v.clone()),
            I::PushSmall(n) => StepAction::PushSmall(*n),
            I::Pop => StepAction::Pop,
            I::Add | I::AddNumNum | I::AddTextText | I::AddListList => StepAction::Add,
            I::Sub | I::SubNumNum => StepAction::Sub,
            I::Mul | I::MulNumNum => StepAction::Mul,
            I::Div | I::DivNumNum => StepAction::Div,
            I::Mod | I::ModNumNum => StepAction::Mod,
            I::Pow | I::PowNumNum => StepAction::Pow,
            I::BitAnd => StepAction::BitAnd,
            I::BitOr => StepAction::BitOr,
            I::BitXor => StepAction::BitXor,
            I::LShift => StepAction::LShift,
            I::RShift => StepAction::RShift,
            I::Neg => StepAction::Neg,
            I::Invert => StepAction::Invert,
            I::Not => StepAction::Not,
            I::TruthyNot => StepAction::TruthyNot,
            I::And => StepAction::And,
            I::Or => StepAction::Or,
            I::Eq | I::EqNumNum => StepAction::Eq,
            I::Ne | I::NeNumNum => StepAction::Ne,
            I::Lt | I::LtNumNum => StepAction::Lt,
            I::Le | I::LeNumNum => StepAction::Le,
            I::Gt | I::GtNumNum => StepAction::Gt,
            I::Ge | I::GeNumNum => StepAction::Ge,
            I::In => StepAction::In,
            I::Is => StepAction::Is,
            I::IsNot => StepAction::IsNot,
            I::Load(name) => StepAction::Load(name.clone()),
            I::LoadGlobal(idx) => StepAction::LoadGlobal(*idx),
            I::LoadMacro(name) => StepAction::LoadMacro(name.clone()),
            I::Store(name) => StepAction::Store(name.clone()),
            I::StoreGlobal(idx) => StepAction::StoreGlobal(*idx),
            I::NewVar { name, is_const } => StepAction::NewVar {
                name: name.clone(),
                is_const: *is_const,
            },
            I::NewVarOrLoad(name) => StepAction::NewVarOrLoad(name.clone()),
            I::LoadFast(slot) => StepAction::LoadFast(*slot),
            I::StoreFast(slot) => StepAction::StoreFast(*slot),
            I::LoadFastSubImm { slot, imm } => StepAction::LoadFastSubImm {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastLeImm { slot, imm } => StepAction::LoadFastLeImm {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastLtImm { slot, imm } => StepAction::LoadFastLtImm {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastGtImm { slot, imm } => StepAction::LoadFastGtImm {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastEqImm { slot, imm } => StepAction::LoadFastEqImm {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastAddImmStore { slot, imm } => StepAction::LoadFastAddImmStore {
                slot: *slot,
                imm: *imm,
            },
            I::LoadFastAddStore { dst, src } => StepAction::LoadFastAddStore {
                dst: *dst,
                src: *src,
            },
            I::LoadFastSqrGt { sqr_slot, rhs_slot } => StepAction::LoadFastSqrGt {
                sqr_slot: *sqr_slot,
                rhs_slot: *rhs_slot,
            },
            I::LoadFastModEq0 { lhs_slot, rhs_slot } => StepAction::LoadFastModEq0 {
                lhs_slot: *lhs_slot,
                rhs_slot: *rhs_slot,
            },
            I::BindFast {
                slot,
                name,
                is_const,
            } => StepAction::BindFast {
                slot: *slot,
                name: name.clone(),
                is_const: *is_const,
            },
            I::EnterScope => StepAction::EnterScope,
            I::LeaveScope => StepAction::LeaveScope,
            I::Label(_) => StepAction::Label,
            I::Goto(target) => StepAction::Goto(*target),
            I::GotoIf(target) => StepAction::GotoIf(*target),
            I::GotoIfNot(target) => StepAction::GotoIfNot(*target),
            I::LoopCountdown(target) => StepAction::LoopCountdown(*target),
            I::Call { argc } => StepAction::Call { argc: *argc },
            I::CallGlobal { global_idx, argc } => StepAction::CallGlobal {
                global_idx: *global_idx,
                argc: *argc,
            },
            I::CallSelf { argc } => StepAction::CallSelf { argc: *argc },
            I::CallList => StepAction::CallList,
            I::CallEx => StepAction::CallEx,
            I::MacroCall { argc } => StepAction::MacroCall { argc: *argc },
            I::ListAppend => StepAction::ListAppend,
            I::ListExtend => StepAction::ListExtend,
            I::DictSet => StepAction::DictSet,
            I::SetAdd => StepAction::SetAdd,
            I::Ret => StepAction::Ret,
            I::RetFast(slot) => StepAction::RetFast(*slot),
            I::RetLeave => StepAction::RetLeave,
            I::VecNew(n) => StepAction::VecNew(*n),
            I::DictNew(n) => StepAction::DictNew(*n),
            I::SetNew(n) => StepAction::SetNew(*n),
            I::TupleNew(n) => StepAction::TupleNew(*n),
            I::Index => StepAction::Index,
            I::IndexSet => StepAction::IndexSet,
            I::SliceGet => StepAction::SliceGet,
            I::SliceSet => StepAction::SliceSet,
            I::DelIndex => StepAction::DelIndex,
            I::DelName(name) => StepAction::DelName(name.clone()),
            I::DelAttr(field) => StepAction::DelAttr(field.clone()),
            I::GetAttr(field) => StepAction::GetAttr(field.clone()),
            I::StructNew { name, argc } => StepAction::StructNew {
                name: name.clone(),
                argc: *argc,
            },
            I::VariantNew { name } => StepAction::VariantNew { name: name.clone() },
            I::SetField(field) => StepAction::SetField(field.clone()),
            I::IterNew => StepAction::IterNew,
            I::IterNext => StepAction::IterNext,
            I::IterEnd => StepAction::IterEnd,
            I::Throw => StepAction::Throw,
            I::Snap => StepAction::Snap,
            I::PushExc => StepAction::PushExc,
            I::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => StepAction::EnterTry {
                catch_label: *catch_label,
                else_label: *else_label,
                end_label: *end_label,
            },
            I::EndTry => StepAction::EndTry,
            I::PopTry => StepAction::PopTry,
            I::ExcMatch(type_name) => StepAction::ExcMatch(type_name.clone()),
            I::IsList => StepAction::IsList,
            I::ListLen => StepAction::ListLen,
            I::IsInstance(type_name) => StepAction::IsInstance(type_name.clone()),
            I::MatchEq => StepAction::MatchEq,
            I::UnpackExact(n) => StepAction::UnpackExact(*n),
            I::UnpackRest { before, after } => StepAction::UnpackRest {
                before: *before,
                after: *after,
            },
            I::Rethrow => StepAction::Rethrow,
            I::TypeCheck => StepAction::TypeCheck,
            I::ResolveFuncTypes => StepAction::ResolveFuncTypes,
            I::FindMod(parts) => StepAction::FindMod(parts.clone()),
            I::FindModFile(path) => StepAction::FindModFile(path.clone()),
            I::RegisterExport(name) => StepAction::RegisterExport(name.clone()),
            I::GoCall(argc) => StepAction::GoCall { argc: *argc },
            I::GoValue => StepAction::GoValue,
            I::Await => StepAction::Await,
            I::Suspend => StepAction::Suspend,
            I::Yield => StepAction::Yield,
            I::YieldFrom => StepAction::YieldFrom,
            I::SelectTryRecv => StepAction::SelectTryRecv,
            I::SelectTrySend => StepAction::SelectTrySend,
            I::SelectPollTask => StepAction::SelectPollTask,
            I::MakeDeadline => StepAction::MakeDeadline,
            I::SelectPollDeadline => StepAction::SelectPollDeadline,
            I::SelectIdle(n) => StepAction::SelectIdle(*n),
            I::SelectBegin(n) => StepAction::SelectBegin(*n),
            I::SelectNextIndex => StepAction::SelectNextIndex,
        }
    }

    fn step(&mut self) -> Result<()> {
        let pc = self.pc;
        if pc >= self.code.len() {
            return Err(RuntimeError::msg(format!(
                "internal: pc {pc} out of range (code len {})",
                self.code.len()
            )));
        }
        self.pc += 1;
        let action = Self::decode_step_action(&self.code[pc]);
        self.run_step_action(action)
    }

    fn run_step_action(&mut self, action: StepAction) -> Result<()> {
        match action {
            StepAction::Push(v) => self.push_value(v),
            StepAction::PushSmall(n) => self.push_int(n),
            StepAction::Pop => {
                if self.stack_sp > 0 {
                    self.op_pop();
                }
            }
            StepAction::Add => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match (&a, &b) {
                    (Value::Num(Num::Small(x)), Value::Num(Num::Small(y))) => {
                        Value::Num(x.checked_add(*y).map_or_else(
                            || {
                                Num::from_bigint(
                                    num_bigint::BigInt::from(*x) + num_bigint::BigInt::from(*y),
                                )
                            },
                            Num::Small,
                        ))
                    }
                    (Value::Num(_), Value::Num(_)) => a.add(&b)?,
                    _ => {
                        self.dispatch_binary_arith(&a, &b, "__add__", "__radd__", |x, y| x.add(y))?
                    }
                };
                self.push_value(result);
            }
            StepAction::Sub => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match (&a, &b) {
                    (Value::Num(Num::Small(x)), Value::Num(Num::Small(y))) => {
                        Value::Num(x.checked_sub(*y).map_or_else(
                            || {
                                Num::from_bigint(
                                    num_bigint::BigInt::from(*x) - num_bigint::BigInt::from(*y),
                                )
                            },
                            Num::Small,
                        ))
                    }
                    (Value::Num(_), Value::Num(_)) => a.sub(&b)?,
                    _ => {
                        self.dispatch_binary_arith(&a, &b, "__sub__", "__rsub__", |x, y| x.sub(y))?
                    }
                };
                self.push_value(result);
            }
            StepAction::Mul => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.mul(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__mul__", "__rmul__", |x, y| x.mul(y))?
                };
                self.push_value(result);
            }
            StepAction::Div => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.div(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__div__", "__rdiv__", |x, y| x.div(y))?
                };
                self.push_value(result);
            }
            StepAction::Pow => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.pow(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__pow__", "__rpow__", |x, y| x.pow(y))?
                };
                self.push_value(result);
            }
            StepAction::Mod => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.rem(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__mod__", "__rmod__", |x, y| x.rem(y))?
                };
                self.push_value(result);
            }
            StepAction::BitAnd => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.bitand(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__and__", "__rand__", |x, y| x.bitand(y))?
                };
                self.push_value(result);
            }
            StepAction::BitOr => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.bitor(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__or__", "__ror__", |x, y| x.bitor(y))?
                };
                self.push_value(result);
            }
            StepAction::BitXor => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.bitxor(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__xor__", "__rxor__", |x, y| x.bitxor(y))?
                };
                self.push_value(result);
            }
            StepAction::LShift => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.lshift(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__lshift__", "__rlshift__", |x, y| {
                        x.lshift(y)
                    })?
                };
                self.push_value(result);
            }
            StepAction::RShift => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.rshift(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__rshift__", "__rrshift__", |x, y| {
                        x.rshift(y)
                    })?
                };
                self.push_value(result);
            }
            StepAction::Neg => {
                let a = self.pop()?;
                let result = self.dispatch_neg(&a)?;
                self.push_value(result);
            }
            StepAction::Invert => {
                let a = self.pop()?;
                let result = self.dispatch_invert(&a)?;
                self.push_value(result);
            }
            StepAction::Not => {
                let a = self.pop()?;
                match a {
                    Value::Bool(b) => self.push_bool(!b),
                    _ => return Err(RuntimeError::type_err("! requires bool")),
                }
            }
            StepAction::TruthyNot => {
                let a = self.pop()?;
                let t = self.value_is_truthy(&a);
                self.push_bool(!t);
            }
            StepAction::And | StepAction::Or => {
                return Err(RuntimeError::type_err(
                    "internal: And/Or opcodes must be lowered to jumps by codegen",
                ));
            }
            StepAction::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = self.dispatch_eq(&a, &b)?;
                self.push_bool(result);
            }
            StepAction::Ne => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = self.dispatch_ne(&a, &b)?;
                self.push_bool(result);
            }
            StepAction::Lt => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match compare_values(&a, &b) {
                    Ok(ord) => ord == std::cmp::Ordering::Less,
                    Err(_) => self.dispatch_compare(&a, &b, "__lt__", |x, y| {
                        Ok(compare_values(x, y)? == std::cmp::Ordering::Less)
                    })?,
                };
                self.push_bool(result);
            }
            StepAction::Le => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match compare_values(&a, &b) {
                    Ok(ord) => ord != std::cmp::Ordering::Greater,
                    Err(_) => self.dispatch_compare(&a, &b, "__le__", |x, y| {
                        Ok(compare_values(x, y)? != std::cmp::Ordering::Greater)
                    })?,
                };
                self.push_bool(result);
            }
            StepAction::Gt => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match compare_values(&a, &b) {
                    Ok(ord) => ord == std::cmp::Ordering::Greater,
                    Err(_) => self.dispatch_compare(&a, &b, "__gt__", |x, y| {
                        Ok(compare_values(x, y)? == std::cmp::Ordering::Greater)
                    })?,
                };
                self.push_bool(result);
            }
            StepAction::Ge => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match compare_values(&a, &b) {
                    Ok(ord) => ord != std::cmp::Ordering::Less,
                    Err(_) => self.dispatch_compare(&a, &b, "__ge__", |x, y| {
                        Ok(compare_values(x, y)? != std::cmp::Ordering::Less)
                    })?,
                };
                self.push_bool(result);
            }
            StepAction::In => {
                let container = self.pop()?;
                let item = self.pop()?;
                let is_member = self.value_contains(&container, &item)?;
                self.push_bool(is_member);
            }
            StepAction::Is => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push_bool(values_identical(&a, &b));
            }
            StepAction::IsNot => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push_bool(!values_identical(&a, &b));
            }
            StepAction::ListAppend => {
                let val = self.pop()?;
                let list = self.pop()?;
                match list {
                    Value::List(l) => {
                        self.check_list_element_write(&l, &val)?;
                        l.borrow_mut().push(val);
                        self.push_value(Value::List(l));
                    }
                    _ => return Err(RuntimeError::type_err("ListAppend requires list")),
                }
            }
            StepAction::ListExtend => {
                let rhs = self.pop()?;
                let lhs = self.pop()?;
                match (lhs, rhs) {
                    (Value::List(l), Value::List(r)) => {
                        let items: Vec<_> = r.borrow().iter().cloned().collect();
                        for item in &items {
                            self.check_list_element_write(&l, item)?;
                        }
                        l.borrow_mut().extend(items);
                        self.push_value(Value::List(l));
                    }
                    _ => return Err(RuntimeError::type_err("ListExtend requires lists")),
                }
            }
            StepAction::DictSet => {
                let val = self.pop()?;
                let key = self.pop()?;
                let dict = self.pop()?;
                match dict {
                    Value::Dict(d) => {
                        self.check_dict_write(&d, &key, &val)?;
                        let vk = ValueKey::from_value(&key)?;
                        d.borrow_mut().insert(vk, val);
                        self.push_value(Value::Dict(d));
                    }
                    _ => return Err(RuntimeError::type_err("DictSet requires dict")),
                }
            }
            StepAction::SetAdd => {
                let val = self.pop()?;
                let set = self.pop()?;
                match set {
                    Value::Set(s) => {
                        self.check_set_element_write(&s, &val)?;
                        let vk = ValueKey::from_value(&val)?;
                        s.borrow_mut().insert(vk);
                        self.push_value(Value::Set(s));
                    }
                    _ => return Err(RuntimeError::type_err("SetAdd requires set")),
                }
            }
            StepAction::Load(name) => {
                self.push_value(self.load_name(&name)?);
            }
            StepAction::LoadGlobal(idx) => {
                self.push_value(self.load_script_global(idx)?);
            }
            StepAction::LoadMacro(name) => {
                self.push_value(self.load_macro(&name)?);
            }
            StepAction::Store(name) => {
                let val = self.pop()?;
                self.store_name(&name, val)?;
            }
            StepAction::StoreGlobal(idx) => {
                let val = self.pop()?;
                self.exec_store_global(idx, val)?;
            }
            StepAction::NewVar { name, is_const } => {
                if is_const {
                    self.has_pending_const = true;
                    self.pending_const.insert(name.clone());
                }
                if self.locals_stack.is_empty() {
                    if !self.globals.contains_key(name.as_str()) {
                        self.globals.insert(name, Value::None);
                    }
                } else {
                    let frame = self.locals_stack.len() - 1;
                    let names = self.scope_name_map_mut(frame);
                    if !names.contains_key(name.as_str()) {
                        let slot = names.len();
                        names.insert(name, slot);
                        if self.locals_stack[frame].len() <= slot {
                            self.locals_stack[frame].resize(slot + 1, Value::None);
                        }
                    }
                }
            }
            StepAction::NewVarOrLoad(name) => {
                if !self.globals.contains_key(name.as_str()) {
                    self.globals.insert(name.clone(), Value::None);
                }
                self.push_value(self.load_name(&name)?);
            }
            StepAction::EnterScope => {
                self.locals_stack.push(Vec::new());
                self.name_to_slot.push(None);
            }
            StepAction::LeaveScope => {
                if let Some(frame) = self.locals_stack.pop() {
                    self.recycle_local_frame(frame);
                }
                self.name_to_slot.pop();
            }
            StepAction::BindFast {
                slot,
                name,
                is_const,
            } => {
                let val = self
                    .pop()
                    .map_err(|_| RuntimeError::msg("internal: BindFast with empty stack"))?;
                if self.locals_stack.is_empty() {
                    return Err(RuntimeError::type_err(
                        "internal: BindFast requires an active local frame",
                    ));
                }
                let frame = self.locals_stack.len() - 1;
                self.local_set(slot, val);
                self.scope_name_map_mut(frame).insert(name.clone(), slot);
                if is_const {
                    self.const_names.insert(name);
                    self.has_const_names = true;
                }
            }
            StepAction::LoadFast(slot) => {
                self.op_push(self.load_fast_sv(slot));
            }
            StepAction::StoreFast(slot) => {
                self.reject_const_fast_store(slot)?;
                let val = self.pop()?;
                let cell = self.locals_stack.last().and_then(|frame| {
                    frame.get(slot).and_then(|v| match v {
                        Value::Cell(c) => Some(c.clone()),
                        _ => None,
                    })
                });
                if let Some(cell) = cell {
                    *cell.borrow_mut() = val;
                } else {
                    self.local_set(slot, val);
                }
            }
            StepAction::LoadFastSubImm { slot, imm } => {
                let sv = self.load_fast_sv(slot);
                self.op_push(sv);
                self.op_push_int(imm);
                let b = self.op_pop();
                let a = self.op_pop();
                self.exec_sub_slow(a, b)?;
            }
            StepAction::LoadFastLeImm { slot, imm } => {
                self.push_fast_cmp_imm(slot, imm, |c| {
                    c == std::cmp::Ordering::Less || c == std::cmp::Ordering::Equal
                })?;
            }
            StepAction::LoadFastLtImm { slot, imm } => {
                self.push_fast_cmp_imm(slot, imm, |c| c == std::cmp::Ordering::Less)?;
            }
            StepAction::LoadFastGtImm { slot, imm } => {
                self.push_fast_cmp_imm(slot, imm, |c| c == std::cmp::Ordering::Greater)?;
            }
            StepAction::LoadFastEqImm { slot, imm } => {
                self.push_fast_cmp_imm(slot, imm, |c| c == std::cmp::Ordering::Equal)?;
            }
            StepAction::LoadFastAddImmStore { slot, imm } => {
                self.reject_const_fast_store(slot)?;
                let sv = self.load_fast_sv(slot);
                self.op_push(sv);
                self.op_push_int(imm);
                let b = self.op_pop();
                let a = self.op_pop();
                self.exec_add_slow(a, b)?;
                let v = self.pop_hot();
                self.store_fast_sv(slot, v);
            }
            StepAction::LoadFastAddStore { dst, src } => {
                self.reject_const_fast_store(dst)?;
                let a = self.load_fast_sv(dst);
                let b = self.load_fast_sv(src);
                self.op_push(a);
                self.op_push(b);
                let rhs = self.op_pop();
                let lhs = self.op_pop();
                self.exec_add_slow(lhs, rhs)?;
                let v = self.pop_hot();
                self.store_fast_sv(dst, v);
            }
            StepAction::LoadFastSqrGt { sqr_slot, rhs_slot } => {
                let d = self.load_fast_sv(sqr_slot);
                self.op_push(d.copy_imm());
                self.op_push(d);
                self.exec_mul_num()?;
                let n = self.load_fast_sv(rhs_slot);
                self.op_push(n);
                self.exec_cmp_num("__gt__", |c| c == std::cmp::Ordering::Greater)?;
            }
            StepAction::LoadFastModEq0 { lhs_slot, rhs_slot } => {
                let a = self.load_fast_sv(lhs_slot);
                let b = self.load_fast_sv(rhs_slot);
                self.op_push(a);
                self.op_push(b);
                self.exec_mod_num()?;
                self.op_push_int(0);
                let z = self.pop_hot();
                let r = self.pop_hot();
                let eq = match (&r, &z) {
                    (StackVal::Int(x), StackVal::Int(y)) => x == y,
                    _ => {
                        let av = r.to_value();
                        let bv = z.to_value();
                        self.dispatch_eq(&av, &bv)?
                    }
                };
                self.op_push_bool(eq);
            }
            StepAction::Label => {}
            StepAction::Goto(target) => {
                self.jump_to_pc(target);
            }
            StepAction::GotoIf(target) => {
                let cond = self.pop()?;
                if self.value_is_truthy_fast(&cond) {
                    self.jump_to_pc(target);
                }
            }
            StepAction::GotoIfNot(target) => {
                let cond = self.pop()?;
                if !self.value_is_truthy_fast(&cond) {
                    self.jump_to_pc(target);
                }
            }
            StepAction::LoopCountdown(target) => {
                let counter = self.pop()?;
                match counter {
                    Value::Num(n) => {
                        if n.cmp_num(&Num::Small(0)) == std::cmp::Ordering::Greater {
                            // 复用减法慢路径以正确处理 BigInt / 溢出提升。
                            self.op_push(StackVal::from_value(Value::Num(n)));
                            self.op_push_int(1);
                            let b = self.pop_hot();
                            let a = self.pop_hot();
                            self.exec_sub_slow(a, b)?;
                        } else {
                            self.jump_to_pc(target);
                        }
                    }
                    other => {
                        return Err(RuntimeError::type_err(format!(
                            "loop count must be a number, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            StepAction::Call { argc } => {
                let callee = self.pop_sv()?.into_value();
                self.call_args_buf.clear();
                for _ in 0..argc {
                    let arg = self.pop()?;
                    self.call_args_buf.push(arg);
                }
                self.call_args_buf.reverse();
                let args = std::mem::take(&mut self.call_args_buf);
                let result = self.call_value(callee, args)?;
                self.finish_value_call(result)?;
            }
            StepAction::CallGlobal { global_idx, argc } => {
                let callee = self.load_script_global(global_idx)?;
                self.call_args_buf.clear();
                for _ in 0..argc {
                    let arg = self.pop()?;
                    self.call_args_buf.push(arg);
                }
                self.call_args_buf.reverse();
                let args = std::mem::take(&mut self.call_args_buf);
                let result = self.call_value(callee, args)?;
                self.finish_value_call(result)?;
            }
            StepAction::CallSelf { argc } => {
                let func = self
                    .func_stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::msg("CallSelf outside function"))?;
                if func.lightweight() && !self.debug_active {
                    self.call_self_lightweight(argc, func.entry_pc, func.frame_slots);
                } else {
                    self.call_args_buf.clear();
                    for _ in 0..argc {
                        let arg = self.pop()?;
                        self.call_args_buf.push(arg);
                    }
                    self.call_args_buf.reverse();
                    let args = std::mem::take(&mut self.call_args_buf);
                    let bound = self.bind_call_arguments(&func, args, DictMap::new())?;
                    self.setup_user_call(func, bound, true)?;
                    self.user_call_deferred = true;
                }
            }
            StepAction::CallList => {
                let callee = self.pop()?;
                let arglist = self.pop()?;
                let args = match arglist {
                    Value::List(l) => l.borrow().clone(),
                    _ => return Err(RuntimeError::type_err("CallList requires arg list")),
                };
                let result = self.call_value(callee, args)?;
                self.finish_value_call(result)?;
            }
            StepAction::CallEx => {
                let callee = self.pop()?;
                let kwargs_v = self.pop()?;
                let arglist = self.pop()?;
                let positional = match arglist {
                    Value::List(l) => l.borrow().clone(),
                    _ => return Err(RuntimeError::type_err("CallEx requires arg list")),
                };
                let kwargs = match kwargs_v {
                    Value::Dict(d) => d.borrow().clone(),
                    _ => return Err(RuntimeError::type_err("CallEx requires kwargs dict")),
                };
                let result = self.call_value_ex(callee, positional, kwargs)?;
                self.finish_value_call(result)?;
            }
            StepAction::MacroCall { argc } => {
                let callee = self.pop()?;
                let mut args = Vec::new();
                for _ in 0..argc {
                    args.push(self.pop()?);
                }
                args.reverse();
                let result = match callee {
                    Value::Macro(m) => self.call_macro(m, args)?,
                    Value::Function(_) => {
                        return Err(RuntimeError::msg(
                            "function cannot be invoked with {}; use () syntax",
                        ));
                    }
                    other => {
                        return Err(RuntimeError::type_err(format!(
                            "macro call requires macro value, got {}",
                            other.type_name()
                        )));
                    }
                };
                if self.active_exception.is_none() {
                    self.push_value(result);
                }
            }
            StepAction::Ret => {
                if self.stack_sp == 0 {
                    self.push_none();
                }
                self.pc = self.code.len();
            }
            StepAction::RetFast(slot) => {
                let result_sv = self.load_fast_sv(slot);
                self.op_push(result_sv);
                self.pc = self.code.len();
            }
            StepAction::RetLeave => {
                if self.stack_sp == 0 {
                    self.push_none();
                }
                self.leave_scope();
                self.pc = self.code.len();
            }
            StepAction::VecNew(n) => {
                let mut elems = Vec::new();
                for _ in 0..n {
                    elems.push(self.pop()?);
                }
                elems.reverse();
                let val = Value::List(Shared::new(elems));
                self.track_value(&val);
                self.push_value(val);
            }
            StepAction::DictNew(n) => {
                let mut map = crate::value::DictMap::new();
                for _ in 0..n {
                    let v = self.pop()?;
                    let k = self.pop()?;
                    map.insert(ValueKey::from_value(&k)?, v);
                }
                let val = Value::Dict(Shared::new(map));
                self.track_value(&val);
                self.push_value(val);
            }
            StepAction::SetNew(n) => {
                let mut elems = Vec::with_capacity(n);
                for _ in 0..n {
                    elems.push(self.pop()?);
                }
                elems.reverse();
                let mut set = crate::value::SetMap::new();
                for v in elems {
                    set.insert(ValueKey::from_value(&v)?);
                }
                let val = Value::Set(Shared::new(set));
                self.track_value(&val);
                self.push_value(val);
            }
            StepAction::TupleNew(n) => {
                let mut elems = Vec::with_capacity(n);
                for _ in 0..n {
                    elems.push(self.pop()?);
                }
                elems.reverse();
                self.push_value(Value::Tuple(elems.into()));
            }
            StepAction::Index => {
                let idx = self.pop()?;
                let obj = self.pop()?;
                let result = index_value(self, &obj, &idx)?;
                self.push_value(result);
            }
            StepAction::IndexSet => {
                let val = self.pop()?;
                let idx = self.pop()?;
                let obj = self.pop()?;
                // 与 Store 一致：赋值语句不向操作数栈压返回值，否则会污染外层已压栈实参。
                index_set(self, &obj, &idx, val)?;
            }
            StepAction::SliceGet => {
                let step = self.pop()?;
                let end = self.pop()?;
                let start = self.pop()?;
                let obj = self.pop()?;
                let result = slice_get(self, &obj, &start, &end, &step)?;
                self.push_value(result);
            }
            StepAction::SliceSet => {
                let val = self.pop()?;
                let step = self.pop()?;
                let end = self.pop()?;
                let start = self.pop()?;
                let obj = self.pop()?;
                slice_set(self, &obj, &start, &end, &step, val)?;
            }
            StepAction::DelIndex => {
                let idx = self.pop()?;
                let obj = self.pop()?;
                del_index(self, &obj, &idx)?;
            }
            StepAction::DelName(name) => {
                self.delete_name(&name)?;
            }
            StepAction::DelAttr(field) => {
                let obj = self.pop()?;
                del_attr(self, &obj, &field)?;
            }
            StepAction::GetAttr(field) => {
                let obj = self.pop()?;
                let val = get_attr(self, &obj, &field)?;
                self.push_value(val);
            }
            StepAction::StructNew { name, argc } => {
                let mut args = Vec::new();
                for _ in 0..argc {
                    args.push(self.pop()?);
                }
                args.reverse();
                let val = make_struct(self, &name, args, None)?;
                self.push_value(val);
            }
            StepAction::VariantNew { name } => {
                let payload = self.pop()?;
                let val = wrap_variant_payload(self, &name, None, payload)?;
                self.push_value(val);
            }
            StepAction::SetField(field) => {
                let val = self.pop()?;
                let obj = self.pop()?;
                set_field(self, &obj, &field, val)?;
            }
            StepAction::IterNew => {
                let obj = self.pop()?;
                let rc = self.to_iterator_shared(&obj)?;
                self.gc.track_iter(&rc);
                self.iterators.push(ActiveIter { state: rc });
            }
            StepAction::IterNext => {
                let state = self
                    .iterators
                    .last()
                    .ok_or_else(|| RuntimeError::msg("no iterator"))?
                    .state
                    .clone();
                match self.advance_iterator(&state) {
                    Ok(Some(val)) => {
                        // Channel.recv 等在任务内阻塞：挂起并重试 IterNext，勿把哨兵 none 当元素。
                        if self.block_suspend {
                            self.block_suspend = false;
                            if self.pc > 0 {
                                self.pc -= 1;
                            }
                            self.pending_suspend = true;
                            return Ok(());
                        }
                        self.push_value(val);
                        self.push_bool(true);
                    }
                    Ok(None) => {
                        if self.block_suspend {
                            self.block_suspend = false;
                            if self.pc > 0 {
                                self.pc -= 1;
                            }
                            self.pending_suspend = true;
                            return Ok(());
                        }
                        self.push_bool(false);
                    }
                    Err(e) => {
                        if e.kind() == crate::error::ExceptionKind::StopIteration {
                            self.push_bool(false);
                            return Ok(());
                        }
                        return Err(e);
                    }
                }
            }
            StepAction::IterEnd => {
                self.iterators.pop();
            }
            StepAction::Throw => {
                let exc = self.pop()?;
                return self.throw_value(exc);
            }
            StepAction::Snap => {
                let v = self.pop()?;
                if matches!(v, Value::None) {
                    return Err(RuntimeError::value_err("snap of none"));
                }
                self.push_value(v);
            }
            StepAction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                self.try_stack.push(TryFrame {
                    catch_pc: catch_label,
                    else_pc: else_label,
                    end_pc: end_label,
                    user_call_depth: self.user_call_frames.len(),
                    stack_sp: self.stack_sp,
                    iterators_len: self.iterators.len(),
                    fast_ret_sp: self.fast_ret_sp,
                });
            }
            StepAction::EndTry => {
                // 成功离开 try：先弹出帧，再跳到 else / end。
                // 这样 else 与成功清理不再被同一 handler 覆盖。
                let frame = self
                    .try_stack
                    .pop()
                    .ok_or_else(|| RuntimeError::msg("END_TRY without ENTER_TRY"))?;
                let target = if frame.else_pc != 0 {
                    frame.else_pc
                } else {
                    frame.end_pc
                };
                self.jump_to_pc(target);
            }
            StepAction::PopTry => {
                self.try_stack.pop();
                self.active_exception = None;
            }
            StepAction::PushExc => {
                let exc = self
                    .active_exception
                    .clone()
                    .ok_or_else(|| RuntimeError::msg("no active exception"))?;
                self.push_value(exc);
            }
            StepAction::ExcMatch(type_name) => {
                let matched = self
                    .active_exception
                    .as_ref()
                    .is_some_and(|e| exceptions::struct_is_a(self, e, &type_name));
                self.push_bool(matched);
            }
            StepAction::IsList => {
                let v = self.pop()?;
                self.push_bool(matches!(v, Value::List(_) | Value::Tuple(_)));
            }
            StepAction::ListLen => {
                let v = self.pop()?;
                let n = match &v {
                    Value::List(lst) => lst.borrow().len(),
                    Value::Tuple(t) => t.len(),
                    _ => return Err(RuntimeError::type_err("ListLen requires list or tuple")),
                };
                self.push_int(n as i64);
            }
            StepAction::IsInstance(type_name) => {
                let v = self.pop()?;
                self.push_bool(types::instance_is_a(self, &v, &type_name));
            }
            StepAction::MatchEq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push_bool(match_values_equal(&a, &b));
            }
            StepAction::UnpackExact(n) => {
                let v = self.pop()?;
                let items = seq_items_for_unpack(&v)?;
                if items.len() != n {
                    return Err(RuntimeError::value_err(format!(
                        "expected {} values to unpack, got {}",
                        n,
                        items.len()
                    )));
                }
                for item in items {
                    self.push_value(item);
                }
            }
            StepAction::UnpackRest { before, after } => {
                let v = self.pop()?;
                let items = seq_items_for_unpack(&v)?;
                let need = before + after;
                if items.len() < need {
                    return Err(RuntimeError::value_err(format!(
                        "expected at least {} values to unpack, got {}",
                        need,
                        items.len()
                    )));
                }
                let rest_end = items.len() - after;
                for item in items.iter().take(before) {
                    self.push_value(item.clone());
                }
                let rest: Vec<Value> = items[before..rest_end].to_vec();
                self.push_value(Value::List(Shared::new(rest)));
                for item in items.iter().skip(rest_end) {
                    self.push_value(item.clone());
                }
            }
            StepAction::Rethrow => {
                if self.active_exception.is_none() {
                    return Err(RuntimeError::msg("rethrow outside except handler"));
                }
                self.try_stack.pop();
                return self.dispatch_to_handler().and_then(|ok| {
                    if ok {
                        Ok(())
                    } else {
                        Err(self.host_error_from_active())
                    }
                });
            }
            StepAction::TypeCheck => {
                let type_val = self.pop()?;
                let val = self.pop()?;
                if let Some(msg) = types::type_check_error(self, &val, &type_val) {
                    return self.raise_type_error(msg);
                }
                types::seal_container_contract(self, &val, &type_val);
                self.push_value(val);
            }
            StepAction::ResolveFuncTypes => {
                let v = self.pop()?;
                let Value::Function(f) = v else {
                    return Err(RuntimeError::type_err(
                        "ResolveFuncTypes expects a function",
                    ));
                };
                if f.types_resolved() {
                    self.push_value(Value::Function(f));
                } else {
                    let mut func = (*f).clone();
                    types::bind_function_annotations(self, &mut func)?;
                    self.push_value(Value::Function(Arc::new(func)));
                }
            }
            StepAction::FindMod(parts) => {
                // 首段走 loader；其余段按 getattr 链取子模块。
                let cur = module::find_module_segments(self, &parts)?;
                self.push_value(cur);
            }
            StepAction::FindModFile(path) => {
                let module = module::load_string_module(self, &path)?;
                self.push_value(module);
            }
            StepAction::RegisterExport(name) => {
                if let Some(ref exports) = self.module_init_exports {
                    let mut val = if let Some(v) = self.load_script_global_by_name(&name) {
                        v
                    } else {
                        self.load_name(&name)?
                    };
                    // NewVar 初值为 none；若 StoreGlobal 曾写错槽，script 槽可能仍是 none。
                    // 回退到 globals / load_name，避免静默导出 none。
                    if matches!(val, Value::None) {
                        if let Ok(v) = self.load_name(&name) {
                            if !matches!(v, Value::None) {
                                val = v;
                            }
                        }
                    }
                    exports.borrow_mut().insert(name, val);
                }
            }
            StepAction::GoCall { argc } => {
                let callee = self.pop()?;
                let mut args = Vec::with_capacity(argc);
                for _ in 0..argc {
                    args.push(self.pop()?);
                }
                args.reverse();
                let task = self.spawn_task(callee, args);
                self.push_value(task);
            }
            StepAction::GoValue => {
                let v = self.pop()?;
                let task = Self::task_from_value(v);
                self.push_value(task);
            }
            StepAction::Await => {
                let v = self.pop()?;
                let result = self.await_value(v.clone())?;
                if self.debug_break_requested {
                    // 任务内调试停点：回绕 Await，让主循环返回 DebugBreak。
                    self.push_value(v);
                    if self.pc > 0 {
                        self.pc -= 1;
                    }
                    return Ok(());
                }
                if self.block_suspend {
                    self.block_suspend = false;
                    self.push_value(v);
                    if self.pc > 0 {
                        self.pc -= 1;
                    }
                    self.pending_suspend = true;
                    return Ok(());
                }
                self.push_value(result);
            }
            StepAction::Suspend => {
                self.fail_if_current_task_cancelled()?;
                self.budget_left = self.suspend_budget;
                if self.task_ctx.is_some() {
                    self.pending_suspend = true;
                } else {
                    self.scheduler_yield()?;
                }
            }
            StepAction::Yield => {
                let v = self.pop()?;
                self.generator_yield_value(v)?;
            }
            StepAction::YieldFrom => {
                let iterable = self.pop()?;
                self.generator_yield_from(iterable)?;
            }
            StepAction::SelectTryRecv => {
                let ch = self.pop()?;
                let inner = match ch {
                    Value::Channel(inner) => inner,
                    Value::Stream(s) => match &*s.borrow() {
                        crate::value::StreamInner::Channel(inner) => inner.clone(),
                        crate::value::StreamInner::Iter(it) => {
                            match select_try_recv_from_iter(it)? {
                                Some(Some(v)) => {
                                    self.push_value(v);
                                    self.push_value(Value::Bool(true));
                                    return Ok(());
                                }
                                Some(None) => {
                                    self.push_value(Value::None);
                                    self.push_value(Value::Bool(true));
                                    return Ok(());
                                }
                                None => {
                                    self.push_value(Value::Bool(false));
                                    return Ok(());
                                }
                            }
                        }
                    },
                    _ => {
                        return Err(RuntimeError::type_err(
                            "select recv expects Channel or Stream",
                        ));
                    }
                };
                let outcome = inner.borrow_mut().try_recv();
                match outcome {
                    Some(Some(v)) => {
                        self.push_value(v);
                        self.push_value(Value::Bool(true));
                    }
                    Some(None) => {
                        self.push_value(Value::None);
                        self.push_value(Value::Bool(true));
                    }
                    None => {
                        self.push_value(Value::Bool(false));
                    }
                }
            }
            StepAction::SelectTrySend => {
                let val = self.pop()?;
                let ch = self.pop()?;
                let Value::Channel(inner) = ch else {
                    return Err(RuntimeError::type_err("select send expects Channel"));
                };
                let outcome = inner.borrow_mut().try_send(val);
                match outcome {
                    Some(Ok(())) => self.push_value(Value::Bool(true)),
                    Some(Err(())) => self.push_value(Value::Bool(false)),
                    None => self.push_value(Value::Bool(false)),
                }
            }
            StepAction::SelectPollTask => {
                let v = self.pop()?;
                let Value::Task(task) = v else {
                    // 非 Task：视为已完成
                    self.push_value(v);
                    self.push_value(Value::Bool(true));
                    return Ok(());
                };
                let state = task.borrow().state.clone();
                match state {
                    TaskState::Done(r) => {
                        self.push_value(r);
                        self.push_value(Value::Bool(true));
                    }
                    TaskState::Failed(e) => {
                        self.throw_value(e)?;
                    }
                    _ => {
                        self.push_value(Value::Bool(false));
                    }
                }
            }
            StepAction::MakeDeadline => {
                let secs = self.pop()?;
                let dl = crate::concurrency::deadline_from_secs(&secs)?;
                self.push_value(dl);
            }
            StepAction::SelectPollDeadline => {
                let dl = self.pop()?;
                let ready = crate::concurrency::poll_deadline_ready(&dl)?;
                self.push_value(Value::Bool(ready));
            }
            StepAction::SelectIdle(n) => {
                let mut deadlines = Vec::with_capacity(n);
                for _ in 0..n {
                    deadlines.push(self.pop()?);
                }
                deadlines.reverse();
                // 先推进墙钟（最近 sleep），再让其它任务跑；任务内 sleep 已协作切片，不会长时间霸住。
                crate::concurrency::sleep_until_nearest_deadline(&deadlines, SELECT_IDLE_CAP_MS)?;
                self.scheduler_yield()?;
            }
            StepAction::SelectBegin(n) => {
                self.select_fair_order.clear();
                self.select_fair_order.extend(0..n);
                // Fisher–Yates + 无偏取模（Lemire），避免书写顺序饿死。
                if n > 1 {
                    for i in (1..n).rev() {
                        let j = self.select_rng_below(i + 1);
                        self.select_fair_order.swap(i, j);
                    }
                }
                self.select_fair_pos = 0;
            }
            StepAction::SelectNextIndex => {
                if self.select_fair_pos >= self.select_fair_order.len() {
                    self.push_value(Value::Num(Num::Small(-1)));
                } else {
                    let idx = self.select_fair_order[self.select_fair_pos];
                    self.select_fair_pos += 1;
                    self.push_value(Value::Num(Num::Small(idx as i64)));
                }
            }
        }
        Ok(())
    }

    fn raise_type_error(&mut self, message: String) -> Result<()> {
        let exc = exceptions::make_exception(self, "TypeError", message)?;
        self.throw_value(exc)
    }

    fn pop(&mut self) -> Result<Value> {
        Ok(self.pop_sv()?.into_value())
    }

    fn enter_scope(&mut self) {
        self.locals_stack.push(Vec::new());
        self.name_to_slot.push(None);
    }

    fn leave_scope(&mut self) {
        if let Some(frame) = self.locals_stack.pop() {
            self.recycle_local_frame(frame);
            self.name_to_slot.pop();
        }
    }

    pub(crate) fn register_enum_def(&mut self, name: String, def: Arc<crate::value::EnumDef>) {
        // EnumDef.methods 已在构建时挂好（builtin + 用户定义）。
        self.enum_defs.insert(name, def);
    }

    pub(crate) fn load_name(&self, name: &str) -> Result<Value> {
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(slot) = map.get(name) {
                    if let Some(v) = self.locals_stack[i].get(*slot) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
            }
        }
        // 模块函数体内的裸 Load：优先定义模块快照（use/import 绑定落于此）。
        if let Some(env) = self.active_module_global_env() {
            if let Some(v) = env.globals.borrow().get(name) {
                return Ok(match v {
                    Value::Cell(c) => c.borrow().clone(),
                    other => other.clone(),
                });
            }
        }
        // 顶层热 Store 可能只写 script_globals；非 none 槽优先。
        // none 槽回退 SharedMap（friend Dispatch 等）；无键则 NameError（含 `del`）。
        if let Some(v) = self.live_script_fast_local(name) {
            return Ok(v);
        }
        if let Some(idx) = self.script_global_names.iter().position(|n| n == name) {
            if let Some(v) = self.script_globals.get(idx) {
                if !matches!(v, Value::None) {
                    return Ok(match v {
                        Value::Cell(c) => c.borrow().clone(),
                        other => other.clone(),
                    });
                }
            }
        }
        match self.globals.get(name) {
            Some(Value::Cell(c)) => Ok(c.borrow().clone()),
            Some(v) => Ok(v),
            None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
        }
    }

    pub(crate) fn get_binding(&self, name: &str) -> Result<Value> {
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(slot) = map.get(name) {
                    if let Some(v) = self.locals_stack[i].get(*slot) {
                        return Ok(v.clone());
                    }
                }
            }
        }
        self.globals
            .get(name)
            .ok_or_else(|| RuntimeError::name_err(format!("undefined name: {name}")))
    }

    pub(crate) fn upgrade_binding_to_cell(&mut self, name: &str) -> Result<Shared<Value>> {
        if let Value::Cell(cell) = self.get_binding(name)? {
            return Ok(cell);
        }
        let val = self.load_name(name)?;
        let cell = Shared::new(val);
        self.gc.track_cell(&cell);
        self.set_binding_raw(name, Value::Cell(cell.clone()));
        Ok(cell)
    }

    fn set_binding_raw(&mut self, name: &str, val: Value) {
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(slot) = map.get(name) {
                    let slot = *slot;
                    if slot >= self.locals_stack[i].len() {
                        self.locals_stack[i].resize(slot + 1, Value::None);
                    }
                    self.locals_stack[i][slot] = val;
                    return;
                }
            }
        }
        self.globals.insert(name.to_string(), val);
    }

    pub(crate) fn load_macro(&self, name: &str) -> Result<Value> {
        if let Some(m) = self.macros.get(name) {
            return Ok(Value::Macro(m));
        }
        match self.load_name(name)? {
            Value::Macro(m) => Ok(Value::Macro(m)),
            Value::Function(_) => Err(RuntimeError::msg(format!(
                "{name} is a function; macros use {{}} and functions use ()"
            ))),
            other => Err(RuntimeError::msg(format!(
                "undefined macro: {name} (got {})",
                other.type_name()
            ))),
        }
    }

    fn finalize_const_init(&mut self, name: &str) {
        if self.pending_const.remove(name) {
            self.has_pending_const = !self.pending_const.is_empty();
            self.const_names.insert(name.to_string());
            self.has_const_names = true;
        }
    }

    /// 拒绝向以 `const` 绑定的槽执行 `StoreFast`（热/冷路径共用）。
    #[inline(always)]
    fn reject_const_fast_store(&self, slot: usize) -> Result<()> {
        if !self.has_const_names {
            return Ok(());
        }
        if let Some(map) = self.name_to_slot.last().and_then(|m| m.as_ref()) {
            for (name, &s) in map {
                if s == slot && self.const_names.contains(name) {
                    return Err(RuntimeError::msg(format!(
                        "cannot assign to const binding: {name}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn store_name(&mut self, name: &str, val: Value) -> Result<()> {
        if self.const_names.contains(name) {
            return Err(RuntimeError::msg(format!(
                "cannot assign to const binding: {name}"
            )));
        }
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(slot) = map.get(name) {
                    let slot = *slot;
                    if let Some(Value::Cell(cell)) = self.locals_stack[i].get(slot) {
                        *cell.borrow_mut() = val;
                        self.finalize_const_init(name);
                        return Ok(());
                    }
                    if slot >= self.locals_stack[i].len() {
                        self.locals_stack[i].resize(slot + 1, Value::None);
                    }
                    self.locals_stack[i][slot] = val;
                    self.finalize_const_init(name);
                    return Ok(());
                }
            }
        }
        self.store_global_by_name(name, val);
        self.finalize_const_init(name);
        Ok(())
    }

    fn delete_name(&mut self, name: &str) -> Result<()> {
        if self.const_names.contains(name) {
            return Err(RuntimeError::msg("cannot delete const binding"));
        }
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &mut self.name_to_slot[i] {
                if let Some(slot) = map.remove(name) {
                    if slot < self.locals_stack[i].len() {
                        self.locals_stack[i][slot] = Value::None;
                    }
                    return Ok(());
                }
            }
        }
        // BindFast 在热路径里编成 H_STORE_FAST，不会写入 name_to_slot。
        // `del` 仍按函数体里的槽位清掉局部（含轻量帧）。
        if let Some(slot) = self.fast_local_slot(name) {
            self.local_set(slot, Value::None);
            if let Some(map) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                map.remove(name);
            }
            return Ok(());
        }
        if self.globals.remove(name).is_some() {
            if let Some(idx) = self.script_global_names.iter().position(|n| n == name) {
                if idx < self.script_globals.len() {
                    self.script_globals[idx] = Value::None;
                }
                self.sync_local_fn_hot(idx, &Value::None);
            }
            return Ok(());
        }
        Err(RuntimeError::name_err(format!("name not found: {name}")))
    }

    fn fast_local_slot(&self, name: &str) -> Option<usize> {
        let func = self.func_stack.last()?;
        for (i, p) in func.params.iter().enumerate() {
            if p.name == name {
                return Some(i);
            }
        }
        for ins in func.body.iter() {
            if let crate::opcode::Instruction::BindFast { slot, name: n, .. } = ins {
                if n == name {
                    return Some(*slot);
                }
            }
        }
        None
    }

    pub(crate) fn struct_has_method(&self, obj: &Value, method: &str) -> bool {
        let Value::Struct(s) = obj else {
            return false;
        };
        s.def.methods.contains_key(method) || s.def.overloads.contains_key(method)
    }

    pub(crate) fn try_call_magic(
        &mut self,
        obj: &Value,
        method: &str,
        args: Vec<Value>,
    ) -> Option<Result<Value>> {
        if self.struct_has_method(obj, method) {
            Some(self.call_struct_method(obj, method, args))
        } else {
            type_registry::try_call_primitive_magic(self, obj, method, args)
        }
    }

    fn magic_to_bool(result: Result<Value>) -> Result<bool> {
        Ok(result?.is_truthy())
    }

    fn dispatch_binary_arith(
        &mut self,
        a: &Value,
        b: &Value,
        method: &str,
        rmethod: &str,
        fallback: impl FnOnce(&Value, &Value) -> Result<Value>,
    ) -> Result<Value> {
        if let Some(r) = self.try_call_magic(a, method, vec![b.clone()]) {
            return r;
        }
        if let Some(r) = self.try_call_magic(b, rmethod, vec![a.clone()]) {
            return r;
        }
        fallback(a, b)
    }

    fn dispatch_neg(&mut self, a: &Value) -> Result<Value> {
        if let Some(r) = self.try_call_magic(a, "__neg__", vec![]) {
            return r;
        }
        a.neg()
    }

    fn dispatch_invert(&mut self, a: &Value) -> Result<Value> {
        if let Some(r) = self.try_call_magic(a, "__invert__", vec![]) {
            return r;
        }
        a.invert()
    }

    fn dispatch_eq(&mut self, a: &Value, b: &Value) -> Result<bool> {
        if let Some(r) = self.try_call_magic(a, "__eq__", vec![b.clone()]) {
            return Self::magic_to_bool(r);
        }
        a.eq(b)
    }

    fn dispatch_ne(&mut self, a: &Value, b: &Value) -> Result<bool> {
        if let Some(r) = self.try_call_magic(a, "__ne__", vec![b.clone()]) {
            return Self::magic_to_bool(r);
        }
        Ok(!self.dispatch_eq(a, b)?)
    }

    fn dispatch_compare(
        &mut self,
        a: &Value,
        b: &Value,
        method: &str,
        fallback: impl FnOnce(&Value, &Value) -> Result<bool>,
    ) -> Result<bool> {
        if let Some(r) = self.try_call_magic(a, method, vec![b.clone()]) {
            return Self::magic_to_bool(r);
        }
        fallback(a, b)
    }

    fn call_struct_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value> {
        let Value::Struct(s) = obj else {
            return Err(RuntimeError::msg("expected struct instance"));
        };
        if let Some(func) = s.def.methods.get(method).cloned() {
            let mut full_args = vec![obj.clone()];
            full_args.extend(args);
            return self.call_user_function(func, full_args);
        }
        if let Some(overloads) = s.def.overloads.get(method).cloned() {
            let mut full_args = vec![obj.clone()];
            full_args.extend(args);
            return self.dispatch_overload(&overloads, &full_args);
        }
        Err(RuntimeError::attr_err(format!(
            "{} has no method {method}",
            s.def.name
        )))
    }

    pub(crate) fn get_attr_value(&mut self, obj: &Value, field: &str) -> Result<Value> {
        get_attr(self, obj, field)
    }

    pub(crate) fn call_value(&mut self, callee: Value, args: Vec<Value>) -> Result<Value> {
        self.call_value_ex(callee, args, DictMap::new())
    }

    fn call_value_ex(
        &mut self,
        callee: Value,
        positional: Vec<Value>,
        kwargs: DictMap,
    ) -> Result<Value> {
        self.user_call_deferred = false;
        if !kwargs.is_empty() && !matches!(callee, Value::Function(_) | Value::GenericFunction(_)) {
            return Err(RuntimeError::type_err(
                "keyword arguments only supported for user functions",
            ));
        }
        let args = positional;
        match callee {
            Value::Struct(ref _s) if self.struct_has_method(&callee, "__call__") => {
                self.call_struct_method(&callee, "__call__", args)
            }
            Value::Builtin(b) => {
                let out = b.call(self, &args)?;
                if self.block_suspend {
                    self.block_suspend = false;
                    self.arm_call_retry(Value::Builtin(b), args);
                    return Ok(Value::None);
                }
                Ok(out)
            }
            Value::Function(func) => {
                if kwargs.is_empty()
                    && !self.debug_active
                    && (func.hot_call_argc as usize) == args.len()
                {
                    self.setup_lightweight_user_call(func, args)?;
                    self.user_call_deferred = true;
                    return Ok(Value::None);
                }
                let bound = self.bind_call_arguments(&func, args, kwargs)?;
                if func.is_generator() {
                    return self.make_generator_iterator(func, bound);
                }
                self.setup_user_call(func, bound, false)?;
                self.user_call_deferred = true;
                Ok(Value::None)
            }
            Value::GenericFunction(template) => {
                let type_args = infer_generic_type_args_from_values(self, &template, &args)?;
                let func = specialize_generic_runtime(self, &template, type_args)?;
                let bound = self.bind_call_arguments(&func, args, kwargs)?;
                if func.is_generator() {
                    return self.make_generator_iterator(func, bound);
                }
                self.setup_user_call(func, bound, false)?;
                self.user_call_deferred = true;
                Ok(Value::None)
            }
            Value::Macro(_) => Err(RuntimeError::msg(
                "macro cannot be called with (); use {} syntax",
            )),
            Value::Dispatch(table) => self.call_dispatch(&table, args),
            Value::TypeRef(ref type_name) if self.struct_defs.contains_key(type_name) => {
                make_struct(self, type_name, args, None)
            }
            Value::TypeRef(ref type_name) if self.variant_defs.contains_key(type_name) => {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err(format!(
                        "variant {type_name} expects 1 argument, got {}",
                        args.len()
                    )));
                }
                wrap_variant_payload(
                    self,
                    type_name,
                    None,
                    args.into_iter()
                        .next()
                        .expect("variant arg count checked above (theoretically unreachable)"),
                )
            }
            Value::TypeRef(ref type_name) => {
                let n_args = args.len();
                if let Some(result) = type_registry::call_primitive_ctor(self, type_name, args) {
                    return result;
                }
                Err(RuntimeError::type_err(format!(
                    "{type_name} is not callable with {n_args} argument(s)"
                )))
            }
            Value::TypeSpec(spec) if self.variant_defs.contains_key(&spec.name) => {
                if args.len() != 1 {
                    return Err(RuntimeError::type_err(format!(
                        "variant {} expects 1 argument, got {}",
                        spec.name,
                        args.len()
                    )));
                }
                wrap_variant_payload(
                    self,
                    &spec.name,
                    Some(spec.args.clone()),
                    args.into_iter()
                        .next()
                        .expect("variant arg count checked above (theoretically unreachable)"),
                )
            }
            Value::TypeSpec(spec) => make_struct(self, &spec.name, args, Some(spec.args.clone())),
            other => Err(RuntimeError::type_err(format!(
                "value is not callable: {}",
                other.type_name()
            ))),
        }
    }

    /// 将位置参数与关键字参数绑定到函数形参槽。
    fn bind_call_arguments(
        &mut self,
        func: &FunctionObject,
        positional: Vec<Value>,
        mut kwargs: DictMap,
    ) -> Result<Vec<Value>> {
        let n = func.params.len();
        let mut bound: Vec<Option<Value>> = (0..n).map(|_| None).collect();
        let var_i = func.variadic_param_index;
        let kwvar_i = func.kwvariadic_param_index;

        let mut ai = 0usize;
        for (pi, slot) in bound.iter_mut().enumerate() {
            if Some(pi) == var_i || Some(pi) == kwvar_i {
                break;
            }
            if ai >= positional.len() {
                break;
            }
            *slot = Some(positional[ai].clone());
            ai += 1;
        }

        if let Some(vi) = var_i {
            let rest: Vec<Value> = positional[ai..].to_vec();
            let list = Value::List(Shared::new(rest));
            self.track_value(&list);
            bound[vi] = Some(list);
        } else if ai < positional.len() {
            return Err(RuntimeError::msg(format!(
                "{}() takes {} argument(s) but {} were given",
                func.name,
                n,
                positional.len()
            )));
        }

        for (pi, param) in func.params.iter().enumerate() {
            if param.is_variadic || param.is_kwvariadic {
                continue;
            }
            let key = ValueKey::from_value(&Value::Text(param.name.clone()))?;
            if let Some(v) = kwargs.remove(&key) {
                if bound[pi].is_some() {
                    return Err(RuntimeError::msg(format!(
                        "{}() got multiple values for argument '{}'",
                        func.name, param.name
                    )));
                }
                bound[pi] = Some(v);
            }
        }

        if let Some(ki) = kwvar_i {
            let dict = Value::Dict(Shared::new(kwargs));
            self.track_value(&dict);
            bound[ki] = Some(dict);
        } else if !kwargs.is_empty() {
            let names: Vec<String> = kwargs.keys().map(value_key_to_display).collect();
            return Err(RuntimeError::msg(format!(
                "{}() got unexpected keyword argument(s): {}",
                func.name,
                names.join(", ")
            )));
        }

        for (pi, param) in func.params.iter().enumerate() {
            if bound[pi].is_some() {
                continue;
            }
            if param.is_variadic {
                let list = Value::List(Shared::new(Vec::new()));
                self.track_value(&list);
                bound[pi] = Some(list);
            } else if param.is_kwvariadic {
                let dict = Value::Dict(Shared::new(DictMap::new()));
                self.track_value(&dict);
                bound[pi] = Some(dict);
            } else if let Some(Some(d)) = func.defaults.get(pi) {
                bound[pi] = Some(d.clone());
            } else if let Some(expr) = &param.default_expr {
                if let Some(v) = const_default_value_runtime(expr) {
                    bound[pi] = Some(v);
                } else {
                    return Err(RuntimeError::msg(format!(
                        "{}() missing required argument '{}'",
                        func.name, param.name
                    )));
                }
            } else {
                return Err(RuntimeError::msg(format!(
                    "{}() missing required argument '{}'",
                    func.name, param.name
                )));
            }
        }

        bound
            .into_iter()
            .map(|v| v.ok_or_else(|| RuntimeError::msg("internal: unbound argument slot")))
            .collect()
    }
    fn resolve_macro_args(mac: &MacroObject, args: &[Value]) -> Result<Vec<Value>> {
        let param_count = mac.params.len();
        if let Some(vi) = mac.variadic_param_index {
            if args.len() < vi {
                return Err(RuntimeError::type_err(format!(
                    "macro {} expects at least {} argument(s), got {}",
                    mac.name,
                    vi,
                    args.len()
                )));
            }
            let mut resolved = vec![Value::None; param_count];
            for (i, arg) in args.iter().take(vi).enumerate() {
                resolved[i] = arg.clone();
            }
            let packed: Vec<Value> = args.iter().skip(vi).cloned().collect();
            resolved[vi] = Value::List(Shared::new(packed));
            Ok(resolved)
        } else if args.len() > param_count {
            Err(RuntimeError::type_err(format!(
                "macro {} expects at most {} argument(s), got {}",
                mac.name,
                param_count,
                args.len()
            )))
        } else {
            let mut resolved = vec![Value::None; param_count];
            for (i, arg) in args.iter().enumerate() {
                resolved[i] = arg.clone();
            }
            Ok(resolved)
        }
    }

    fn call_macro(&mut self, mac: Arc<MacroObject>, args: Vec<Value>) -> Result<Value> {
        for (i, arg) in args.iter().enumerate() {
            if !matches!(arg, Value::RuntimeAst(_)) {
                return Err(RuntimeError::type_err(format!(
                    "macro argument {} must be AST (frozen at parse time), got {}",
                    i,
                    arg.type_name()
                )));
            }
        }
        let resolved = Self::resolve_macro_args(&mac, &args)?;

        for (i, param) in mac.params.iter().enumerate() {
            if param.is_variadic {
                continue;
            }
            if let (Some(ty), true) = (&param.type_expr, param.type_strong) {
                if let Some(Value::RuntimeAst(ast)) = resolved.get(i) {
                    runtime_ast::check_macro_param_ast_kind(ty, ast)?;
                }
            }
        }

        self.macro_eval_scopes.push(self.snapshot_for_eval());
        let result = self.run_macro_body(mac, resolved)?;
        let expanded = match result {
            Value::RuntimeAst(ast) => runtime_ast::eval_ast_value(self, &ast)?,
            other => {
                return Err(RuntimeError::msg(format!(
                    "macro must return AST, got {}",
                    other.type_name()
                )));
            }
        };
        self.macro_eval_scopes.pop();
        Ok(expanded)
    }

    fn run_macro_body(&mut self, mac: Arc<MacroObject>, args: Vec<Value>) -> Result<Value> {
        self.enter_scope();
        for (i, param) in mac.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::None);
            if let Some(locals) = self.locals_stack.last_mut() {
                locals.insert(i, val);
            }
            if !self.name_to_slot.is_empty() {
                let frame = self.locals_stack.len() - 1;
                self.scope_name_map_mut(frame).insert(param.name.clone(), i);
            }
        }

        let saved_code = self.code.clone();
        let saved_hot_ops = self.hot_ops.clone();
        let saved_hot_args = self.hot_args.clone();
        let saved_pc = self.pc;
        self.code = mac.body.clone();
        let mac_hot = crate::hot_code::HotCode::encode(&mac.body);
        self.hot_ops = mac_hot.ops;
        self.hot_args = mac_hot.args;
        self.pc = 0;

        let result = match self.run_interpreter(None) {
            Ok(InterpResult::Value(v)) => v.unwrap_or_else(|| self.stack_top()),
            Ok(InterpResult::Suspended) => {
                self.leave_scope();
                self.code = saved_code;
                self.hot_ops = saved_hot_ops;
                self.hot_args = saved_hot_args;
                self.pc = saved_pc;
                return Err(RuntimeError::msg(
                    "internal error: task suspended inside macro expansion",
                ));
            }
            Ok(InterpResult::DebugBreak) => {
                self.leave_scope();
                self.code = saved_code;
                self.hot_ops = saved_hot_ops;
                self.hot_args = saved_hot_args;
                self.pc = saved_pc;
                return Err(RuntimeError::msg(
                    "internal error: debug break inside macro expansion",
                ));
            }
            Ok(InterpResult::Yielded(_)) => {
                self.leave_scope();
                self.code = saved_code;
                self.hot_ops = saved_hot_ops;
                self.hot_args = saved_hot_args;
                self.pc = saved_pc;
                return Err(RuntimeError::msg(
                    "internal error: generator yield inside macro expansion",
                ));
            }
            Err(e) => {
                self.leave_scope();
                self.code = saved_code;
                self.hot_ops = saved_hot_ops;
                self.hot_args = saved_hot_args;
                self.pc = saved_pc;
                return Err(e);
            }
        };

        self.leave_scope();
        self.code = saved_code;
        self.hot_ops = saved_hot_ops;
        self.hot_args = saved_hot_args;
        self.pc = saved_pc;
        Ok(result)
    }

    pub(crate) fn value_is_truthy(&mut self, val: &Value) -> bool {
        if let Some(r) = self.try_call_magic(val, "__bool__", vec![]) {
            return r.is_ok_and(|v| v.is_truthy());
        }
        val.is_truthy()
    }

    pub(crate) fn value_contains(&mut self, container: &Value, item: &Value) -> Result<bool> {
        if let Some(r) = self.try_call_magic(container, "__contains__", vec![item.clone()]) {
            return r.map(|v| v.is_truthy());
        }
        match container {
            Value::List(v) => {
                for elem in v.borrow().iter() {
                    if self.dispatch_eq(elem, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Text(s) => {
                if let Value::Text(needle) = item {
                    Ok(s.contains(needle.as_str()))
                } else {
                    Ok(false)
                }
            }
            Value::Dict(d) => {
                let key = ValueKey::from_value(item)?;
                Ok(d.borrow().contains_key(&key))
            }
            Value::Set(s) => {
                let key = ValueKey::from_value(item)?;
                Ok(s.borrow().contains(&key))
            }
            Value::Tuple(t) => {
                for elem in t.iter() {
                    if self.dispatch_eq(elem, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Bytes(b) => match item {
                Value::Num(n) => {
                    let Some(v) = n.to_i64() else {
                        return Ok(false);
                    };
                    if !(0..=255).contains(&v) {
                        return Ok(false);
                    }
                    Ok(b.contains(&(v as u8)))
                }
                Value::Bytes(other) => Ok(b.windows(other.len()).any(|w| w == other.as_slice())),
                _ => Ok(false),
            },
            _ => {
                // 用户类型：走 `__iter__` / `__next__` 协议，不要求先物化为 list。
                let it = self.to_iterator_shared(container)?;
                while let Some(elem) = self.advance_iterator(&it)? {
                    if self.dispatch_eq(&elem, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    /// 将任意可迭代对象转为共享 iterator 状态。
    /// 内置序列走内建游标；用户 `struct` 走 `__iter__` / `__next__` 协议。
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_iterator_shared(&mut self, v: &Value) -> Result<Shared<IteratorState>> {
        match v {
            Value::Iterator(it) => Ok(it.clone()),
            Value::Stream(_) => crate::value::value_to_iterator_shared(v),
            other => {
                if let Some(r) = self.try_call_magic(other, "__iter__", vec![]) {
                    let it_val = r?;
                    return self.iterator_from_protocol_value(it_val);
                }
                if self.struct_has_method(other, "__next__") {
                    return self.wrap_user_iterator(other.clone());
                }
                crate::value::value_to_iterator_shared(other)
            }
        }
    }

    fn wrap_user_iterator(&mut self, obj: Value) -> Result<Shared<IteratorState>> {
        if !self.struct_has_method(&obj, "__next__") {
            return Err(RuntimeError::type_err(
                "__iter__ must return an object with __next__",
            ));
        }
        let it = Shared::new(IteratorState {
            kind: IteratorKind::User { obj },
        });
        self.gc.track_iter(&it);
        Ok(it)
    }

    fn iterator_from_protocol_value(&mut self, it_val: Value) -> Result<Shared<IteratorState>> {
        match it_val {
            Value::Iterator(it) => Ok(it),
            Value::List(_)
            | Value::Tuple(_)
            | Value::Set(_)
            | Value::Bytes(_)
            | Value::Text(_)
            | Value::Dict(_)
            | Value::Channel(_)
            | Value::Stream(_) => Ok(Shared::new(crate::value::value_to_iterable(&it_val)?)),
            other => self.wrap_user_iterator(other),
        }
    }

    pub(crate) fn get_or_create_dispatch(&mut self, name: &str) -> Shared<DispatchTable> {
        if let Some(t) = self.globals.get(name).and_then(|v| {
            if let Value::Dispatch(t) = v {
                Some(t)
            } else {
                None
            }
        }) {
            return t;
        }
        let table = Shared::new(DispatchTable {
            name: name.to_string(),
            handlers: Shared::new(Vec::new()),
        });
        self.store_global_by_name(name, Value::Dispatch(table.clone()));
        table
    }

    pub(crate) fn get_or_create_convert(&mut self, type_name: &str) -> Shared<DispatchTable> {
        let key = format!("__convert__:{type_name}");
        self.convert_tables
            .entry(key.clone())
            .or_insert_with(|| {
                Shared::new(DispatchTable {
                    name: key,
                    handlers: Shared::new(Vec::new()),
                })
            })
            .clone()
    }

    fn call_dispatch(&mut self, table: &Shared<DispatchTable>, args: Vec<Value>) -> Result<Value> {
        enum DispatchTarget {
            Function(Arc<FunctionObject>),
            Builtin(Arc<crate::value::BuiltinObject>),
        }

        let handlers = table.borrow().handlers.borrow().clone();
        let mut best: Option<(usize, usize, DispatchTarget)> = None;
        for (idx, handler_val) in handlers.iter().enumerate() {
            let (score, target) = match handler_val {
                Value::Function(func) => {
                    let func = self.ensure_func_types_resolved(func.clone())?;
                    let Some(score) = types::dispatch_match_score(self, &func, &args) else {
                        continue;
                    };
                    (score, DispatchTarget::Function(func))
                }
                Value::Builtin(b) => (usize::MAX, DispatchTarget::Builtin(b.clone())),
                _ => continue,
            };
            match &best {
                None => best = Some((score, idx, target)),
                Some((best_score, best_idx, _)) => {
                    if score < *best_score || (score == *best_score && idx < *best_idx) {
                        best = Some((score, idx, target));
                    }
                }
            }
        }
        if let Some((_, _, target)) = best {
            return match target {
                DispatchTarget::Function(func) => {
                    let bound = self.bind_call_arguments(&func, args, DictMap::new())?;
                    if func.is_generator() {
                        return self.make_generator_iterator(func, bound);
                    }
                    self.setup_user_call(func, bound, false)?;
                    self.user_call_deferred = true;
                    Ok(Value::None)
                }
                DispatchTarget::Builtin(b) => {
                    let out = b.call(self, &args)?;
                    if self.block_suspend {
                        self.block_suspend = false;
                        self.arm_call_retry(Value::Builtin(b), args);
                        return Ok(Value::None);
                    }
                    Ok(out)
                }
            };
        }
        let table_name = table.borrow().name.clone();
        if let Some(type_name) = table_name.strip_prefix("__convert__:") {
            let src = args.get(1).map_or("?", super::value::Value::type_name);
            return Err(RuntimeError::type_err(format!(
                "no matching __convert__ handler for {type_name} from {src}"
            )));
        }
        Err(RuntimeError::type_err(format!(
            "no matching __dispatch__ for {table_name}"
        )))
    }

    pub(crate) fn convert_type(&mut self, type_expr: Value, value: Value) -> Result<Value> {
        let type_name = match &type_expr {
            Value::TypeRef(s) | Value::Text(s) => s.clone(),
            other => other.type_name_string(),
        };
        if let Some(result) = try_variant_case_convert(self, &type_name, &value) {
            return result;
        }
        let table = self.get_or_create_convert(&type_name);
        let args = vec![Value::type_ref(type_name.clone()), value];
        self.call_dispatch(&table, args)
    }

    pub(crate) fn macro_eval_scope(&self) -> Option<&EvalSnapshot> {
        self.macro_eval_scopes.last()
    }

    pub(crate) fn snapshot_for_eval(&self) -> EvalSnapshot {
        EvalSnapshot {
            globals: self.globals.clone(),
            locals_stack: self.locals_stack.clone(),
            name_to_slot: self.name_to_slot.clone(),
            code: self.code.clone(),
            hot_ops: self.hot_ops.clone(),
            hot_args: self.hot_args.clone(),
            active_line_map: self.active_line_map.clone(),
            active_column_map: self.active_column_map.clone(),
            pc: self.pc,
            stack: self.stack.get(..self.stack_sp).unwrap_or(&[]).to_vec(),
            functions: self.functions.snapshot_map(),
            macros: self.macros.snapshot_map(),
            struct_defs: self.struct_defs.snapshot_map(),
            enum_defs: self.enum_defs.snapshot_map(),
            variant_defs: self.variant_defs.snapshot_map(),
            script_global_names: self.script_global_names.clone(),
            script_globals: self.script_globals.clone(),
            script_frame_slots: self.script_frame_slots,
            script_local_to_global: self.script_local_to_global.clone(),
            lw_slots: self.lw_slots.clone(),
            lw_bases: self.lw_bases.clone(),
            lw_bases_sp: self.lw_bases_sp,
            lw_sp: self.lw_sp,
            lw_base: self.lw_base,
            lw_depth: self.lw_depth,
        }
    }

    pub(crate) fn restore_eval_snapshot(&mut self, snap: EvalSnapshot) {
        self.globals = snap.globals;
        self.locals_stack = snap.locals_stack;
        self.name_to_slot = snap.name_to_slot;
        self.code = snap.code;
        self.hot_ops = snap.hot_ops;
        self.hot_args = snap.hot_args;
        self.active_line_map = snap.active_line_map;
        self.active_column_map = snap.active_column_map;
        self.pc = snap.pc;
        self.stack = snap.stack;
        self.stack_sp = self.stack.len();
        self.functions.replace_with(snap.functions);
        self.macros.replace_with(snap.macros);
        self.struct_defs.replace_with(snap.struct_defs);
        self.enum_defs.replace_with(snap.enum_defs);
        self.variant_defs.replace_with(snap.variant_defs);
        self.script_global_names = snap.script_global_names;
        self.script_globals = snap.script_globals;
        self.script_frame_slots = snap.script_frame_slots;
        self.script_local_to_global = snap.script_local_to_global;
        self.lw_slots = snap.lw_slots;
        self.lw_bases = snap.lw_bases;
        self.lw_bases_sp = snap.lw_bases_sp;
        self.lw_sp = snap.lw_sp;
        self.lw_base = snap.lw_base;
        self.lw_depth = snap.lw_depth;
    }

    pub(crate) fn run_snippet(&mut self, program: CompiledProgram) -> Result<()> {
        let saved = self.snapshot_for_eval();
        if let Err(e) = self.run_snippet_keep(program) {
            self.restore_eval_snapshot(saved);
            return Err(e);
        }
        Ok(())
    }

    /// 加载并跑一段程序；不恢复快照（由调用方负责）。
    pub(crate) fn run_snippet_keep(&mut self, program: CompiledProgram) -> Result<()> {
        self.functions.extend(program.functions);
        self.macros.extend(program.macros);
        self.struct_defs.extend(program.struct_defs);
        self.enum_defs.extend(program.enum_defs);
        self.variant_defs.extend(program.variant_defs);
        // 顶层热 Store 可能只写平行槽；snippet 按 SharedMap 重建槽前必须先刷，
        // 否则 `eval(quote { a })` / 宏展开会读到 NewVar 的 none 或 NameError。
        self.flush_script_globals_to_map();
        self.script_frame_slots = program.script_frame_slots;
        self.script_local_to_global = program.script_local_to_global;
        self.init_script_globals(program.global_names);
        self.prepare_script_fast_frame();
        self.code = Arc::new(program.code);
        self.hot_ops = program.hot.ops.clone();
        self.hot_args = program.hot.args.clone();
        self.active_line_map = Arc::new(program.line_map);
        self.active_column_map = Arc::new(program.column_map);
        self.pc = 0;
        self.op_clear();
        self.run_interpreter(None)?;
        Ok(())
    }

    pub(crate) fn push_quote_binding_scope(&mut self, quote: &RuntimeAstNode) -> Result<()> {
        if quote.binding_names.is_empty() {
            return Ok(());
        }
        self.enter_scope();
        for (i, name) in quote.binding_names.iter().enumerate() {
            let val = quote
                .bindings
                .get(i)
                .map(runtime_ast::quote_binding_to_value)
                .transpose()?
                .unwrap_or(Value::None);
            if let Some(locals) = self.locals_stack.last_mut() {
                if i >= locals.len() {
                    locals.resize(i + 1, Value::None);
                }
                locals[i] = val;
            }
            if !self.name_to_slot.is_empty() {
                let frame = self.locals_stack.len() - 1;
                self.scope_name_map_mut(frame).insert(name.clone(), i);
            }
        }
        Ok(())
    }

    fn value_is_truthy_fast(&mut self, val: &Value) -> bool {
        match val {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Num(n) => !n.is_zero(),
            Value::Text(s) => !s.is_empty(),
            Value::List(v) => !v.borrow().is_empty(),
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            _ => self.value_is_truthy(val),
        }
    }

    pub(crate) fn current_line(&self) -> usize {
        if self.pc == 0 {
            return self.active_line_map.first().copied().unwrap_or(0);
        }
        self.active_line_map
            .get(self.pc.saturating_sub(1))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn call_user_function_catching(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
    ) -> std::result::Result<std::result::Result<Value, Value>, RuntimeError> {
        match self.call_user_function(func, args) {
            Ok(v) => Ok(Ok(v)),
            Err(e) => {
                if let Some(exc) = self.active_exception.take() {
                    Ok(Err(exc))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// 若尚未在定义处绑定类型注解，立即求值并缓存（方法等未走 `ResolveFuncTypes` 的路径）。
    fn ensure_func_types_resolved(
        &mut self,
        func: Arc<FunctionObject>,
    ) -> Result<Arc<FunctionObject>> {
        if func.types_resolved() {
            return Ok(func);
        }
        let mut f = (*func).clone();
        types::bind_function_annotations(self, &mut f)?;
        Ok(Arc::new(f))
    }

    fn check_strong_params(&mut self, func: &FunctionObject, args: &[Value]) -> Result<()> {
        for (i, param) in func.params.iter().enumerate() {
            if param.is_variadic {
                continue;
            }
            if !param.type_strong {
                continue;
            }
            let Some(ty) = func.param_types.get(i).and_then(|t| t.as_ref()) else {
                continue;
            };
            if let Some(val) = args.get(i) {
                if let Some(detail) = types::type_check_error(self, val, ty) {
                    let msg = format!("parameter '{}': {detail}", param.name);
                    let exc = exceptions::make_exception(self, "TypeError", msg)?;
                    self.throw_value(exc)?;
                } else {
                    types::seal_container_contract(self, val, ty);
                }
            }
        }
        Ok(())
    }

    fn apply_implicit_param_converts(
        &mut self,
        func: &FunctionObject,
        mut args: Vec<Value>,
    ) -> Result<Vec<Value>> {
        for (i, param) in func.params.iter().enumerate() {
            if !param.implicit {
                continue;
            }
            let Some(ty_val) = func.param_types.get(i).and_then(|t| t.as_ref()) else {
                continue;
            };
            let Some(val) = args.get(i).cloned() else {
                continue;
            };
            if types::type_accepts(self, &val, ty_val) {
                continue;
            }
            let type_name = types::type_value_base(ty_val)
                .map(str::to_string)
                .ok_or_else(|| {
                    RuntimeError::type_err(format!(
                        "parameter '{}': cannot resolve implicit convert target type",
                        param.name
                    ))
                })?;
            match self.convert_type(Value::type_ref(type_name), val) {
                Ok(converted) => {
                    args[i] = converted;
                }
                Err(e) => {
                    let msg = format!("parameter '{}': implicit convert failed: {e}", param.name);
                    let exc = exceptions::make_exception(self, "TypeError", msg)?;
                    self.throw_value(exc)?;
                }
            }
        }
        Ok(args)
    }

    pub(crate) fn check_list_element_write(
        &mut self,
        list: &Shared<Vec<Value>>,
        elem: &Value,
    ) -> Result<()> {
        let ptr = list.as_ptr() as usize;
        let Some(ty) = self.list_element_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&ty, elem, "[*]")
    }

    pub(crate) fn check_dict_write(
        &mut self,
        dict: &Shared<crate::value::DictMap>,
        key: &Value,
        val: &Value,
    ) -> Result<()> {
        let ptr = dict.as_ptr() as usize;
        let Some((kty, vty)) = self.dict_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&kty, key, "[key]")?;
        self.check_element_against(&vty, val, &format!("[{}]", key.print_string()))
    }

    pub(crate) fn check_set_element_write(
        &mut self,
        set: &Shared<crate::value::SetMap>,
        elem: &Value,
    ) -> Result<()> {
        let ptr = set.as_ptr() as usize;
        let Some(ty) = self.set_element_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&ty, elem, "{*}")
    }

    fn check_element_against(&mut self, ty: &Value, elem: &Value, path: &str) -> Result<()> {
        if types::type_accepts(self, elem, ty) {
            return Ok(());
        }
        let msg = format!(
            "expected {}, got {} at {path}",
            type_value_display(ty),
            elem.type_name()
        );
        self.raise_type_error(msg)
    }

    pub(crate) fn call_user_function(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
    ) -> Result<Value> {
        match self.call_user_function_poll(func, args)? {
            InterpResult::Value(v) => Ok(v.unwrap_or(Value::None)),
            InterpResult::Suspended => {
                self.nested_user_call_suspended = true;
                Ok(Value::None)
            }
            InterpResult::DebugBreak => Err(RuntimeError::msg(
                "internal error: debug break outside debugger session",
            )),
            InterpResult::Yielded(_) => Err(RuntimeError::msg(
                "internal error: generator yield outside iterator",
            )),
        }
    }

    /// opcode Call 之后：处理重试、延迟调用、以及嵌套用户函数挂起。
    fn finish_value_call(&mut self, result: Value) -> Result<()> {
        if self.nested_user_call_suspended {
            if self.task_ctx.is_none() {
                self.pending_suspend = true;
            }
            return Ok(());
        }
        if self.call_retry_armed {
            self.call_retry_armed = false;
            return Ok(());
        }
        if !self.user_call_deferred && self.active_exception.is_none() {
            self.push_value(result);
        }
        Ok(())
    }

    fn take_task_suspend(&mut self) -> Result<InterpResult> {
        if self.task_ctx.is_some() {
            return self.complete_task_suspend();
        }
        if self.nested_user_call_suspended {
            self.nested_user_call_suspended = false;
            self.pending_suspend = false;
            return Ok(InterpResult::Suspended);
        }
        self.complete_task_suspend()
    }

    fn call_user_function_poll(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
    ) -> Result<InterpResult> {
        let bound = self.bind_call_arguments(&func, args, DictMap::new())?;
        if func.is_generator() {
            return Ok(InterpResult::Value(Some(
                self.make_generator_iterator(func, bound)?,
            )));
        }
        let stack_base = self.stack_sp;
        let stop_depth = self.user_call_frames.len();
        self.setup_user_call(func, bound, false)?;
        match self.run_interpreter(Some(stop_depth))? {
            InterpResult::Value(v) => {
                self.op_truncate(stack_base);
                Ok(InterpResult::Value(Some(v.unwrap_or(Value::None))))
            }
            InterpResult::Suspended => Ok(InterpResult::Suspended),
            InterpResult::DebugBreak => Ok(InterpResult::DebugBreak),
            InterpResult::Yielded(v) => Ok(InterpResult::Yielded(v)),
        }
    }

    fn setup_user_call(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
        reenter: bool,
    ) -> Result<()> {
        if self.user_call_frames.len() >= self.cached_max_depth {
            return Err(RuntimeError::recursion_err(
                "maximum recursion depth exceeded",
            ));
        }
        let func = self.ensure_func_types_resolved(func)?;
        let args = self.apply_implicit_param_converts(&func, args)?;
        if self.active_exception.is_some() {
            return Ok(());
        }
        self.check_strong_params(&func, &args)?;
        if self.active_exception.is_some() {
            return Ok(());
        }

        if func.track_frames() {
            let call_line = self.current_line();
            self.func_frames.push(FuncFrame {
                name: func.name.clone(),
                file: self.source_file.clone(),
                line: call_line,
            });
        }

        if !reenter {
            self.func_stack.push(func.clone());
        }

        if func.uses_name_map() {
            self.name_to_slot.push(Some(FxHashMap::default()));
        } else {
            self.name_to_slot.push(None);
        }

        let captured_len = func
            .captured
            .as_ref()
            .map_or(0, std::collections::HashMap::len);
        let frame_size = func.frame_slots.max(func.params.len() + captured_len);
        let mut locals = self.alloc_local_frame(frame_size);

        let mut slot = func.params.len();
        if func.uses_name_map() {
            if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                if let Some(captured) = &func.captured {
                    let mut caps: Vec<_> = captured.iter().collect();
                    caps.sort_by(|a, b| a.0.cmp(b.0));
                    for (name, val) in caps {
                        if func.params.iter().any(|p| p.name == *name) {
                            continue;
                        }
                        if slot < locals.len() {
                            locals[slot] = val.clone();
                        }
                        names.insert(name.clone(), slot);
                        slot += 1;
                    }
                }
            }
        } else if let Some(captured) = &func.captured {
            let mut caps: Vec<_> = captured.iter().collect();
            caps.sort_by(|a, b| a.0.cmp(b.0));
            for (name, val) in caps {
                if func.params.iter().any(|p| p.name == *name) {
                    continue;
                }
                if slot < locals.len() {
                    locals[slot] = val.clone();
                }
                let _ = name;
                slot += 1;
            }
        }

        for (i, val) in args.into_iter().enumerate() {
            if i < locals.len() {
                locals[i] = val;
            }
            if func.uses_name_map() {
                if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                    if let Some(param) = func.params.get(i) {
                        names.insert(param.name.clone(), i);
                    }
                }
            }
        }
        // 确保所有形参名都在 name map 中（含仅默认值 / *args / **kwargs）。
        if func.uses_name_map() {
            if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                for (i, param) in func.params.iter().enumerate() {
                    names.entry(param.name.clone()).or_insert(i);
                }
            }
        }
        self.locals_stack.push(locals);

        let saved_lw_depth = self.lw_depth;
        let saved_lw_base = self.lw_base;
        // 脚本未逃逸局部占用 lw 帧；重函数的 LoadFast 走 locals_stack，须暂时摘掉。
        self.lw_depth = 0;
        self.user_call_frames.push(UserCallFrame {
            saved_code: self.code.clone(),
            saved_hot_ops: self.hot_ops.clone(),
            saved_hot_args: self.hot_args.clone(),
            saved_pc: self.pc,
            saved_line_map: self.active_line_map.clone(),
            saved_column_map: self.active_column_map.clone(),
            func: func.clone(),
            pushed_func_stack: !reenter,
            pushed_name_frame: true,
            saved_lw_depth,
            saved_lw_base,
        });
        self.code = func.body.clone();
        validate_function_hot(&func)?;
        self.hot_ops = func.hot.ops.clone();
        self.hot_args = func.hot.args.clone();
        self.active_line_map = func.line_map.clone();
        self.active_column_map = func.column_map.clone();
        self.pc = 0;
        Ok(())
    }

    /// 轻量函数的普通 `Call`（参数已是 `Vec`）：转到栈版入口。
    fn setup_lightweight_user_call(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
    ) -> Result<()> {
        let n_args = args.len();
        for a in args {
            self.op_push(StackVal::from_value(a));
        }
        self.setup_lightweight_user_call_stack(func, n_args)
    }

    /// 从操作数栈弹出 `argc` 个参数（顶为最后一参）并进入轻量调用。
    fn setup_lightweight_user_call_stack(
        &mut self,
        func: Arc<FunctionObject>,
        argc: usize,
    ) -> Result<()> {
        if self.user_call_frames.len() >= self.cached_max_depth {
            return Err(RuntimeError::recursion_err(
                "maximum recursion depth exceeded",
            ));
        }
        if self.lw_depth >= self.cached_max_depth {
            return Err(RuntimeError::recursion_err(
                "maximum recursion depth exceeded",
            ));
        }
        if self.stack_sp < argc {
            return Err(RuntimeError::msg("stack underflow"));
        }

        self.func_stack.push(func.clone());

        let nslots = func.frame_slots.max(argc);
        let entry_pc = func.entry_pc;
        let frame_slots = func.frame_slots;
        let saved_lw_depth = self.lw_depth;
        let saved_lw_base = self.lw_base;
        self.user_call_frames.push(UserCallFrame {
            saved_code: std::mem::replace(&mut self.code, func.body.clone()),
            saved_hot_ops: std::mem::replace(&mut self.hot_ops, func.hot.ops.clone()),
            saved_hot_args: std::mem::replace(&mut self.hot_args, func.hot.args.clone()),
            saved_pc: self.pc,
            saved_line_map: std::mem::replace(&mut self.active_line_map, func.line_map.clone()),
            saved_column_map: std::mem::replace(
                &mut self.active_column_map,
                func.column_map.clone(),
            ),
            func,
            pushed_func_stack: true,
            pushed_name_frame: false,
            saved_lw_depth,
            saved_lw_base,
        });
        let base = self.lw_sp;
        self.push_lw_base(base);
        self.lw_base = base;
        self.lw_depth += 1;
        self.lw_entry_pc = entry_pc;
        self.lw_frame_slots = frame_slots;
        let need = base + nslots;
        if need > self.lw_slots.len() {
            self.lw_slots.resize(need, StackVal::Empty);
        }
        for i in (0..argc).rev() {
            self.lw_slots[base + i] = self.pop_hot();
        }
        for i in argc..nslots {
            self.lw_slots[base + i] = StackVal::Empty;
        }
        self.lw_sp = need;
        self.pc = entry_pc;
        Ok(())
    }

    fn restore_user_call_frame(&mut self, frame: UserCallFrame) {
        self.code = frame.saved_code;
        self.hot_ops = frame.saved_hot_ops;
        self.hot_args = frame.saved_hot_args;
        self.pc = frame.saved_pc;
        self.active_line_map = frame.saved_line_map;
        self.active_column_map = frame.saved_column_map;
        self.lw_depth = frame.saved_lw_depth;
        self.lw_base = frame.saved_lw_base;
    }

    fn complete_user_return_instruction(
        &mut self,
        leave_scope: bool,
        result: Value,
    ) -> Result<Option<Value>> {
        // 防御：清掉当前调用帧内未走 EndTry 的 try 帧（codegen 应已清理 with；此处防泄漏）。
        let depth = self.user_call_frames.len();
        while self
            .try_stack
            .last()
            .is_some_and(|f| f.user_call_depth >= depth)
        {
            self.try_stack.pop();
        }

        if leave_scope
            && self
                .user_call_frames
                .last()
                .is_some_and(|f| f.pushed_name_frame)
        {
            self.leave_scope();
        }

        if self.user_call_frames.is_empty() {
            self.flush_script_fast_locals();
            self.push_value(result);
            self.pc = self.code.len();
            return Ok(Some(self.stack_top()));
        }

        let frame = self
            .user_call_frames
            .pop()
            .expect("user_call_frames non-empty on return (theoretically unreachable)");
        let func = frame.func.clone();
        if func.return_strong() {
            if let Some(ref ty) = func.return_type_value {
                if let Some(detail) = types::type_check_error(self, &result, ty) {
                    let msg = format!("return: {detail}");
                    if frame.pushed_name_frame {
                        self.leave_scope();
                    }
                    if frame.func.track_frames() {
                        self.func_frames.pop();
                    }
                    if frame.pushed_func_stack {
                        self.func_stack.pop();
                    }
                    self.restore_user_call_frame(frame);
                    let exc = exceptions::make_exception(self, "TypeError", msg)?;
                    self.throw_value(exc)?;
                    return Ok(None);
                }
                types::seal_container_contract(self, &result, ty);
            }
        }

        if frame.pushed_name_frame {
            self.leave_scope();
        }
        if frame.func.track_frames() {
            self.func_frames.pop();
        }
        if frame.pushed_func_stack {
            self.func_stack.pop();
        }
        self.restore_user_call_frame(frame);
        self.push_value(result);
        Ok(None)
    }

    fn unwind_user_calls_on_error(&mut self) -> Result<()> {
        // 任务失败：保留纤程状态，由 scheduler 的 capture_fiber 按 stop_* 裁剪；
        // 若此处 restore 任务帧会先把 code/pc 搅成宿主再 capture，状态损坏。
        if self.task_ctx.is_some() {
            return Ok(());
        }
        while self.fast_ret_sp > 0 {
            self.fast_ret_sp -= 1;
            self.pop_lightweight_frame();
        }
        while let Some(frame) = self.user_call_frames.pop() {
            if frame.pushed_name_frame {
                self.leave_scope();
            } else if self.lw_depth > frame.saved_lw_depth {
                self.pop_lightweight_frame();
            }
            if frame.func.track_frames() {
                self.func_frames.pop();
            }
            if frame.pushed_func_stack {
                self.func_stack.pop();
            }
            self.restore_user_call_frame(frame);
        }
        Ok(())
    }

    /// 在 unwind 前快照调用栈，供诊断展示。
    fn record_error_stack(&mut self) {
        let mut frames = Vec::new();
        for (i, ucf) in self.user_call_frames.iter().enumerate() {
            let (func, file, source) = if i == 0 {
                (
                    "<module>".to_string(),
                    self.source_file.clone(),
                    self.current_source.clone(),
                )
            } else {
                let caller = &self.user_call_frames[i - 1].func;
                (
                    caller.name.clone(),
                    caller.source_file.clone(),
                    caller.source.clone(),
                )
            };
            frames.push(ErrorStackFrame {
                func,
                file,
                line: Self::line_from_map(&ucf.saved_line_map, ucf.saved_pc),
                column: Self::line_from_map(&ucf.saved_column_map, ucf.saved_pc).max(1),
                source,
            });
        }

        let (func, file, source) = if let Some(f) = self.func_stack.last() {
            (f.name.clone(), f.source_file.clone(), f.source.clone())
        } else {
            (
                "<module>".to_string(),
                self.source_file.clone(),
                self.current_source.clone(),
            )
        };
        frames.push(ErrorStackFrame {
            func,
            file,
            line: self.current_line(),
            column: self.current_column().max(1),
            source,
        });
        self.last_error_stack = frames;
    }

    pub fn take_error_stack(&mut self) -> Vec<ErrorStackFrame> {
        std::mem::take(&mut self.last_error_stack)
    }

    fn finish_uncaught(&mut self, e: RuntimeError) -> Result<InterpResult> {
        let finalized = self.finalize_runtime_error(e);
        if let Some(dbg) = &self.debug {
            if self.task_ctx.is_none() {
                let mut st = dbg.borrow_mut();
                if st.exception_uncaught {
                    st.last_uncaught = Some(finalized.uncaught_line());
                    st.request_break(crate::debug::StopReason::Uncaught);
                    crate::debug::mark_stopped(self, &mut st);
                    return Ok(InterpResult::DebugBreak);
                }
            }
        }
        Err(finalized)
    }

    pub fn debug_store_local(&mut self, name: &str, value: Value) -> bool {
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(&slot) = map.get(name) {
                    if let Some(locals) = self.locals_stack.get_mut(i) {
                        if slot < locals.len() {
                            match &locals[slot] {
                                Value::Cell(c) => {
                                    *c.borrow_mut() = value;
                                }
                                _ => locals[slot] = value,
                            }
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn debug_list_globals(&self) -> Vec<(String, Value)> {
        let unwrap = |v: &Value| match v {
            Value::Cell(c) => c.borrow().clone(),
            other => other.clone(),
        };
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        for (idx, name) in self.script_global_names.iter().enumerate() {
            if name.is_empty() {
                continue;
            }
            if let Some(v) = self.script_globals.get(idx) {
                // `del` 清掉 map 键并把槽写成 none：调试器不应再列出该名。
                if matches!(v, Value::None) && !self.globals.contains_key(name.as_str()) {
                    continue;
                }
                seen.insert(name.clone());
                out.push((name.clone(), unwrap(v)));
            }
        }
        for &(local, global) in &self.script_local_to_global {
            let Some(name) = self.script_global_names.get(global) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            if let Some(sv) = self.script_fast_slot(local) {
                let val = sv.copy_imm().into_value();
                if let Some(slot) = out.iter_mut().find(|(n, _)| n == name) {
                    slot.1 = val;
                } else {
                    seen.insert(name.clone());
                    out.push((name.clone(), val));
                }
            }
        }
        let mut rest = self.globals.keys();
        rest.retain(|k| !seen.contains(k));
        rest.sort();
        for k in rest {
            if let Some(v) = self.globals.get(&k) {
                out.push((k, unwrap(&v)));
            }
        }
        out
    }

    pub fn debug_list_locals(&self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        // 1) 当前函数的快局部（含形参）——从 BindFast 指令与 params 重建名字→槽
        if let Some(func) = self.func_stack.last() {
            let mut map: FxHashMap<String, usize> = FxHashMap::default();
            for (i, p) in func.params.iter().enumerate() {
                map.entry(p.name.clone()).or_insert(i);
            }
            for ins in func.body.iter() {
                if let crate::opcode::Instruction::BindFast { slot, name, .. } = ins {
                    map.entry(name.clone()).or_insert(*slot);
                }
            }
            if let Some(locals) = self.locals_stack.last() {
                let mut pairs: Vec<_> = map.iter().collect();
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                for (name, &slot) in pairs {
                    if let Some(v) = locals.get(slot) {
                        let val = match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        };
                        out.push((name.clone(), val));
                    }
                }
            }
        }
        // 2) 仍可见的 name-mapped 词法作用域
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                let mut pairs: Vec<_> = map.iter().collect();
                pairs.sort_by(|a, b| a.0.cmp(b.0));
                for (name, &slot) in pairs {
                    if let Some(locals) = self.locals_stack.get(i) {
                        if let Some(v) = locals.get(slot) {
                            let val = match v {
                                Value::Cell(c) => c.borrow().clone(),
                                other => other.clone(),
                            };
                            if !out.iter().any(|(n, _)| n == name) {
                                out.push((name.clone(), val));
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// 当前函数可见的局部名（快局部 + name-mapped 作用域），供 eval 绑定注入用。
    pub(crate) fn debug_visible_local_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let mut seen: FxHashSet<String> = FxHashSet::default();
        if let Some(func) = self.func_stack.last() {
            for p in &func.params {
                if seen.insert(p.name.clone()) {
                    names.push(p.name.clone());
                }
            }
            for ins in func.body.iter() {
                if let crate::opcode::Instruction::BindFast { name, .. } = ins {
                    if seen.insert(name.clone()) {
                        names.push(name.clone());
                    }
                }
            }
        }
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                for name in map.keys() {
                    if seen.insert(name.clone()) {
                        names.push(name.clone());
                    }
                }
            }
        }
        names
    }

    /// 调试器求值用：按名取值，快局部 → 词法作用域 → 全局。
    pub(crate) fn debug_load_name(&self, name: &str) -> Result<Value> {
        if let Some(func) = self.func_stack.last() {
            let mut slot: Option<usize> = None;
            for (i, p) in func.params.iter().enumerate() {
                if p.name == name {
                    slot = Some(i);
                    break;
                }
            }
            if slot.is_none() {
                for ins in func.body.iter() {
                    if let crate::opcode::Instruction::BindFast {
                        slot: s, name: n, ..
                    } = ins
                    {
                        if n == name {
                            slot = Some(*s);
                            break;
                        }
                    }
                }
            }
            if let Some(s) = slot {
                if let Some(locals) = self.locals_stack.last() {
                    if let Some(v) = locals.get(s) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
            }
        }
        self.load_name(name)
    }

    /// 求值表达式（类型注解等）：走与字节码相同的 `load_name` / getattr / Index / Call。
    pub fn eval_expr(&mut self, expr: &crate::ast::Expr) -> Result<Value> {
        use crate::ast::ExprKind;
        match &expr.kind {
            ExprKind::Var(name) => {
                if let Some(env) = &self.annotation_bind_env {
                    if let Some(v) = env.globals.borrow().get(name.as_str()) {
                        return Ok(match v {
                            Value::Cell(c) => c.borrow().clone(),
                            other => other.clone(),
                        });
                    }
                }
                if let Some(v) = self.load_script_global_by_name(name) {
                    return Ok(v);
                }
                self.load_name(name)
            }
            ExprKind::Member { object, field } => {
                let obj = self.eval_expr(object)?;
                get_attr(self, &obj, field)
            }
            ExprKind::Index { object, index } => {
                let obj = self.eval_expr(object)?;
                let idx = self.eval_expr(index)?;
                index_value(self, &obj, &idx)
            }
            ExprKind::Call { callee, args } => {
                let f = self.eval_expr(callee)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    if a.is_splat || a.is_kwsplat || a.name.is_some() {
                        return Err(RuntimeError::msg(
                            "type annotation call does not support named/splat args",
                        ));
                    }
                    argv.push(self.eval_expr(&a.value)?);
                }
                self.call_value(f, argv)
            }
            ExprKind::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.eval_expr(it)?);
                }
                let v = Value::List(Shared::new(out));
                self.track_value(&v);
                Ok(v)
            }
            ExprKind::DoFunc {
                params,
                return_type,
                return_strong,
                return_wrapper,
                body,
            } => {
                // 经正式 codegen 编成函数对象（与表达式位 `do` 一致），不替换当前脚本全局。
                let mut gen = crate::codegen::Generator::new();
                let name = format!("<annot-do@{}:{}>", expr.loc.line, expr.loc.column);
                let func = gen.compile_annot_do(
                    &name,
                    params,
                    return_type.as_deref(),
                    *return_strong,
                    return_wrapper.as_deref(),
                    body,
                )?;
                Ok(Value::Function(func))
            }
            ExprKind::None => Ok(Value::None),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Number(n) => Ok(Value::Num(Num::from_literal(n)?)),
            ExprKind::String(s) => Ok(Value::Text(s.clone())),
            other => Err(RuntimeError::type_err(format!(
                "unsupported expression in type annotation: {other:?}"
            ))),
        }
    }

    /// 调试器求值：把 `bindings` 作为词法局部注入，跑 snippet，再恢复。
    pub(crate) fn eval_debug_snippet(
        &mut self,
        bindings: &[String],
        program: crate::opcode::CompiledProgram,
        instruction_budget: usize,
    ) -> Result<Value> {
        let saved = self.snapshot_for_eval();
        let dbg = self.debug.take();
        let saved_eval_budget = self.debug_eval_budget.replace(instruction_budget.max(1));

        // 取每个绑定名的当前值（必须在隔离调用栈之前，否则 func_stack 为空）。
        let values: Vec<Value> = bindings
            .iter()
            .map(|n| self.debug_load_name(n).unwrap_or(Value::None))
            .collect();

        // 隔离暂停函数的调用栈：snippet 的 Ret 不能弹掉原程序的 user_call_frames / func_stack。
        let saved_user_frames = std::mem::take(&mut self.user_call_frames);
        let saved_func_stack = std::mem::take(&mut self.func_stack);
        let saved_func_frames = std::mem::take(&mut self.func_frames);
        let saved_try = std::mem::take(&mut self.try_stack);
        let saved_iters = std::mem::take(&mut self.iterators);
        let saved_call_deferred = self.user_call_deferred;
        self.user_call_deferred = false;

        let result = (|| -> Result<Value> {
            self.functions.extend(program.functions.clone());
            self.macros.extend(program.macros.clone());
            self.struct_defs.extend(program.struct_defs.clone());
            self.enum_defs.extend(program.enum_defs.clone());
            self.variant_defs.extend(program.variant_defs.clone());
            // 顶层热 Store 可能只写 script_globals；先刷到 SharedMap，再按 snippet
            // 的 global_names 重建槽，否则条件/日志断点求值会读到 none。
            self.flush_script_globals_to_map();
            self.script_frame_slots = program.script_frame_slots;
            self.script_local_to_global = program.script_local_to_global.clone();
            self.init_script_globals(program.global_names.clone());
            self.prepare_script_fast_frame();
            self.code = Arc::new(program.code);
            self.hot_ops = program.hot.ops.clone();
            self.hot_args = program.hot.args.clone();
            self.active_line_map = Arc::new(program.line_map.clone());
            self.active_column_map = Arc::new(program.column_map.clone());
            self.pc = 0;
            self.op_clear();

            // 推入绑定作用域，让 snippet 能按名引用这些局部。
            self.enter_scope();
            let frame = self.locals_stack.len() - 1;
            let locals = self.locals_stack.last_mut().unwrap();
            locals.resize(bindings.len(), Value::None);
            for (i, v) in values.iter().enumerate() {
                locals[i] = v.clone();
            }
            if !bindings.is_empty() {
                let map = self.scope_name_map_mut(frame);
                for (i, name) in bindings.iter().enumerate() {
                    map.insert(name.clone(), i);
                }
            }

            let run_res = self.run_interpreter(None);
            let top = self.stack_top();
            self.leave_scope();
            run_res?;
            Ok(top)
        })();

        self.restore_eval_snapshot(saved);
        // 恢复隔离的调用栈
        self.user_call_frames = saved_user_frames;
        self.func_stack = saved_func_stack;
        self.func_frames = saved_func_frames;
        self.try_stack = saved_try;
        self.iterators = saved_iters;
        self.user_call_deferred = saved_call_deferred;
        self.debug = dbg;
        self.debug_active = self.debug.is_some();
        self.debug_eval_budget = saved_eval_budget;
        result
    }

    /// 调试器 attach 时调用：强制热循环退出，使下一次进入重新评估 `debug_active`。
    /// 主纤程用 `pending_main_yield`（由 `HotFlow::Cont` 消费），任务内用 `pending_suspend`。
    pub(crate) const fn force_debug_recheck(&mut self) {
        if self.task_ctx.is_some() {
            self.pending_suspend = true;
        } else {
            self.pending_main_yield = true;
        }
    }

    /// 无偏 `0..bound`（bound > 0）；xorshift* + Lemire。
    fn select_rng_below(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        let mut x = self.select_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if x == 0 {
            x = 0xA5A5_A5A5_A5A5_A5A5;
        }
        self.select_rng = x;
        let bound = bound as u64;
        let mut m = u128::from(x) * u128::from(bound);
        let mut lo = m as u64;
        if lo < bound {
            let thresh = bound.wrapping_neg() % bound;
            while lo < thresh {
                x = self.select_rng;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.select_rng = x;
                m = u128::from(x) * u128::from(bound);
                lo = m as u64;
            }
        }
        (m >> 64) as usize
    }

    pub(crate) fn dispatch_overload(
        &mut self,
        overloads: &[Arc<FunctionObject>],
        args: &[Value],
    ) -> Result<Value> {
        let mut best: Option<(usize, Arc<FunctionObject>)> = None;
        for func in overloads {
            let func = self.ensure_func_types_resolved(func.clone())?;
            if let Some(score) = types::dispatch_match_score(self, &func, args) {
                if best.as_ref().is_none_or(|(s, _)| score < *s) {
                    best = Some((score, func));
                }
            }
        }
        if let Some((_, func)) = best {
            return self.call_user_function(func, args.to_vec());
        }
        Err(RuntimeError::msg("no matching overload"))
    }

    pub(crate) fn advance_iterator(
        &mut self,
        state: &Shared<IteratorState>,
    ) -> Result<Option<Value>> {
        if matches!(&state.borrow().kind, IteratorKind::Generator { .. }) {
            return self.resume_generator(state);
        }
        loop {
            match &mut state.borrow_mut().kind {
                IteratorKind::Range {
                    current,
                    stop,
                    step,
                } => {
                    if (*step > 0 && *current >= *stop) || (*step < 0 && *current <= *stop) {
                        return Ok(None);
                    }
                    let val = Value::Num(Num::Small(*current));
                    *current += *step;
                    return Ok(Some(val));
                }
                IteratorKind::List { items, index } => {
                    if *index >= items.len() {
                        return Ok(None);
                    }
                    let val = items[*index].clone();
                    *index += 1;
                    return Ok(Some(val));
                }
                IteratorKind::Zip { children } => {
                    let mut out = Vec::new();
                    for child in children.iter() {
                        match self.advance_iterator(child)? {
                            Some(v) => out.push(v),
                            None => return Ok(None),
                        }
                    }
                    return Ok(Some(Value::List(Shared::new(out))));
                }
                IteratorKind::Map { func, source } => {
                    let func = func.clone();
                    let source = source.clone();
                    match self.advance_iterator(&source)? {
                        Some(item) => return self.call_user_function(func, vec![item]).map(Some),
                        None => return Ok(None),
                    }
                }
                IteratorKind::Filter { func, source } => {
                    let func = func.clone();
                    let source = source.clone();
                    match self.advance_iterator(&source)? {
                        Some(item) => {
                            if self
                                .call_user_function(func, vec![item.clone()])?
                                .is_truthy()
                            {
                                return Ok(Some(item));
                            }
                        }
                        None => return Ok(None),
                    }
                }
                IteratorKind::GenExpr {
                    source,
                    arity,
                    elem,
                    guards,
                } => {
                    let source = source.clone();
                    let elem = elem.clone();
                    let guards = guards.clone();
                    let arity = *arity;
                    match self.advance_iterator(&source)? {
                        Some(item) => {
                            let args = unpack_genexpr_args_vm(item, arity)?;
                            let mut keep = true;
                            for g in &guards {
                                if !self
                                    .call_user_function(g.clone(), args.clone())?
                                    .is_truthy()
                                {
                                    keep = false;
                                    break;
                                }
                            }
                            if keep {
                                return Ok(Some(self.call_user_function(elem, args)?));
                            }
                        }
                        None => return Ok(None),
                    }
                }
                IteratorKind::Repeat { value, remaining } => {
                    let value = value.clone();
                    match remaining {
                        None => return Ok(Some(value)),
                        Some(0) => return Ok(None),
                        Some(n) => {
                            *n -= 1;
                            return Ok(Some(value));
                        }
                    }
                }
                IteratorKind::Cycle { items, index } => {
                    if items.is_empty() {
                        return Ok(None);
                    }
                    let i = *index;
                    *index = (i + 1) % items.len();
                    return Ok(Some(items[i].clone()));
                }
                IteratorKind::Channel { channel } => {
                    let ch = channel.clone();
                    let v = self.channel_recv(&ch)?;
                    // 关闭后 recv 返回 none → 迭代结束（与 docs 一致；无法与合法 none 载荷区分）。
                    if matches!(v, Value::None) && ch.borrow().closed {
                        return Ok(None);
                    }
                    return Ok(Some(v));
                }
                IteratorKind::Take { remaining, source } => {
                    if *remaining == 0 {
                        return Ok(None);
                    }
                    let source = source.clone();
                    match self.advance_iterator(&source)? {
                        Some(v) => {
                            *remaining -= 1;
                            return Ok(Some(v));
                        }
                        None => return Ok(None),
                    }
                }
                IteratorKind::Skip { remaining, source } => {
                    let source = source.clone();
                    while *remaining > 0 {
                        match self.advance_iterator(&source)? {
                            Some(_) => *remaining -= 1,
                            None => return Ok(None),
                        }
                    }
                    return self.advance_iterator(&source);
                }
                IteratorKind::Enumerate { index, source } => {
                    let source = source.clone();
                    match self.advance_iterator(&source)? {
                        Some(item) => {
                            let i = *index;
                            *index = index.saturating_add(1);
                            return Ok(Some(Value::List(Shared::new(vec![
                                Value::Num(Num::Small(i as i64)),
                                item,
                            ]))));
                        }
                        None => return Ok(None),
                    }
                }
                IteratorKind::Chain { sources, current } => {
                    while *current < sources.len() {
                        let src = sources[*current].clone();
                        match self.advance_iterator(&src)? {
                            Some(v) => return Ok(Some(v)),
                            None => *current += 1,
                        }
                    }
                    return Ok(None);
                }
                IteratorKind::User { obj } => {
                    let obj = obj.clone();
                    match self.try_call_magic(&obj, "__next__", vec![]) {
                        Some(Ok(v)) => return Ok(Some(v)),
                        Some(Err(e)) if e.kind() == crate::error::ExceptionKind::StopIteration => {
                            self.active_exception = None;
                            return Ok(None);
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            return Err(RuntimeError::type_err(
                                "iterator protocol requires __next__",
                            ));
                        }
                    }
                }
                IteratorKind::Generator { .. } => {
                    unreachable!("generators handled before advance_iterator loop");
                }
            }
        }
    }

    fn make_generator_iterator(
        &mut self,
        func: Arc<FunctionObject>,
        args: Vec<Value>,
    ) -> Result<Value> {
        let func = self.ensure_func_types_resolved(func)?;
        let args = self.apply_implicit_param_converts(&func, args)?;
        if self.active_exception.is_some() {
            return Ok(Value::None);
        }
        self.check_strong_params(&func, &args)?;
        if self.active_exception.is_some() {
            return Ok(Value::None);
        }

        let captured_len = func
            .captured
            .as_ref()
            .map_or(0, std::collections::HashMap::len);
        let frame_size = func.frame_slots.max(func.params.len() + captured_len);
        let mut locals = vec![Value::None; frame_size];
        let mut name_map = if func.uses_name_map() {
            Some(FxHashMap::default())
        } else {
            None
        };

        let mut slot = func.params.len();
        if let Some(captured) = &func.captured {
            let mut caps: Vec<_> = captured.iter().collect();
            caps.sort_by(|a, b| a.0.cmp(b.0));
            for (name, val) in caps {
                if func.params.iter().any(|p| p.name == *name) {
                    continue;
                }
                if slot < locals.len() {
                    locals[slot] = val.clone();
                }
                if let Some(names) = name_map.as_mut() {
                    names.insert(name.clone(), slot);
                }
                slot += 1;
            }
        }
        for (i, val) in args.into_iter().enumerate() {
            if i < locals.len() {
                locals[i] = val;
            }
            if let (Some(names), Some(param)) = (name_map.as_mut(), func.params.get(i)) {
                names.insert(param.name.clone(), i);
            }
        }
        if let Some(names) = name_map.as_mut() {
            for (i, param) in func.params.iter().enumerate() {
                names.entry(param.name.clone()).or_insert(i);
            }
        }

        Ok(IteratorState {
            kind: IteratorKind::Generator {
                func,
                locals,
                name_map,
                pc: 0,
                exhausted: false,
                yield_from: None,
            },
        }
        .into_value())
    }

    fn resume_generator(&mut self, state: &Shared<IteratorState>) -> Result<Option<Value>> {
        // 消费者取消时，生成器在下一 yield/恢复点协作退出。
        self.fail_if_current_task_cancelled()?;
        loop {
            let (func, locals, name_map, pc, exhausted, yield_from) = {
                let st = state.borrow();
                let IteratorKind::Generator {
                    func,
                    locals,
                    name_map,
                    pc,
                    exhausted,
                    yield_from,
                } = &st.kind
                else {
                    return Err(RuntimeError::msg(
                        "internal: resume_generator on non-generator",
                    ));
                };
                (
                    func.clone(),
                    locals.clone(),
                    name_map.clone(),
                    *pc,
                    *exhausted,
                    yield_from.clone(),
                )
            };
            if exhausted {
                return Ok(None);
            }
            if let Some(yf) = yield_from {
                if let Some(v) = self.advance_iterator(&yf)? {
                    return Ok(Some(v));
                }
                if let IteratorKind::Generator { yield_from, .. } = &mut state.borrow_mut().kind {
                    *yield_from = None;
                }
                continue;
            }

            let stack_base = self.stack_sp;
            let stop_depth = self.user_call_frames.len();
            self.active_generator = Some(state.clone());
            self.generator_resuming = true;

            if func.track_frames() {
                let call_line = self.current_line();
                self.func_frames.push(FuncFrame {
                    name: func.name.clone(),
                    file: self.source_file.clone(),
                    line: call_line,
                });
            }
            self.func_stack.push(func.clone());
            self.name_to_slot.push(name_map);
            self.locals_stack.push(locals);
            let saved_lw_depth = self.lw_depth;
            let saved_lw_base = self.lw_base;
            self.lw_depth = 0;
            self.user_call_frames.push(UserCallFrame {
                saved_code: self.code.clone(),
                saved_hot_ops: self.hot_ops.clone(),
                saved_hot_args: self.hot_args.clone(),
                saved_pc: self.pc,
                saved_line_map: self.active_line_map.clone(),
                saved_column_map: self.active_column_map.clone(),
                func: func.clone(),
                pushed_func_stack: true,
                pushed_name_frame: true,
                saved_lw_depth,
                saved_lw_base,
            });
            self.code = func.body.clone();
            validate_function_hot(&func)?;
            self.hot_ops = func.hot.ops.clone();
            self.hot_args = func.hot.args.clone();
            self.active_line_map = func.line_map.clone();
            self.active_column_map = func.column_map.clone();
            self.pc = pc;

            let result = self.run_interpreter(Some(stop_depth));
            self.generator_resuming = false;
            self.active_generator = None;

            match result? {
                InterpResult::Yielded(v) => {
                    self.op_truncate(stack_base);
                    return Ok(Some(v));
                }
                InterpResult::Value(_) => {
                    if let IteratorKind::Generator { exhausted, .. } = &mut state.borrow_mut().kind
                    {
                        *exhausted = true;
                    }
                    self.op_truncate(stack_base);
                    return Ok(None);
                }
                InterpResult::Suspended => {
                    return Err(RuntimeError::msg(
                        "internal error: generator suspended via task scheduler",
                    ));
                }
                InterpResult::DebugBreak => {
                    return Err(RuntimeError::msg(
                        "internal error: debug break inside generator",
                    ));
                }
            }
        }
    }

    fn generator_yield_value(&mut self, v: Value) -> Result<()> {
        if !self.generator_resuming {
            return Err(RuntimeError::msg(
                "`yield` is only valid inside a generator function or do",
            ));
        }
        self.capture_generator_pause()?;
        self.pending_gen_yield = Some(v);
        Ok(())
    }

    fn generator_yield_from(&mut self, iterable: Value) -> Result<()> {
        if !self.generator_resuming {
            return Err(RuntimeError::msg(
                "`yield from` is only valid inside a generator function or do",
            ));
        }
        let iter = self.to_iterator_shared(&iterable)?;
        // 内层生成器的 resume 会改写 active_generator / generator_resuming，需保存外层。
        let saved_active = self.active_generator.clone();
        let saved_resuming = self.generator_resuming;
        if let Some(v) = self.advance_iterator(&iter)? {
            self.active_generator = saved_active.clone();
            self.generator_resuming = saved_resuming;
            if let Some(state) = &saved_active {
                if let IteratorKind::Generator { yield_from, .. } = &mut state.borrow_mut().kind {
                    *yield_from = Some(iter);
                }
            }
            self.capture_generator_pause()?;
            self.pending_gen_yield = Some(v);
        } else {
            self.active_generator = saved_active;
            self.generator_resuming = saved_resuming;
            // 空可迭代：继续执行下一条指令。
        }
        Ok(())
    }

    fn capture_generator_pause(&mut self) -> Result<()> {
        let Some(state) = self.active_generator.clone() else {
            return Err(RuntimeError::msg(
                "internal: yield without active generator",
            ));
        };
        let locals = self
            .locals_stack
            .last()
            .cloned()
            .ok_or_else(|| RuntimeError::msg("internal: generator missing locals"))?;
        let name_map = self.name_to_slot.last().cloned().flatten();
        let pc = self.pc;
        if let IteratorKind::Generator {
            locals: l,
            name_map: nm,
            pc: p,
            ..
        } = &mut state.borrow_mut().kind
        {
            *l = locals;
            *nm = name_map;
            *p = pc;
        }

        let frame = self
            .user_call_frames
            .pop()
            .ok_or_else(|| RuntimeError::msg("internal: generator missing call frame"))?;
        self.locals_stack.pop();
        self.name_to_slot.pop();
        if frame.pushed_func_stack {
            self.func_stack.pop();
        }
        if frame.func.track_frames() {
            self.func_frames.pop();
        }
        self.restore_user_call_frame(frame);
        Ok(())
    }

    pub(crate) fn spawn_task(&mut self, callable: Value, args: Vec<Value>) -> Value {
        // go 任务可能经 SharedMap / module_env 读顶层绑定；刷平行槽。
        self.flush_script_globals_to_map();
        self.publish_script_globals();
        let task = Shared::new(TaskInner::pending(callable, args));
        self.enqueue_new_task(task.clone());
        Value::Task(task)
    }

    /// 新 `go` / TaskGroup：M:N 进全局 injector，主线程与 helper 公平取完整任务。
    fn enqueue_new_task(&mut self, task: Shared<TaskInner>) {
        if self.mn_parallel {
            self.mn.push_task(task);
        } else {
            self.ready_tasks.push_back(task);
        }
    }

    /// 时间片挂起后重入队：留在本 worker local，保持亲和、避免无谓迁移。
    fn enqueue_task(&mut self, task: Shared<TaskInner>) {
        if self.mn_parallel {
            // M:N：injector 不可遍历，靠弱表给 GC 补根；挂起重入队是热路径。
            self.mn.note_scheduled_task(&task);
            self.local_worker.push(task);
            self.mn.notify_one();
        } else {
            // M:1：就绪队列 + task_fibers 已是完整根，避免每次挂起抢 scheduled_tasks 锁。
            self.ready_tasks.push_back(task);
        }
    }

    fn take_ready_task(&mut self) -> Option<Shared<TaskInner>> {
        if self.mn_parallel {
            let task = self.mn.steal_task(&self.local_worker);
            if task.is_some() {
                // 与 scheduler_run_task 末尾的 note_task_done 配对（死锁检测）。
                self.mn.note_task_taken();
            }
            task
        } else {
            self.ready_tasks.pop_front()
        }
    }

    fn fiber_insert(&mut self, key: usize, fiber: TaskFiber) {
        if self.mn_parallel {
            self.mn.fibers.lock().insert(key, fiber);
        } else {
            self.task_fibers.insert(key, fiber);
        }
    }

    fn fiber_take(&mut self, key: usize) -> Option<TaskFiber> {
        if self.mn_parallel {
            self.mn.fibers.lock().remove(&key)
        } else {
            self.task_fibers.remove(&key)
        }
    }

    /// 为 M:N 辅助线程复制一份可共享 globals/注册表的 Vm。
    fn fork_worker(&self) -> Self {
        let local_worker = scheduler::new_local_worker();
        self.mn.register_stealer(local_worker.stealer());
        Self {
            code: Arc::new(Vec::new()),
            hot_ops: Arc::from([]),
            hot_args: Arc::from([]),
            stack: Vec::with_capacity(STACK_INIT_CAP),
            stack_sp: 0,
            globals: self.globals.clone(),
            locals_stack: Vec::new(),
            name_to_slot: Vec::new(),
            func_stack: Vec::new(),
            func_frames: Vec::new(),
            pc: 0,
            active_line_map: Arc::new(Vec::new()),
            active_column_map: Arc::new(Vec::new()),
            struct_defs: self.struct_defs.clone(),
            enum_defs: self.enum_defs.clone(),
            variant_defs: self.variant_defs.clone(),
            functions: self.functions.clone(),
            macros: self.macros.clone(),
            try_stack: Vec::new(),
            active_exception: None,
            iterators: Vec::new(),
            const_names: self.const_names.clone(),
            pending_const: FxHashSet::default(),
            module_cache: self.module_cache.clone(),
            builtin_modules: self.builtin_modules.clone(),
            module_init_exports: None,
            macro_eval_scopes: Vec::new(),
            convert_tables: self.convert_tables.clone(),
            source_file: self.source_file.clone(),
            current_source: self.current_source.clone(),
            last_error_stack: Vec::new(),
            import_base: self.import_base.clone(),
            dep_map: self.dep_map.clone(),
            current_package_id: self.current_package_id.clone(),
            package_root: self.package_root.clone(),
            overload_tables: self.overload_tables.clone(),
            primitive_methods: self.primitive_methods.clone(),
            user_call_frames: Vec::new(),
            user_call_deferred: false,
            nested_user_call_suspended: false,
            script_global_names: self.script_global_names.clone(),
            script_globals: self.script_globals.clone(),
            script_frame_slots: self.script_frame_slots,
            script_local_to_global: self.script_local_to_global.clone(),
            local_fn_hot: Vec::new(),
            annotation_bind_env: None,
            local_frame_pool: Vec::new(),
            call_args_buf: Vec::with_capacity(CALL_ARGS_BUF_INIT_CAP),
            fast_ret_pcs: Vec::with_capacity(FAST_RET_PCS_INIT_CAP),
            fast_ret_sp: 0,
            lw_slots: Vec::with_capacity(LW_SLOTS_INIT_CAP),
            lw_bases: Vec::with_capacity(LW_BASES_INIT_CAP),
            lw_bases_sp: 0,
            lw_sp: 0,
            lw_base: 0,
            lw_depth: 0,
            lw_entry_pc: 0,
            lw_frame_slots: 0,
            cached_max_depth: self.cached_max_depth,
            pending_ret: None,
            hot_failed: false,
            hot_error: None,
            gc: self.gc.clone(),
            gc_threshold: self.gc_threshold,
            list_element_contracts: self.list_element_contracts.clone(),
            dict_contracts: self.dict_contracts.clone(),
            set_element_contracts: self.set_element_contracts.clone(),
            protocols: self.protocols.clone(),
            ready_tasks: VecDeque::new(),
            task_fibers: FxHashMap::default(),
            mn: self.mn.clone(),
            local_worker,
            mn_parallel: true,
            mn_primary: false,
            mn_idle_rounds: 0,
            select_fair_order: Vec::new(),
            select_fair_pos: 0,
            select_rng: 0x00C0_FFEE_u64 ^ std::time::Instant::now().elapsed().as_nanos() as u64,
            debug_break_requested: false,
            debug_paused_tasks: Vec::new(),
            gc_auto_cooldown_until: None,
            gc_auto_cooldown_hold_count: 0,
            sched_depth: 0,
            block_suspend: false,
            call_retry_armed: false,
            ffi_cfg: self.ffi_cfg.clone(),
            ffi_wait: None,
            pending_poll_retry: None,
            sync_wait_resume: None,
            task_ctx: None,
            suspend_budget: clamp_suspend_budget(self.suspend_budget),
            budget_left: clamp_suspend_budget(self.suspend_budget),
            pending_suspend: false,
            pending_main_yield: false,
            script_globals_map_dirty: false,
            generator_resuming: false,
            active_generator: None,
            pending_gen_yield: None,
            debug: self.debug.clone(),
            debug_active: self.debug_active,
            cover: self.cover.clone(),
            cover_active: self.cover_active,
            test_case_log: Vec::new(),
            test_tmp_dirs: Vec::new(),
            has_const_names: self.has_const_names,
            has_pending_const: self.has_pending_const,
            caps: self.caps.clone(),
            host_caps: self.host_caps.clone(),
            argv_override: self.argv_override.clone(),
            output_sink: self.output_sink.clone(),
            debug_eval_budget: None,
            host_cancel: self.host_cancel.clone(),
            metrics: self.metrics.clone(),
            metrics_active: self.metrics_active,
        }
    }

    /// 返回成功启动的 helper 数；单个失败仅告警，由调用方决定是否降级。
    fn spawn_helper_workers(&self) -> usize {
        let n = self.mn.worker_count();
        let mut started = 0usize;
        for i in 1..n {
            let mut worker_vm = self.fork_worker();
            let mn = self.mn.clone();
            match std::thread::Builder::new()
                .name(format!("optive-w{i}"))
                .spawn(move || {
                    crate::gc::install_current_gc(worker_vm.gc.clone());
                    while !mn.is_shutdown() {
                        worker_vm.poll_gc_safepoint();
                        if let Some(task) = worker_vm.take_ready_task() {
                            let _ = worker_vm.scheduler_run_task(task);
                        } else {
                            worker_vm.poll_gc_safepoint();
                            mn.wait_brief();
                        }
                    }
                    crate::gc::clear_current_gc();
                }) {
                Ok(_) => {
                    // 在父线程登记：避免 STW 在 helper 尚未执行 mark 前按 need=0 误判成功。
                    self.mn.mark_helper_started();
                    started += 1;
                }
                Err(e) => {
                    eprintln!("optive: failed to spawn helper worker {i}: {e}");
                }
            }
        }
        started
    }

    /// 阻塞等待时：
    /// - 已在任务纤程内（`sched_depth>0`）：**禁止再入** `scheduler_run_one`
    ///   （嵌套 capture 会破坏外层栈）；改为挂起本任务，让外层调度器继续；
    /// - 否则 wait-as-worker：**一次只跑一个**就绪任务再回到等待条件。
    ///   M:N 若在此把 injector 抽干，helper 还在 `wait_brief` 时主线程会把
    ///   全部 CPU `go` 串行做完（Criterion `par/2` 会在 ~1× 与 ~2× 两档间跳）。
    pub(crate) fn wait_or_deadlock(&mut self, msg: &str) -> Result<()> {
        self.poll_gc_safepoint();
        if self.debug_break_requested {
            return Ok(());
        }
        // 任务内阻塞：立即挂起并回绕 Call，由外层（主纤程/其它 worker）推进。
        if self.sched_depth > 0 && self.task_ctx.is_some() {
            self.block_suspend = true;
            return Ok(());
        }
        if self.scheduler_run_one()? || self.debug_break_requested {
            return Ok(());
        }
        if self.mn_parallel {
            // 其它 OS worker 可能正在跑 CPU 任务；不能仅凭「本线程无就绪」判死锁。
            // 静默判据：无任何线程在执行任务（busy==0）且各级队列均为空。
            // sleep / FFI / 定时等待的任务会持续在就绪队列自旋或处于执行中，
            // 故不会造成全局静默；连续静默若干轮（覆盖取任务瞬间的竞态窗口）
            // 后才报死锁。
            let quiescent = !self.mn.is_shutdown() && self.mn.busy() == 0 && self.mn.queues_empty();
            if quiescent {
                self.mn_idle_rounds += 1;
                if self.mn_idle_rounds >= MN_DEADLOCK_IDLE_ROUNDS {
                    self.mn_idle_rounds = 0;
                    let detail = format!(
                        "{msg} (workers={} busy={} queues_empty=true; cannot detect CPU livelock)",
                        self.mn.worker_count(),
                        self.mn.busy()
                    );
                    return Err(RuntimeError::deadlock(detail));
                }
            } else {
                self.mn_idle_rounds = 0;
            }
            self.poll_gc_safepoint();
            self.mn.wait_brief();
            return Ok(());
        }
        Err(RuntimeError::deadlock(msg))
    }

    /// 将 callee/args 压回栈并把 PC 拨回 Call，供挂起恢复后重试。
    fn arm_call_retry(&mut self, callee: Value, args: Vec<Value>) {
        for a in args {
            self.push_value(a);
        }
        // `Call` / `CallList` / `CallEx` 从栈取 callee；`CallGlobal` / `CallSelf` 自取目标。
        // 若误压 callee，重入 `CallGlobal` 时会把函数当成第一个实参（如 convert function to u32）。
        let call_pc = self.pc.saturating_sub(1);
        let push_callee = !matches!(
            self.code.get(call_pc),
            Some(Instruction::CallGlobal { .. } | Instruction::CallSelf { .. })
        );
        if push_callee {
            self.push_value(callee);
        }
        self.pc = call_pc;
        self.pending_suspend = true;
        self.call_retry_armed = true;
    }

    pub(crate) fn task_from_value(value: Value) -> Value {
        if matches!(value, Value::Task(_)) {
            return value;
        }
        Value::Task(Shared::new(TaskInner::done(value)))
    }

    #[inline(always)]
    fn tick_budget(&mut self) {
        // wrapping_sub: avoid per-insn overflow-check branches; refill when exhausted.
        // 热循环（H_GOTO）每次迭代都进这里：不要在预算未耗尽时读 STW 原子，
        // 否则 M:N 每条回跳都多一次跨核缓存行探测，串行路径没有这笔税。
        // STW 仍在预算耗尽时 poll（默认 8192 tick 内停车）。
        self.budget_left = self.budget_left.wrapping_sub(1);
        if self.budget_left != 0 {
            return;
        }
        self.budget_left = clamp_suspend_budget(self.suspend_budget);
        if self.metrics_active {
            crate::metrics::sample(self);
        }
        if self.mn_parallel {
            self.poll_gc_safepoint();
            if self.mn_primary && self.mn.gc_request_pending() {
                self.maybe_auto_gc();
            }
        }
        // 安全点与时间片解耦：M:N 下仅当本 worker 或 injector 仍有其它活时才挂起。
        if !self.should_timeslice_preempt() {
            return;
        }
        if self.task_ctx.is_some() {
            self.pending_suspend = true;
        } else if self.mn_parallel || !self.ready_tasks.is_empty() {
            self.pending_main_yield = true;
        }
    }

    /// M:1 保持协作切片；M:N 仅当本 worker local 还有其它就绪任务时让出。
    ///
    /// 不读 `Injector::is_empty()`：该方法允许把空队列报成非空，会让时间片永远开着。
    /// injector 里的活由空闲 worker 自己去偷，不必打断正在跑的独占 CPU 任务。
    #[inline]
    fn should_timeslice_preempt(&self) -> bool {
        if !self.mn_parallel {
            return true;
        }
        !self.local_worker.is_empty()
    }

    fn task_ptr_key(task: &Shared<TaskInner>) -> usize {
        task.as_ptr() as usize
    }

    fn snapshot_task_ctx(task: Shared<TaskInner>, vm: &Self) -> TaskRunCtx {
        TaskRunCtx {
            task,
            stop_ucf: vm.user_call_frames.len(),
            stop_locals: vm.locals_stack.len(),
            stop_nts: vm.name_to_slot.len(),
            stop_stack: vm.stack_sp,
            stop_func_stack: vm.func_stack.len(),
            stop_func_frames: vm.func_frames.len(),
            stop_try: vm.try_stack.len(),
            stop_iters: vm.iterators.len(),
            stop_fast_ret: vm.fast_ret_sp,
            stop_lw_bases: vm.lw_bases_sp,
            stop_lw_sp: vm.lw_sp,
            stop_lw_depth: vm.lw_depth,
            stop_lw_base: vm.lw_base,
            stop_lw_entry_pc: vm.lw_entry_pc,
            stop_lw_frame_slots: vm.lw_frame_slots,
            host_code: vm.code.clone(),
            host_hot_ops: vm.hot_ops.clone(),
            host_hot_args: vm.hot_args.clone(),
            host_pc: vm.pc,
            host_line_map: vm.active_line_map.clone(),
            host_column_map: vm.active_column_map.clone(),
        }
    }

    fn capture_fiber(&mut self, ctx: &TaskRunCtx) -> TaskFiber {
        let stack = if self.stack_sp > ctx.stop_stack {
            let mut s = Vec::with_capacity(self.stack_sp - ctx.stop_stack);
            for i in ctx.stop_stack..self.stack_sp {
                s.push(std::mem::replace(&mut self.stack[i], StackVal::Empty));
            }
            s
        } else {
            Vec::new()
        };
        // 防御：若错误路径已卸到 stop 以下，勿 panic（debug 仍断言）。
        fn split_at_or_empty<T>(v: &mut Vec<T>, stop: usize) -> Vec<T> {
            if v.len() >= stop {
                v.split_off(stop)
            } else {
                debug_assert!(false, "capture_fiber: len {} < stop {}", v.len(), stop);
                // release 下不变量被破坏会导致静默空帧；至少留可观测痕迹。
                eprintln!(
                    "optive internal: capture_fiber invariant broken (len {} < stop {})",
                    v.len(),
                    stop
                );
                Vec::new()
            }
        }
        let locals_stack = split_at_or_empty(&mut self.locals_stack, ctx.stop_locals);
        let name_to_slot = split_at_or_empty(&mut self.name_to_slot, ctx.stop_nts);
        let user_call_frames = split_at_or_empty(&mut self.user_call_frames, ctx.stop_ucf);
        let func_stack = split_at_or_empty(&mut self.func_stack, ctx.stop_func_stack);
        let func_frames = split_at_or_empty(&mut self.func_frames, ctx.stop_func_frames);
        let try_stack = {
            let mut ts = split_at_or_empty(&mut self.try_stack, ctx.stop_try);
            for f in &mut ts {
                f.user_call_depth = f.user_call_depth.saturating_sub(ctx.stop_ucf);
                f.stack_sp = f.stack_sp.saturating_sub(ctx.stop_stack);
                f.iterators_len = f.iterators_len.saturating_sub(ctx.stop_iters);
                f.fast_ret_sp = f.fast_ret_sp.saturating_sub(ctx.stop_fast_ret);
            }
            ts
        };
        let iterators = split_at_or_empty(&mut self.iterators, ctx.stop_iters);

        let fast_ret_pcs = if self.fast_ret_sp > ctx.stop_fast_ret {
            self.fast_ret_pcs[ctx.stop_fast_ret..self.fast_ret_sp].to_vec()
        } else {
            Vec::new()
        };
        self.fast_ret_sp = ctx.stop_fast_ret;

        let lw_bases = if self.lw_bases_sp > ctx.stop_lw_bases {
            self.lw_bases[ctx.stop_lw_bases..self.lw_bases_sp]
                .iter()
                .map(|b| b.saturating_sub(ctx.stop_lw_sp))
                .collect()
        } else {
            Vec::new()
        };
        self.lw_bases_sp = ctx.stop_lw_bases;

        let lw_slots = if self.lw_sp > ctx.stop_lw_sp {
            let mut slots = Vec::with_capacity(self.lw_sp - ctx.stop_lw_sp);
            for i in ctx.stop_lw_sp..self.lw_sp {
                slots.push(std::mem::replace(&mut self.lw_slots[i], StackVal::Empty));
            }
            slots
        } else {
            Vec::new()
        };

        let fiber = TaskFiber {
            code: self.code.clone(),
            hot_ops: self.hot_ops.clone(),
            hot_args: self.hot_args.clone(),
            pc: self.pc,
            active_line_map: self.active_line_map.clone(),
            active_column_map: self.active_column_map.clone(),
            stack,
            locals_stack,
            name_to_slot,
            user_call_frames,
            func_stack,
            func_frames,
            try_stack,
            iterators,
            fast_ret_pcs,
            lw_slots,
            lw_bases,
            lw_base: self.lw_base.saturating_sub(ctx.stop_lw_sp),
            lw_depth: self.lw_depth as isize - ctx.stop_lw_depth as isize,
            lw_entry_pc: self.lw_entry_pc,
            lw_frame_slots: self.lw_frame_slots,
            ffi_wait: self.ffi_wait.take(),
            retry_poll: self.pending_poll_retry.take(),
            sync_wait_resume: self.sync_wait_resume.take(),
        };

        self.stack_sp = ctx.stop_stack;
        self.lw_sp = ctx.stop_lw_sp;
        self.lw_depth = ctx.stop_lw_depth;
        self.lw_base = ctx.stop_lw_base;
        self.lw_entry_pc = ctx.stop_lw_entry_pc;
        self.lw_frame_slots = ctx.stop_lw_frame_slots;

        // 始终恢复本 worker 切入任务前的代码指针（见 TaskRunCtx 注释）。
        self.code = ctx.host_code.clone();
        self.hot_ops = ctx.host_hot_ops.clone();
        self.hot_args = ctx.host_hot_args.clone();
        self.pc = ctx.host_pc;
        self.active_line_map = ctx.host_line_map.clone();
        self.active_column_map = ctx.host_column_map.clone();

        fiber
    }

    fn install_fiber(&mut self, task: Shared<TaskInner>, mut fiber: TaskFiber) {
        let ctx = Self::snapshot_task_ctx(task, self);

        // 纤程可能从其它 worker 迁来：最底帧的 saved_* 仍是旧宿主（helper 上常为空）。
        // 改写为当前宿主，这样任务 return 时 restore_user_call_frame 不会清掉主模块代码。
        if let Some(frame) = fiber.user_call_frames.first_mut() {
            frame.saved_code = self.code.clone();
            frame.saved_hot_ops = self.hot_ops.clone();
            frame.saved_hot_args = self.hot_args.clone();
            frame.saved_pc = self.pc;
            frame.saved_line_map = self.active_line_map.clone();
            frame.saved_column_map = self.active_column_map.clone();
            // 与 saved_code 相同：底帧的 lw 水位属于当前宿主，不是旧 worker。
            frame.saved_lw_depth = self.lw_depth;
            frame.saved_lw_base = self.lw_base;
        }

        self.code = fiber.code;
        self.hot_ops = fiber.hot_ops;
        self.hot_args = fiber.hot_args;
        self.pc = fiber.pc;
        self.active_line_map = fiber.active_line_map;
        self.active_column_map = fiber.active_column_map;

        for sv in fiber.stack.drain(..) {
            self.op_push(sv);
        }
        self.locals_stack.append(&mut fiber.locals_stack);
        self.name_to_slot.append(&mut fiber.name_to_slot);
        self.user_call_frames.append(&mut fiber.user_call_frames);
        self.func_stack.append(&mut fiber.func_stack);
        self.func_frames.append(&mut fiber.func_frames);
        for mut f in fiber.try_stack.drain(..) {
            f.user_call_depth += ctx.stop_ucf;
            f.stack_sp += ctx.stop_stack;
            f.iterators_len += ctx.stop_iters;
            f.fast_ret_sp += ctx.stop_fast_ret;
            self.try_stack.push(f);
        }
        self.iterators.append(&mut fiber.iterators);

        for pc in fiber.fast_ret_pcs.drain(..) {
            if self.fast_ret_sp >= self.fast_ret_pcs.len() {
                self.fast_ret_pcs.push(pc);
            } else {
                self.fast_ret_pcs[self.fast_ret_sp] = pc;
            }
            self.fast_ret_sp += 1;
        }
        let lw_base_offset = self.lw_sp;
        for b in fiber.lw_bases.drain(..) {
            let abs = lw_base_offset + b;
            if self.lw_bases_sp >= self.lw_bases.len() {
                self.lw_bases.push(abs);
            } else {
                self.lw_bases[self.lw_bases_sp] = abs;
            }
            self.lw_bases_sp += 1;
        }
        for sv in fiber.lw_slots.drain(..) {
            if self.lw_sp >= self.lw_slots.len() {
                self.lw_slots.push(sv);
            } else {
                self.lw_slots[self.lw_sp] = sv;
            }
            self.lw_sp += 1;
        }
        self.lw_base = ctx.stop_lw_sp + fiber.lw_base;
        self.lw_depth = (ctx.stop_lw_depth as isize + fiber.lw_depth).max(0) as usize;
        // `go` 入口走 `setup_user_call`，执行中 `lw_depth` 被置 0，LoadFast 走 locals_stack。
        // helper 上 stop=0 时相对深度也是 0；迁到带脚本帧的宿主会变成 1，误读脚本槽。
        if self
            .user_call_frames
            .last()
            .is_some_and(|f| f.pushed_name_frame)
        {
            self.lw_depth = 0;
        }
        self.lw_entry_pc = fiber.lw_entry_pc;
        self.lw_frame_slots = fiber.lw_frame_slots;
        self.ffi_wait = fiber.ffi_wait.take();
        self.sync_wait_resume = fiber.sync_wait_resume.take();

        self.task_ctx = Some(ctx);
        self.budget_left = self.suspend_budget;
        self.pending_suspend = false;
    }

    fn complete_task_suspend(&mut self) -> Result<InterpResult> {
        self.pending_suspend = false;
        let Some(ctx) = self.task_ctx.take() else {
            return Err(RuntimeError::msg(
                "internal error: suspend without task context",
            ));
        };
        let key = Self::task_ptr_key(&ctx.task);
        let fiber = self.capture_fiber(&ctx);
        self.fiber_insert(key, fiber);
        ctx.task.borrow_mut().state = TaskState::Suspended;
        self.enqueue_task(ctx.task);
        Ok(InterpResult::Suspended)
    }

    pub(crate) fn scheduler_run_one(&mut self) -> Result<bool> {
        self.flush_script_globals_to_map();
        let Some(task) = self.take_ready_task() else {
            return Ok(false);
        };
        self.scheduler_run_task(task)
    }

    fn scheduler_run_task(&mut self, task: Shared<TaskInner>) -> Result<bool> {
        // 与 take_ready_task 的 note_task_taken 配对；所有出口统一 -1。
        struct BusyGuard(Arc<MnScheduler>);
        impl Drop for BusyGuard {
            fn drop(&mut self) {
                self.0.note_task_done();
            }
        }
        let _busy = if self.mn_parallel {
            Some(BusyGuard(self.mn.clone()))
        } else {
            None
        };
        if self.mn_parallel {
            if !self.mn_primary {
                self.mn.note_helper_run();
                self.pull_script_globals_if_helper();
            } else {
                // 主线程不 pull 快照，但 helper 的 StoreGlobal 只写了 SharedMap / 对方槽。
                self.overlay_script_globals_from_map();
                if self.local_fn_hot.is_empty() {
                    self.rebuild_local_fn_hot();
                }
            }
        }
        let key = Self::task_ptr_key(&task);
        // 认领：仅 Pending/Suspended 可进入 Running。重复入队的副本直接跳过。
        let state = {
            let mut inner = task.borrow_mut();
            match &inner.state {
                TaskState::Pending { .. } | TaskState::Suspended => {
                    std::mem::replace(&mut inner.state, TaskState::Running)
                }
                TaskState::Running | TaskState::Done(_) | TaskState::Failed(_) => {
                    return Ok(false);
                }
            }
        };

        // 认领后若已取消：直接 Failed(Cancelled)，不跑用户代码。
        if task.borrow().is_cancelled() {
            let exc = match exceptions::make_exception(self, "Cancelled", "task cancelled") {
                Ok(v) => v,
                Err(_) => Value::Text("task cancelled".into()),
            };
            // 丢弃 Pending 载荷 / Suspended fiber，避免泄漏。
            if matches!(state, TaskState::Suspended) {
                let _ = self.fiber_take(key);
            }
            task.borrow_mut().state = TaskState::Failed(exc.clone());
            self.taskgroup_notify_finished(&task, Some(exc));
            self.mn.notify_all();
            return Ok(true);
        }

        let saved_ctx = self.task_ctx.take();
        let saved_budget = self.budget_left;
        let saved_pending = self.pending_suspend;
        let saved_block_suspend = self.block_suspend;
        let saved_call_retry = self.call_retry_armed;
        let saved_ucd = self.user_call_deferred;
        let saved_nested_suspend = self.nested_user_call_suspended;
        self.pending_suspend = false;
        // 任务内阻塞挂起不得泄漏到调用方（主 fiber 的 Channel.recv 等），
        // 否则调用方会误走 arm_call_retry / 把栈上残留的 Task 当成 recv 结果。
        self.block_suspend = false;
        // 任务内 Call 重试 / 延迟返回标志同样不得泄漏：任务体内 `arm_call_retry`
        // 留下的 `call_retry_armed`/`user_call_deferred` 会让宿主下一次 `Call`
        // 跳过压栈返回值（如 `ch.recv()` 结果），栈顶残留值被误绑定。
        self.call_retry_armed = false;
        self.user_call_deferred = false;
        self.nested_user_call_suspended = false;
        self.sched_depth = self.sched_depth.saturating_add(1);

        let run_result = (|| -> Result<Option<Value>> {
            match state {
                TaskState::Pending { callable, args } => {
                    self.task_ctx = Some(Self::snapshot_task_ctx(task.clone(), self));
                    self.budget_left = self.suspend_budget;
                    match self.call_value_poll(callable, args)? {
                        InterpResult::Value(v) => {
                            // 丢弃任务帧，保证不污染调用方（主 fiber / 外层任务）栈。
                            if let Some(ctx) = self.task_ctx.take() {
                                let _ = self.capture_fiber(&ctx);
                            }
                            Ok(Some(v.unwrap_or(Value::None)))
                        }
                        InterpResult::Suspended => Ok(None),
                        InterpResult::DebugBreak => {
                            self.park_task_for_debug();
                            Ok(None)
                        }
                        InterpResult::Yielded(v) => {
                            if let Some(ctx) = self.task_ctx.take() {
                                let _ = self.capture_fiber(&ctx);
                            }
                            Ok(Some(v))
                        }
                    }
                }
                TaskState::Suspended => {
                    let Some(mut fiber) = self.fiber_take(key) else {
                        return Err(RuntimeError::msg(
                            "internal error: suspended task missing fiber",
                        ));
                    };
                    if let Some((callable, args)) = fiber.retry_poll.take() {
                        self.install_fiber(task.clone(), fiber);
                        match self.call_value_poll(callable, args)? {
                            InterpResult::Value(v) => {
                                if let Some(ctx) = self.task_ctx.take() {
                                    let _ = self.capture_fiber(&ctx);
                                }
                                Ok(Some(v.unwrap_or(Value::None)))
                            }
                            InterpResult::Suspended => Ok(None),
                            InterpResult::DebugBreak => {
                                self.park_task_for_debug();
                                Ok(None)
                            }
                            InterpResult::Yielded(v) => {
                                if let Some(ctx) = self.task_ctx.take() {
                                    let _ = self.capture_fiber(&ctx);
                                }
                                Ok(Some(v))
                            }
                        }
                    } else {
                        self.install_fiber(task.clone(), fiber);
                        let stop = self.task_ctx.as_ref().map_or(0, |c| c.stop_ucf);
                        match self.run_interpreter(Some(stop))? {
                            InterpResult::Value(v) => {
                                if let Some(ctx) = self.task_ctx.take() {
                                    let _ = self.capture_fiber(&ctx);
                                }
                                Ok(Some(v.unwrap_or(Value::None)))
                            }
                            InterpResult::Suspended => Ok(None),
                            InterpResult::DebugBreak => {
                                self.park_task_for_debug();
                                Ok(None)
                            }
                            InterpResult::Yielded(v) => {
                                if let Some(ctx) = self.task_ctx.take() {
                                    let _ = self.capture_fiber(&ctx);
                                }
                                Ok(Some(v))
                            }
                        }
                    }
                }
                TaskState::Running | TaskState::Done(_) | TaskState::Failed(_) => {
                    unreachable!("claimed only Pending/Suspended")
                }
            }
        })();

        match run_result {
            Ok(Some(v)) => {
                if matches!(task.borrow().state, TaskState::Running) {
                    task.borrow_mut().state = TaskState::Done(v);
                    self.taskgroup_notify_finished(&task, None);
                }
            }
            Ok(None) => {}
            Err(e) => {
                // 任务失败时同样回滚到 stop_*，避免残留栈槽破坏调用方的下一条指令。
                if let Some(ctx) = self.task_ctx.take() {
                    let _ = self.capture_fiber(&ctx);
                }
                let msg = e.message();
                let exc = match exceptions::make_exception_kind(self, e.kind(), msg) {
                    Ok(v) => v,
                    Err(_) => Value::Text(e.message().to_string()),
                };
                task.borrow_mut().state = TaskState::Failed(exc.clone());
                self.taskgroup_notify_finished(&task, Some(exc));
            }
        }

        self.sched_depth = self.sched_depth.saturating_sub(1);
        self.task_ctx = saved_ctx;
        self.budget_left = saved_budget;
        self.pending_suspend = saved_pending;
        self.block_suspend = saved_block_suspend;
        self.call_retry_armed = saved_call_retry;
        self.user_call_deferred = saved_ucd;
        self.nested_user_call_suspended = saved_nested_suspend;
        self.mn.notify_all();
        Ok(true)
    }

    pub(crate) fn scheduler_yield(&mut self) -> Result<()> {
        self.flush_script_globals_to_map();
        if self.mn_parallel {
            // 并行：与 M:1 一致，以「进入时本地队列长度」为上界跑一轮就绪任务。
            // 固定 64 次会让单个 sleep/IO 切片任务重入队后被反复抽干，
            // 饿死 select 主纤程的 deadline poll（tick 截止被拖到 channel 就绪之后）。
            let n = self.local_worker.len().max(1);
            for _ in 0..n {
                if !self.scheduler_run_one()? {
                    break;
                }
            }
        } else {
            let n = self.ready_tasks.len();
            for _ in 0..n {
                if !self.scheduler_run_one()? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// 协作式让出：任务 fiber 设 `pending_suspend`（挂起并重回就绪队列），
    /// 主 fiber 设 `pending_main_yield`（跑一轮就绪任务后继续）。
    /// 供 `std.sync.yield` 与阻塞式 IO 内建在操作前后让出 CPU，避免长 IO 饿死其它 fiber。
    pub(crate) const fn request_cooperative_yield(&mut self) {
        self.budget_left = self.suspend_budget;
        if self.task_ctx.is_some() {
            self.pending_suspend = true;
        } else {
            self.pending_main_yield = true;
        }
    }

    /// 任务纤程内的协作式 sleep：每次最多睡一小片并 `block_suspend`，让 select/其它任务推进。
    /// 主纤程仍用整段 `thread::sleep`（无调度对象可让出）。
    pub(crate) fn coop_sleep_secs(&mut self, secs: f64) -> Result<Value> {
        let secs = sanitize_coop_sleep_secs(secs);
        self.coop_sleep_duration(std::time::Duration::from_secs_f64(secs))
    }

    pub(crate) fn coop_sleep_ms(&mut self, ms: u64) -> Result<Value> {
        self.coop_sleep_duration(std::time::Duration::from_millis(ms))
    }

    fn coop_sleep_duration(&mut self, total: std::time::Duration) -> Result<Value> {
        if total.is_zero() {
            return Ok(Value::None);
        }
        self.fail_if_current_task_cancelled()?;
        let in_task = self.task_ctx.is_some() || self.sched_depth > 0;
        if !in_task {
            std::thread::sleep(total);
            return Ok(Value::None);
        }
        let until = match self.sync_wait_resume {
            Some(SyncWaitResume::Sleep { until }) => until,
            _ => {
                if let Some(t) = std::time::Instant::now().checked_add(total) {
                    t
                } else {
                    // 溢出：睡完本切片后视为到期，避免 Instant 加法 panic。
                    std::thread::sleep(total.min(COOP_SLEEP_SLICE));
                    self.sync_wait_resume = None;
                    return Ok(Value::None);
                }
            }
        };
        let now = std::time::Instant::now();
        if now >= until {
            self.sync_wait_resume = None;
            return Ok(Value::None);
        }
        let slice = (until - now).min(COOP_SLEEP_SLICE);
        std::thread::sleep(slice);
        self.fail_if_current_task_cancelled()?;
        if std::time::Instant::now() >= until {
            self.sync_wait_resume = None;
            return Ok(Value::None);
        }
        self.sync_wait_resume = Some(SyncWaitResume::Sleep { until });
        self.block_suspend = true;
        Ok(Value::None)
    }

    pub(crate) fn await_value(&mut self, value: Value) -> Result<Value> {
        let Value::Task(task) = value else {
            return Ok(value);
        };
        loop {
            if task.borrow().is_cancelled()
                && !matches!(
                    task.borrow().state,
                    TaskState::Done(_) | TaskState::Failed(_)
                )
            {
                return self.finalize_task_cancelled(&task);
            }
            let state = task.borrow().state.clone();
            match state {
                TaskState::Done(v) => return Ok(v),
                TaskState::Failed(e) => {
                    self.throw_value(e)?;
                    return Ok(Value::None);
                }
                TaskState::Pending { .. } | TaskState::Suspended => {
                    if task.borrow().debug_paused || self.debug_break_requested {
                        return Ok(Value::None);
                    }
                    self.ensure_task_runnable(&task);
                    self.wait_or_deadlock("no runnable tasks while awaiting")?;
                    if self.block_suspend || self.debug_break_requested {
                        return Ok(Value::None);
                    }
                }
                TaskState::Running => {
                    self.wait_or_deadlock("no runnable tasks while awaiting")?;
                    if self.block_suspend || self.debug_break_requested {
                        return Ok(Value::None);
                    }
                }
            }
        }
    }

    /// 确保任务在就绪队列中（不阻塞等待完成）。
    pub(crate) fn ensure_task_runnable(&mut self, task: &Shared<TaskInner>) {
        if task.borrow().debug_paused {
            return;
        }
        let state = task.borrow().state.clone();
        if matches!(state, TaskState::Pending { .. } | TaskState::Suspended) {
            if self.mn_parallel {
                self.enqueue_task(task.clone());
            } else if !self.ready_tasks.iter().any(|t| Shared::ptr_eq(t, task)) {
                self.ready_tasks.push_back(task.clone());
            }
        }
    }

    fn call_value_poll(&mut self, callee: Value, args: Vec<Value>) -> Result<InterpResult> {
        match callee {
            Value::Function(f) => self.call_user_function_poll(f, args),
            Value::Builtin(b) => {
                let out = b.call(self, &args)?;
                if self.block_suspend {
                    self.block_suspend = false;
                    self.pending_poll_retry = Some((Value::Builtin(b), args));
                    return self.complete_task_suspend();
                }
                if self.nested_user_call_suspended {
                    return Ok(InterpResult::Suspended);
                }
                Ok(InterpResult::Value(Some(out)))
            }
            other => {
                let stop = self.user_call_frames.len();
                let result = self.call_value(other, args)?;
                if self.user_call_deferred {
                    match self.run_interpreter(Some(stop))? {
                        InterpResult::Value(v) => {
                            Ok(InterpResult::Value(Some(v.unwrap_or(Value::None))))
                        }
                        InterpResult::Suspended => Ok(InterpResult::Suspended),
                        InterpResult::DebugBreak => Ok(InterpResult::DebugBreak),
                        InterpResult::Yielded(v) => Ok(InterpResult::Yielded(v)),
                    }
                } else {
                    Ok(InterpResult::Value(Some(result)))
                }
            }
        }
    }

    pub(crate) fn channel_send(&mut self, ch: &Shared<ChannelInner>, value: Value) -> Result<()> {
        loop {
            let outcome = {
                let mut inner = ch.borrow_mut();
                inner.try_send(value.clone())
            };
            match outcome {
                Some(Ok(())) => {
                    self.mn.notify_all();
                    if ch.borrow().capacity == Some(0) {
                        loop {
                            if ch.borrow().queue.is_empty() {
                                return Ok(());
                            }
                            self.wait_or_deadlock("channel send blocked")?;
                            if self.block_suspend {
                                return Ok(());
                            }
                            if ch.borrow().queue.is_empty() {
                                return Ok(());
                            }
                        }
                    }
                    return Ok(());
                }
                Some(Err(())) => {
                    return Err(RuntimeError::msg(
                        "ChannelClosed: cannot send on closed channel",
                    ));
                }
                None => {
                    self.wait_or_deadlock("channel send blocked")?;
                    if self.block_suspend {
                        return Ok(());
                    }
                }
            }
        }
    }

    pub(crate) fn channel_recv(&mut self, ch: &Shared<ChannelInner>) -> Result<Value> {
        loop {
            let outcome = {
                let mut inner = ch.borrow_mut();
                inner.try_recv()
            };
            match outcome {
                Some(Some(v)) => {
                    self.mn.notify_all();
                    return Ok(v);
                }
                Some(None) => return Ok(Value::None),
                None => {
                    self.wait_or_deadlock("channel recv blocked")?;
                    if self.block_suspend {
                        return Ok(Value::None);
                    }
                }
            }
        }
    }

    pub(crate) fn mutex_lock(&mut self, m: &Shared<MutexInner>) -> Result<Value> {
        loop {
            {
                let mut inner = m.borrow_mut();
                if !inner.locked {
                    inner.locked = true;
                    return Ok(Value::MutexGuard(Shared::new(
                        crate::value::MutexGuardInner::new(m.clone()),
                    )));
                }
            }
            self.wait_or_deadlock("mutex lock blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn rwmutex_read(&mut self, s: &Shared<crate::value::SyncInner>) -> Result<Value> {
        use crate::value::{SyncGuardInner, SyncInner};
        loop {
            {
                let mut inner = s.borrow_mut();
                if let SyncInner::RWMutex {
                    readers, writer, ..
                } = &mut *inner
                {
                    if !*writer {
                        *readers += 1;
                        return Ok(Value::SyncGuard(Shared::new(SyncGuardInner::Read {
                            mu: s.clone(),
                        })));
                    }
                } else {
                    return Err(RuntimeError::type_err("expected RWMutex"));
                }
            }
            self.wait_or_deadlock("RWMutex.read blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn rwmutex_write(&mut self, s: &Shared<crate::value::SyncInner>) -> Result<Value> {
        use crate::value::{SyncGuardInner, SyncInner};
        loop {
            {
                let mut inner = s.borrow_mut();
                if let SyncInner::RWMutex {
                    readers, writer, ..
                } = &mut *inner
                {
                    if !*writer && *readers == 0 {
                        *writer = true;
                        return Ok(Value::SyncGuard(Shared::new(SyncGuardInner::Write {
                            mu: s.clone(),
                        })));
                    }
                } else {
                    return Err(RuntimeError::type_err("expected RWMutex"));
                }
            }
            self.wait_or_deadlock("RWMutex.write blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn waitgroup_wait(&mut self, s: &Shared<crate::value::SyncInner>) -> Result<Value> {
        use crate::value::SyncInner;
        loop {
            {
                let inner = s.borrow();
                if let SyncInner::WaitGroup { count } = &*inner {
                    if *count == 0 {
                        return Ok(Value::None);
                    }
                } else {
                    return Err(RuntimeError::type_err("expected WaitGroup"));
                }
            }
            self.wait_or_deadlock("WaitGroup.wait blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn taskgroup_run(
        &mut self,
        s: &Shared<crate::value::SyncInner>,
        callable: Value,
    ) -> Result<Value> {
        use crate::value::{SyncInner, TaskInner};
        // 必须在入队前挂上 task_group，避免 M:N 下竞态漏记 done。
        let mut pending = TaskInner::pending(callable, vec![]);
        pending.task_group = Some(s.clone());
        let task = Shared::new(pending);
        {
            let mut inner = s.borrow_mut();
            let SyncInner::TaskGroup {
                count,
                cancel_requested,
                tasks,
                ..
            } = &mut *inner
            else {
                return Err(RuntimeError::type_err("expected TaskGroup"));
            };
            *count = count.saturating_add(1);
            tasks.push(task.clone());
            if *cancel_requested {
                task.borrow_mut().request_cancel();
            }
        }
        self.publish_script_globals();
        self.enqueue_new_task(task.clone());
        Ok(Value::Task(task))
    }

    pub(crate) fn taskgroup_cancel(&mut self, s: &Shared<crate::value::SyncInner>) {
        use crate::value::SyncInner;
        let tasks = {
            let mut inner = s.borrow_mut();
            let SyncInner::TaskGroup {
                cancel_requested,
                tasks,
                ..
            } = &mut *inner
            else {
                return;
            };
            *cancel_requested = true;
            tasks.clone()
        };
        for t in tasks {
            self.cancel_task(&t);
        }
    }

    pub(crate) fn taskgroup_wait(&mut self, s: &Shared<crate::value::SyncInner>) -> Result<Value> {
        use crate::value::SyncInner;
        loop {
            let first_error = {
                let mut inner = s.borrow_mut();
                let SyncInner::TaskGroup {
                    count, first_error, ..
                } = &mut *inner
                else {
                    return Err(RuntimeError::type_err("expected TaskGroup"));
                };
                if *count == 0 {
                    first_error.take()
                } else {
                    drop(inner);
                    self.wait_or_deadlock("TaskGroup.wait blocked")?;
                    if self.block_suspend {
                        return Ok(Value::None);
                    }
                    continue;
                }
            };
            if let Some(err) = first_error {
                self.throw_value(err)?;
            }
            return Ok(Value::None);
        }
    }

    fn taskgroup_notify_finished(
        &mut self,
        task: &Shared<crate::value::TaskInner>,
        failed: Option<Value>,
    ) {
        use crate::value::SyncInner;
        let Some(group) = task.borrow().task_group.clone() else {
            return;
        };
        // 只通知一次：清掉归属，避免重复 done。
        task.borrow_mut().task_group = None;
        let (hit_zero, cancel_siblings) = {
            let mut inner = group.borrow_mut();
            match &mut *inner {
                SyncInner::TaskGroup {
                    count,
                    first_error,
                    cancel_requested,
                    ..
                } => {
                    let mut cancel_siblings = false;
                    if let Some(err) = failed {
                        let is_cancelled = exceptions::struct_is_a(self, &err, "Cancelled");
                        if is_cancelled {
                            // 组主动取消时的 Cancelled 不记为组错误。
                            if !*cancel_requested && first_error.is_none() {
                                *first_error = Some(err);
                            }
                        } else if first_error.is_none() {
                            *first_error = Some(err);
                            *cancel_requested = true;
                            cancel_siblings = true;
                        }
                    }
                    if *count > 0 {
                        *count -= 1;
                    }
                    (*count == 0, cancel_siblings)
                }
                _ => (false, false),
            }
        };
        if cancel_siblings {
            self.taskgroup_cancel(&group);
        }
        if hit_zero {
            self.mn.notify_all();
        }
    }

    /// 请求协作式取消；已结束的任务不变。挂起/待运行任务会入队以便尽快以 `Cancelled` 收尾。
    pub(crate) fn cancel_task(&mut self, task: &Shared<TaskInner>) {
        let should_enqueue = {
            let mut inner = task.borrow_mut();
            if matches!(inner.state, TaskState::Done(_) | TaskState::Failed(_)) {
                return;
            }
            inner.request_cancel();
            matches!(
                inner.state,
                TaskState::Pending { .. } | TaskState::Suspended
            )
        };
        if should_enqueue {
            self.ensure_task_runnable(task);
        }
        self.mn.notify_all();
    }

    /// 若当前任务已取消，返回 `Cancelled` 宿主错误。
    pub(crate) fn fail_if_current_task_cancelled(&self) -> Result<()> {
        self.fail_if_host_cancelled()?;
        if let Some(ctx) = &self.task_ctx {
            if ctx.task.borrow().is_cancelled() {
                return Err(RuntimeError::cancelled("task cancelled"));
            }
        }
        Ok(())
    }

    pub(crate) fn fail_if_host_cancelled(&self) -> Result<()> {
        if self
            .host_cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(RuntimeError::cancelled("host cancelled"));
        }
        Ok(())
    }

    /// 将仍开放的任务以 `Cancelled` 失败收尾（用于 await 已 cancel 的任务）。
    fn finalize_task_cancelled(&mut self, task: &Shared<TaskInner>) -> Result<Value> {
        let already = {
            let state = task.borrow().state.clone();
            match state {
                TaskState::Done(v) => Some(Ok(v)),
                TaskState::Failed(e) => Some(Err(e)),
                _ => None,
            }
        };
        match already {
            Some(Ok(v)) => return Ok(v),
            Some(Err(e)) => {
                self.throw_value(e)?;
                return Ok(Value::None);
            }
            None => {}
        }
        let exc = exceptions::make_exception(self, "Cancelled", "task cancelled")?;
        {
            let mut inner = task.borrow_mut();
            if !matches!(inner.state, TaskState::Done(_) | TaskState::Failed(_)) {
                inner.state = TaskState::Failed(exc.clone());
            }
        }
        self.taskgroup_notify_finished(task, Some(exc.clone()));
        self.mn.notify_all();
        self.throw_value(exc)?;
        Ok(Value::None)
    }

    pub(crate) fn semaphore_acquire(
        &mut self,
        s: &Shared<crate::value::SyncInner>,
    ) -> Result<Value> {
        use crate::value::SyncInner;
        loop {
            {
                let mut inner = s.borrow_mut();
                if let SyncInner::Semaphore { permits } = &mut *inner {
                    if *permits > 0 {
                        *permits -= 1;
                        return Ok(Value::None);
                    }
                } else {
                    return Err(RuntimeError::type_err("expected Semaphore"));
                }
            }
            self.wait_or_deadlock("Semaphore.acquire blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn once_do(
        &mut self,
        s: &Shared<crate::value::SyncInner>,
        callable: Value,
    ) -> Result<Value> {
        use crate::value::{OncePhase, SyncInner};
        loop {
            {
                let inner = s.borrow();
                let SyncInner::Once { phase, value } = &*inner else {
                    return Err(RuntimeError::type_err("expected Once"));
                };
                match *phase {
                    OncePhase::Done => return Ok(value.clone()),
                    OncePhase::Running => {}
                    OncePhase::Idle => break,
                }
            }
            // 其它任务正在执行：等待 Done（或挂起重试本 Call）。
            self.wait_or_deadlock("Once.run blocked")?;
            if self.block_suspend {
                return Ok(Value::None);
            }
        }
        {
            let mut inner = s.borrow_mut();
            let SyncInner::Once { phase, value } = &mut *inner else {
                return Err(RuntimeError::type_err("expected Once"));
            };
            match *phase {
                OncePhase::Done => return Ok(value.clone()),
                OncePhase::Running => {
                    // 竞态下另一线程抢先：回环等待。
                    drop(inner);
                    self.wait_or_deadlock("Once.run blocked")?;
                    if self.block_suspend {
                        return Ok(Value::None);
                    }
                    return self.once_do(s, callable);
                }
                OncePhase::Idle => {
                    *phase = OncePhase::Running;
                }
            }
        }
        let result = match self.call_value_poll(callable, vec![]) {
            Ok(InterpResult::Value(v)) => v.unwrap_or(Value::None),
            Ok(InterpResult::Suspended) => {
                if let SyncInner::Once { phase, value } = &mut *s.borrow_mut() {
                    *phase = OncePhase::Idle;
                    *value = Value::None;
                }
                self.mn.notify_all();
                return Err(RuntimeError::msg(
                    "Once.run: callable suspended; use a non-suspending function",
                ));
            }
            Ok(InterpResult::DebugBreak) => {
                if let SyncInner::Once { phase, value } = &mut *s.borrow_mut() {
                    *phase = OncePhase::Idle;
                    *value = Value::None;
                }
                self.mn.notify_all();
                return Err(RuntimeError::msg("Once.run interrupted by debugger"));
            }
            Ok(InterpResult::Yielded(_)) => {
                if let SyncInner::Once { phase, value } = &mut *s.borrow_mut() {
                    *phase = OncePhase::Idle;
                    *value = Value::None;
                }
                self.mn.notify_all();
                return Err(RuntimeError::msg(
                    "Once.run: callable is a generator; pass a non-generator function",
                ));
            }
            Err(e) => {
                if let SyncInner::Once { phase, value } = &mut *s.borrow_mut() {
                    *phase = OncePhase::Idle;
                    *value = Value::None;
                }
                self.mn.notify_all();
                return Err(e);
            }
        };
        if let SyncInner::Once { phase, value } = &mut *s.borrow_mut() {
            *value = result.clone();
            *phase = OncePhase::Done;
        }
        self.mn.notify_all();
        Ok(result)
    }

    /// M:1：主纤程释放 barrier 后跑一轮就绪任务，让挂起方先从 `wait` 返回。
    #[inline]
    fn yield_after_barrier_release(&mut self) -> Result<()> {
        if self.mn_parallel || self.task_ctx.is_some() {
            return Ok(());
        }
        self.scheduler_yield()
    }

    pub(crate) fn barrier_wait(&mut self, s: &Shared<crate::value::SyncInner>) -> Result<Value> {
        use crate::value::SyncInner;
        let id = s.as_ptr() as usize;
        let my_gen = if let Some(SyncWaitResume::Barrier {
            id: rid,
            generation,
        }) = self.sync_wait_resume
        {
            if rid == id {
                generation
            } else {
                self.sync_wait_resume = None;
                let mut inner = s.borrow_mut();
                let SyncInner::Barrier {
                    n,
                    waiting,
                    generation,
                } = &mut *inner
                else {
                    return Err(RuntimeError::type_err("expected Barrier"));
                };
                *waiting += 1;
                if *waiting as i64 >= *n {
                    *waiting = 0;
                    *generation = generation.wrapping_add(1);
                    drop(inner);
                    self.sync_wait_resume = None;
                    self.mn.notify_all();
                    // 主纤程作为最后一方释放时，须让出给已挂起的其它方，
                    // 否则对方 barrier 后代码（如写共享变量）可能永远跑不到。
                    self.yield_after_barrier_release()?;
                    return Ok(Value::None);
                }
                *generation
            }
        } else {
            let mut inner = s.borrow_mut();
            let SyncInner::Barrier {
                n,
                waiting,
                generation,
            } = &mut *inner
            else {
                return Err(RuntimeError::type_err("expected Barrier"));
            };
            *waiting += 1;
            if *waiting as i64 >= *n {
                *waiting = 0;
                *generation = generation.wrapping_add(1);
                drop(inner);
                self.sync_wait_resume = None;
                self.mn.notify_all();
                self.yield_after_barrier_release()?;
                return Ok(Value::None);
            }
            *generation
        };
        loop {
            {
                let inner = s.borrow();
                if let SyncInner::Barrier { generation, .. } = &*inner {
                    if *generation != my_gen {
                        self.sync_wait_resume = None;
                        return Ok(Value::None);
                    }
                } else {
                    return Err(RuntimeError::type_err("expected Barrier"));
                }
            }
            self.wait_or_deadlock("Barrier.wait blocked")?;
            if self.block_suspend {
                self.sync_wait_resume = Some(SyncWaitResume::Barrier {
                    id,
                    generation: my_gen,
                });
                return Ok(Value::None);
            }
        }
    }

    pub(crate) fn cond_wait(
        &mut self,
        cond: &Shared<crate::value::SyncInner>,
        guard: &Shared<crate::value::MutexGuardInner>,
    ) -> Result<Value> {
        use crate::value::SyncInner;
        let id = cond.as_ptr() as usize;
        let resume_wait = matches!(
            self.sync_wait_resume,
            Some(SyncWaitResume::Cond { id: rid }) if rid == id
        );
        let resume_relock = matches!(
            self.sync_wait_resume,
            Some(SyncWaitResume::CondRelock { id: rid }) if rid == id
        );
        if resume_wait || resume_relock {
            self.sync_wait_resume = None;
        }
        if !resume_wait && !resume_relock {
            // 登记 waiter，释放锁
            {
                let mut inner = cond.borrow_mut();
                let SyncInner::Cond { waiters, .. } = &mut *inner else {
                    return Err(RuntimeError::type_err("expected Cond"));
                };
                *waiters += 1;
            }
            guard.borrow().release();
        }

        // CondRelock：信号已消耗，直接重新加锁。
        if !resume_relock {
            // 等待信号
            loop {
                let got = {
                    let mut inner = cond.borrow_mut();
                    let SyncInner::Cond { signals, waiters } = &mut *inner else {
                        return Err(RuntimeError::type_err("expected Cond"));
                    };
                    if *signals > 0 {
                        *signals -= 1;
                        *waiters -= 1;
                        true
                    } else {
                        false
                    }
                };
                if got {
                    break;
                }
                match self.wait_or_deadlock("Cond.wait blocked") {
                    Ok(()) => {
                        if self.block_suspend {
                            // 保持 waiter 登记；恢复后跳过再次 +1 / unlock。
                            self.sync_wait_resume = Some(SyncWaitResume::Cond { id });
                            return Ok(Value::None);
                        }
                    }
                    Err(e) => {
                        if let SyncInner::Cond { waiters, .. } = &mut *cond.borrow_mut() {
                            *waiters = (*waiters - 1).max(0);
                        }
                        self.sync_wait_resume = None;
                        return Err(e);
                    }
                }
            }
        }

        // 重新加锁并复活当前守卫（勿新建 Guard 再立刻 Drop，否则会再次 unlock）。
        loop {
            {
                let mutex = guard.borrow().mutex();
                let mut inner = mutex.borrow_mut();
                if !inner.locked {
                    inner.locked = true;
                    drop(inner);
                    guard.borrow().clear_released();
                    self.sync_wait_resume = None;
                    break;
                }
            }
            self.wait_or_deadlock("mutex lock blocked")?;
            if self.block_suspend {
                self.sync_wait_resume = Some(SyncWaitResume::CondRelock { id });
                return Ok(Value::None);
            }
        }
        Ok(Value::None)
    }

    pub(crate) fn zip_iterables(&mut self, iterables: Vec<Value>) -> Result<Value> {
        let mut children = Vec::new();
        for it in iterables {
            children.push(self.to_iterator_shared(&it)?);
        }
        Ok(IteratorState::from_zip(children).into_value())
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if self.mn_primary && self.mn_parallel {
            self.mn.shutdown();
        }
    }
}

/// 协作 sleep 秒数：NaN / 负 → 0；+Inf 与过大有限值钳到上界（可安全 `from_secs_f64`）。
fn sanitize_coop_sleep_secs(secs: f64) -> f64 {
    const MAX_SECS: f64 = crate::concurrency::MAX_TIMEOUT_SECS;
    if secs.is_nan() || secs <= 0.0 {
        return 0.0;
    }
    if !secs.is_finite() {
        return MAX_SECS;
    }
    secs.min(MAX_SECS)
}

fn unpack_genexpr_args_vm(item: Value, arity: usize) -> Result<Vec<Value>> {
    if arity <= 1 {
        return Ok(vec![item]);
    }
    match item {
        Value::List(l) => {
            let items = l.borrow().clone();
            if items.len() != arity {
                return Err(RuntimeError::msg(format!(
                    "generator expression expected {arity} values, got {}",
                    items.len()
                )));
            }
            Ok(items)
        }
        Value::Tuple(t) => {
            if t.len() != arity {
                return Err(RuntimeError::msg(format!(
                    "generator expression expected {arity} values, got {}",
                    t.len()
                )));
            }
            Ok(t.to_vec())
        }
        _ => Err(RuntimeError::type_err(
            "generator expression multi-binding requires list/tuple item",
        )),
    }
}

fn compare_num(a: &Value, b: &Value) -> Result<i32> {
    match compare_values(a, b)? {
        std::cmp::Ordering::Less => Ok(-1),
        std::cmp::Ordering::Equal => Ok(0),
        std::cmp::Ordering::Greater => Ok(1),
    }
}

fn compare_values(a: &Value, b: &Value) -> Result<std::cmp::Ordering> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Ok(x.cmp_num(y)),
        (Value::Text(x), Value::Text(y)) => Ok(x.as_str().cmp(y.as_str())),
        _ => Err(RuntimeError::type_err(
            "comparison requires num or text (lexicographic)",
        )),
    }
}

fn index_value(vm: &mut Vm, obj: &Value, idx: &Value) -> Result<Value> {
    match (obj, idx) {
        (Value::TypeRef(ref type_name), idx)
            if types::is_generic_type_formable(vm, type_name)
                || crate::type_registry::is_type_form(type_name)
                || crate::ptr_registry::is_ptr_type_name(type_name) =>
        {
            let args = types::type_index_operand_to_args(idx)?;
            if let Some(def) = vm.struct_defs.get(type_name) {
                if !def.type_params.is_empty() && args.len() != def.type_params.len() {
                    return Err(RuntimeError::type_err(format!(
                        "struct {type_name} expects {} type argument(s), got {}",
                        def.type_params.len(),
                        args.len()
                    )));
                }
            } else if crate::ptr_registry::is_ptr_type_name(type_name) && args.len() != 1 {
                return Err(RuntimeError::type_err(
                    "ptr[T] expects exactly 1 type argument",
                ));
            }
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                type_name.clone(),
                args,
            )))
        }
        (Value::Ptr(addr), Value::Num(n)) => {
            let i = n
                .to_i64()
                .ok_or_else(|| RuntimeError::type_err("pointer index must be integer"))?;
            crate::ffi_extra::ptr_index_get(vm, *addr, i)
        }
        (Value::List(v), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let borrowed = v
                .try_borrow()
                .ok_or_else(|| RuntimeError::msg("list is already borrowed"))?;
            let len = borrowed.len() as isize;
            let idx = if i < 0 { len + i } else { i };
            if idx < 0 || idx >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            Ok(borrowed[idx as usize].clone())
        }
        (Value::Tuple(t), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let len = t.len() as isize;
            let idx = if i < 0 { len + i } else { i };
            if idx < 0 || idx >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            Ok(t[idx as usize].clone())
        }
        (Value::Bytes(b), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let len = b.len() as isize;
            let idx = if i < 0 { len + i } else { i };
            if idx < 0 || idx >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            Ok(Value::Num(Num::Small(i64::from(b[idx as usize]))))
        }
        (Value::Text(s), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as isize;
            let idx = if i < 0 { len + i } else { i };
            if idx < 0 || idx >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            Ok(Value::Text(chars[idx as usize].to_string()))
        }
        (Value::Dict(d), key) => {
            let k = ValueKey::from_value(key)?;
            d.borrow()
                .get(&k)
                .cloned()
                .ok_or_else(|| RuntimeError::key_err("key not found"))
        }
        (Value::Struct(_), _) => vm.call_struct_method(obj, "__getitem__", vec![idx.clone()]),
        (Value::GenericFunction(template), idx) => {
            let type_args = type_args_from_runtime_index(idx)?;
            let func = specialize_generic_runtime(vm, template, type_args)?;
            Ok(Value::Function(func))
        }
        _ => Err(RuntimeError::unsupported("unsupported index operation")),
    }
}

fn index_set(vm: &mut Vm, obj: &Value, idx: &Value, val: Value) -> Result<()> {
    match (obj, idx) {
        (Value::Ptr(addr), Value::Num(n)) => {
            let i = n
                .to_i64()
                .ok_or_else(|| RuntimeError::type_err("pointer index must be integer"))?;
            crate::ffi_extra::ptr_index_set(vm, *addr, i, val)
        }
        (Value::List(v), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let len = v.borrow().len() as isize;
            let index = if i < 0 { len + i } else { i };
            if index < 0 || index >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            vm.check_list_element_write(v, &val)?;
            v.borrow_mut()[index as usize] = val;
            Ok(())
        }
        (Value::Dict(d), key) => {
            vm.check_dict_write(d, key, &val)?;
            let k = ValueKey::from_value(key)?;
            d.borrow_mut().insert(k, val);
            Ok(())
        }
        (Value::Struct(_), _) => {
            vm.call_struct_method(obj, "__setitem__", vec![idx.clone(), val])?;
            Ok(())
        }
        _ => Err(RuntimeError::unsupported("unsupported index assignment")),
    }
}

fn slice_get(vm: &mut Vm, obj: &Value, start: &Value, end: &Value, step: &Value) -> Result<Value> {
    match obj {
        Value::List(v) => {
            let len = v.borrow().len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let out: Vec<Value> = indices.into_iter().map(|i| v.borrow()[i].clone()).collect();
            Ok(Value::List(Shared::new(out)))
        }
        Value::Tuple(t) => {
            let len = t.len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let out: Vec<Value> = indices.into_iter().map(|i| t[i].clone()).collect();
            Ok(Value::Tuple(out.into()))
        }
        Value::Bytes(b) => {
            let len = b.len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let out: Vec<u8> = indices.into_iter().map(|i| b[i]).collect();
            Ok(Value::Bytes(Arc::new(out)))
        }
        Value::Text(s) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let out: String = indices.into_iter().map(|i| chars[i]).collect();
            Ok(Value::Text(out))
        }
        Value::Struct(_) => vm.call_struct_method(
            obj,
            "__getitem__",
            vec![start.clone(), end.clone(), step.clone()],
        ),
        _ => Err(RuntimeError::unsupported("unsupported slice operation")),
    }
}

fn slice_set(
    vm: &mut Vm,
    obj: &Value,
    start: &Value,
    end: &Value,
    step: &Value,
    val: Value,
) -> Result<()> {
    match obj {
        Value::List(v) => {
            let len = v.borrow().len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let replacements = match &val {
                Value::List(rhs) => rhs.borrow().clone(),
                other => vec![other.clone()],
            };
            if replacements.len() != indices.len() {
                return Err(RuntimeError::value_err("slice assignment length mismatch"));
            }
            let mut list = v.borrow_mut();
            for (i, item) in indices.into_iter().zip(replacements) {
                list[i] = item;
            }
            Ok(())
        }
        Value::Text(_) => Err(RuntimeError::value_err(
            "text does not support slice assignment",
        )),
        Value::Struct(_) => {
            vm.call_struct_method(
                obj,
                "__setitem__",
                vec![start.clone(), end.clone(), step.clone(), val],
            )?;
            Ok(())
        }
        _ => Err(RuntimeError::unsupported("unsupported slice assignment")),
    }
}

fn del_index(vm: &mut Vm, obj: &Value, idx: &Value) -> Result<()> {
    match (obj, idx) {
        (Value::List(v), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let len = v.borrow().len() as isize;
            let index = if i < 0 { len + i } else { i };
            if index < 0 || index >= len {
                return Err(RuntimeError::index_err("index out of range"));
            }
            v.borrow_mut().remove(index as usize);
            Ok(())
        }
        (Value::Dict(d), key) => {
            let k = ValueKey::from_value(key)?;
            if d.borrow_mut().remove(&k).is_none() {
                return Err(RuntimeError::key_err("key not found"));
            }
            Ok(())
        }
        (Value::Struct(_), _) => {
            vm.call_struct_method(obj, "__delitem__", vec![idx.clone()])?;
            Ok(())
        }
        _ => Err(RuntimeError::unsupported("unsupported del index operation")),
    }
}

fn del_attr(vm: &mut Vm, obj: &Value, field: &str) -> Result<()> {
    if let Value::Struct(s) = obj {
        if s.def.fields.iter().any(|f| f == field) {
            return Err(RuntimeError::msg(format!(
                "cannot delete declared field {field}"
            )));
        }
        vm.call_struct_method(obj, "__delattr__", vec![Value::Text(field.to_string())])?;
        Ok(())
    } else {
        Err(RuntimeError::attr_err("del attribute on non-struct"))
    }
}

const fn is_slice_omitted(v: &Value) -> bool {
    matches!(v, Value::None)
}

fn slice_bound_to_isize(bound: &Value, len: isize, is_end: bool) -> Result<isize> {
    if is_slice_omitted(bound) {
        return Ok(if is_end { len } else { 0 });
    }
    let i = match bound {
        Value::Num(n) => num_to_isize(n)?,
        _ => return Err(RuntimeError::type_err("slice bound must be num")),
    };
    Ok(if i < 0 { len + i } else { i })
}

fn compute_slice_indices(
    len: isize,
    start: &Value,
    end: &Value,
    step: &Value,
) -> Result<Vec<usize>> {
    let step_val = if is_slice_omitted(step) {
        1
    } else {
        match step {
            Value::Num(n) => num_to_isize(n)?,
            _ => return Err(RuntimeError::type_err("slice step must be num")),
        }
    };
    if step_val == 0 {
        return Err(RuntimeError::value_err("slice step cannot be zero"));
    }

    let mut start_val = slice_bound_to_isize(start, len, false)?;
    let mut end_val = slice_bound_to_isize(end, len, true)?;

    if step_val > 0 {
        start_val = start_val.clamp(0, len);
        end_val = end_val.clamp(0, len);
        if start_val >= end_val {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut i = start_val;
        while i < end_val {
            out.push(i as usize);
            i += step_val;
        }
        Ok(out)
    } else {
        start_val = start_val.clamp(-1, len - 1);
        end_val = end_val.clamp(-1, len - 1);
        if start_val <= end_val {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut i = start_val;
        while i > end_val {
            out.push(i as usize);
            i += step_val;
        }
        Ok(out)
    }
}

fn try_variant_case_convert(
    vm: &Vm,
    case_struct_name: &str,
    value: &Value,
) -> Option<Result<Value>> {
    let parent_variant = vm
        .variant_defs
        .values()
        .into_iter()
        .find(|vdef| vdef.cases.iter().any(|c| c.struct_name == case_struct_name))?;
    let Value::Variant(v) = value else {
        return Some(Err(type_registry::type_convert_error(
            case_struct_name,
            value,
        )));
    };
    if v.def.name != parent_variant.name && v.inst_name != parent_variant.name {
        return Some(Err(RuntimeError::type_err(format!(
            "cannot convert variant {} to {case_struct_name}",
            v.inst_name
        ))));
    }
    let Value::Struct(s) = &v.payload else {
        return Some(Err(RuntimeError::type_err(format!(
            "variant payload is not a case struct for {case_struct_name}"
        ))));
    };
    if s.def.name != case_struct_name {
        return Some(Err(RuntimeError::type_err(format!(
            "variant case {} does not match {case_struct_name}",
            s.def.name
        ))));
    }
    if s.def.fields.len() == 1 {
        return Some(Ok(s.slots.borrow()[0].clone()));
    }
    Some(Ok(value.clone()))
}

fn wrap_variant_payload(
    vm: &mut Vm,
    inst_name: &str,
    generic_args: Option<Vec<Value>>,
    payload: Value,
) -> Result<Value> {
    let vdef = vm
        .variant_defs
        .get(inst_name)
        .ok_or_else(|| RuntimeError::msg(format!("unknown variant: {inst_name}")))?;
    let case_idx = match &payload {
        Value::Struct(s) => vdef
            .cases
            .iter()
            .position(|c| c.struct_name == s.def.name)
            .ok_or_else(|| {
                RuntimeError::msg(format!(
                    "payload is not a case of variant {inst_name}: {}",
                    s.def.name
                ))
            })?,
        _ => {
            return Err(RuntimeError::type_err(format!(
                "variant {inst_name} expects case struct payload, got {}",
                payload.type_name()
            )));
        }
    };
    Ok(crate::enum_variant::wrap_variant(
        inst_name,
        &vdef,
        generic_args.unwrap_or_default(),
        case_idx,
        payload,
    ))
}

fn variant_type_attr(vm: &mut Vm, variant_name: &str, field: &str) -> Result<Value> {
    let vdef = vm
        .variant_defs
        .get(variant_name)
        .ok_or_else(|| RuntimeError::msg(format!("unknown variant: {variant_name}")))?;
    if let Some(case) = vdef.cases.iter().find(|c| c.name == field) {
        let struct_name = case.struct_name.clone();
        return Ok(Value::builtin(field, move |vm, args| {
            make_struct(vm, &struct_name, args.to_vec(), None)
        }));
    }
    Err(RuntimeError::attr_err(format!(
        "variant {variant_name} has no case {field}"
    )))
}

fn enum_type_attr(vm: &mut Vm, enum_name: &str, field: &str) -> Result<Value> {
    let def = vm
        .enum_defs
        .get(enum_name)
        .ok_or_else(|| RuntimeError::msg(format!("unknown enum: {enum_name}")))?;
    if let Some(func) = def.methods.get(field) {
        let cls = Value::type_ref(enum_name);
        let func = func.clone();
        return Ok(Value::builtin(field, move |vm, args| {
            let mut full_args = vec![cls.clone()];
            full_args.extend_from_slice(args);
            vm.call_user_function(func.clone(), full_args)
        }));
    }
    if field == "name_of" {
        let enum_name = enum_name.to_string();
        return Ok(Value::builtin("name_of", move |vm, args| {
            crate::enum_variant::enum_name_of(vm, &enum_name, args)
        }));
    }
    if let Some(idx) = def.members.iter().position(|m| m.name == field) {
        return Ok(crate::enum_variant::enum_member_value(&def, idx));
    }
    Err(RuntimeError::attr_err(format!(
        "enum {enum_name} has no member or method {field}"
    )))
}

fn type_spec_attr(vm: &mut Vm, name: &str, type_args: &[Value], field: &str) -> Result<Value> {
    if let Some(vdef) = vm.variant_defs.get(name) {
        if vdef.cases.iter().any(|c| c.name == field) {
            let struct_name = crate::enum_variant::case_struct_name(name, field);
            let generic_args = type_args.to_vec();
            return Ok(Value::builtin(field, move |vm, args| {
                make_struct(vm, &struct_name, args.to_vec(), Some(generic_args.clone()))
            }));
        }
    }
    if vm.struct_defs.contains_key(name) {
        return Err(RuntimeError::attr_err(format!(
            "type spec has no attribute {field}"
        )));
    }
    Err(RuntimeError::msg(format!("unknown type spec {name}")))
}

fn resolve_type_ref_attr(vm: &mut Vm, type_name: &str, field: &str) -> Result<Value> {
    if vm.enum_defs.contains_key(type_name) {
        return enum_type_attr(vm, type_name, field);
    }
    if vm.variant_defs.contains_key(type_name) {
        return variant_type_attr(vm, type_name, field);
    }
    if field == "__convert__" && type_registry::supports_convert(vm, type_name) {
        let table = vm.get_or_create_convert(type_name);
        return Ok(Value::Dispatch(table));
    }
    Err(RuntimeError::attr_err(format!(
        "type {type_name} has no attribute {field}"
    )))
}

fn get_attr(vm: &mut Vm, obj: &Value, field: &str) -> Result<Value> {
    if let Some(v) = type_registry::bind_primitive_method(vm, obj, field) {
        return Ok(v);
    }
    match obj {
        Value::Module(m) => m
            .borrow()
            .get_attr(field)
            .ok_or_else(|| RuntimeError::attr_err(format!("module has no export '{field}'"))),
        Value::Dispatch(table) if field == "__dispatch__" => {
            Ok(Value::List(table.borrow().handlers.clone()))
        }
        Value::TypeRef(ref type_name) => resolve_type_ref_attr(vm, type_name, field),
        Value::Text(s) => type_registry::get_text_method(s, field),
        Value::TypeSpec(spec) => type_spec_attr(vm, &spec.name, &spec.args, field),
        Value::EnumMember(m) if field == "__value__" => Ok(Value::Num(
            crate::enum_variant::enum_member_numeric_value(m),
        )),
        Value::EnumMember(_) => Err(RuntimeError::attr_err(format!(
            "enum member has no field {field}"
        ))),
        Value::Variant(v) if field == "value" || field == "__payload__" => Ok(v.payload.clone()),
        Value::List(list) => type_registry::get_list_method(list, field),
        Value::Dict(dict) => type_registry::get_dict_method(dict, field),
        Value::Set(set) => type_registry::get_set_method(set, field),
        Value::Tuple(tuple) => type_registry::get_tuple_method(tuple, field),
        Value::Bytes(bytes) => type_registry::get_bytes_method(bytes, field),
        Value::Task(t) => crate::concurrency::get_task_method(t, field),
        Value::Channel(ch) => crate::concurrency::get_channel_method(ch, field),
        Value::Stream(s) => crate::concurrency::get_stream_method(s, field),
        Value::Mutex(m) => crate::concurrency::get_mutex_method(m, field),
        Value::MutexGuard(m) => crate::concurrency::get_mutex_guard_method(m, field),
        Value::Sync(s) => crate::concurrency::get_sync_method(s, field),
        Value::SyncGuard(g) => crate::concurrency::get_sync_guard_method(g, field),
        Value::Struct(s) => {
            if let Some(idx) = s.def.fields.iter().position(|f| f == field) {
                return Ok(s.slots.borrow()[idx].clone());
            }
            if let Some(func) = s.def.methods.get(field) {
                let self_val = obj.clone();
                let func = func.clone();
                return Ok(Value::builtin(field, move |vm, args| {
                    let mut full_args = vec![self_val.clone()];
                    full_args.extend_from_slice(args);
                    vm.call_user_function(func.clone(), full_args)
                }));
            }
            if let Some(overloads) = s.def.overloads.get(field) {
                let self_val = obj.clone();
                let overloads = overloads.clone();
                return Ok(Value::builtin(field, move |vm, args| {
                    let mut full_args = vec![self_val.clone()];
                    full_args.extend_from_slice(args);
                    vm.dispatch_overload(&overloads, &full_args)
                }));
            }
            Err(RuntimeError::attr_err(format!("no field {field}")))
        }
        _ => {
            if let Some(f) = vm.functions.get(field) {
                return Ok(Value::Function(f));
            }
            Err(RuntimeError::attr_err(format!("no attribute {field}")))
        }
    }
}

fn set_field(vm: &mut Vm, obj: &Value, field: &str, val: Value) -> Result<()> {
    if let Value::Struct(s) = obj {
        let idx = s
            .def
            .fields
            .iter()
            .position(|f| f == field)
            .ok_or_else(|| RuntimeError::attr_err(format!("no field {field}")))?;
        if !s.def.mutable_fields[idx] {
            return Err(RuntimeError::attr_err(format!(
                "field {field} is not mutable"
            )));
        }
        if let Some(info) = s.def.field_types.get(idx) {
            if info.strict {
                if let Some(ref ty_expr) = info.type_expr {
                    let subs: std::collections::HashMap<String, Value> = s
                        .def
                        .type_params
                        .iter()
                        .zip(s.generic_args.iter())
                        .map(|((p, _), v)| (p.clone(), v.clone()))
                        .collect();
                    let subbed = types::substitute_type_annotation(ty_expr, &subs);
                    let resolved = types::eval_type_annotation(vm, &subbed)?;
                    if let Some(detail) = types::type_check_error(vm, &val, &resolved) {
                        let msg = format!("field '{field}': {detail}");
                        let exc = exceptions::make_exception(vm, "TypeError", msg)?;
                        vm.throw_value(exc)?;
                        return Ok(());
                    }
                    types::seal_container_contract(vm, &val, &resolved);
                }
            }
        }
        s.slots.borrow_mut()[idx] = val;
        Ok(())
    } else {
        Err(RuntimeError::msg("set_field on non-struct"))
    }
}

fn make_struct(
    vm: &mut Vm,
    name: &str,
    args: Vec<Value>,
    explicit_generics: Option<Vec<Value>>,
) -> Result<Value> {
    let def = vm
        .struct_defs
        .get(name)
        .ok_or_else(|| RuntimeError::msg(format!("unknown struct: {name}")))?;
    let allow_partial = def.fields.len() == 2
        && def.fields.get(1).map(std::string::String::as_str) == Some("traceback")
        && args.len() == 1;
    if args.len() > def.fields.len() {
        return Err(RuntimeError::type_err(format!(
            "struct {name} expects at most {} args, got {}",
            def.fields.len(),
            args.len()
        )));
    }
    if args.len() != def.fields.len() && !allow_partial {
        return Err(RuntimeError::type_err(format!(
            "struct {name} expects {} args, got {}",
            def.fields.len(),
            args.len()
        )));
    }

    let generic_args: Vec<Value> = if let Some(explicit) = explicit_generics {
        if explicit.len() != def.type_params.len() {
            return Err(RuntimeError::type_err(format!(
                "struct {name} expects {} type argument(s), got {}",
                def.type_params.len(),
                explicit.len()
            )));
        }
        explicit
    } else if !def.type_params.is_empty() {
        let inferred = types::infer_generic_args(vm, &def, &args);
        if inferred.len() < def.type_params.len() {
            return Err(RuntimeError::msg(format!(
                "cannot infer type parameter(s) for struct {name}"
            )));
        }
        def.type_params
            .iter()
            .map(|(p, _)| {
                inferred
                    .get(p)
                    .cloned()
                    .unwrap_or_else(|| Value::type_ref(p.clone()))
            })
            .collect()
    } else {
        Vec::new()
    };

    if !def.type_params.is_empty()
        && !types::check_type_param_bounds(vm, &def.type_params, &generic_args)
    {
        let exc = exceptions::make_exception(
            vm,
            "TypeError",
            format!("type argument out of bounds for {name}"),
        )?;
        vm.throw_value(exc)?;
        return Ok(Value::None);
    }

    let subs: std::collections::HashMap<String, Value> = def
        .type_params
        .iter()
        .zip(generic_args.iter())
        .map(|((p, _), ty)| (p.clone(), ty.clone()))
        .collect();

    let mut slots = args;
    while slots.len() < def.fields.len() {
        slots.push(Value::None);
    }
    for (i, val) in slots.iter().enumerate() {
        if let Some(info) = def.field_types.get(i) {
            if info.strict {
                if let Some(ref ty_expr) = info.type_expr {
                    let subbed = types::substitute_type_annotation(ty_expr, &subs);
                    let resolved = types::eval_type_annotation(vm, &subbed)?;
                    if let Some(detail) = types::type_check_error(vm, val, &resolved) {
                        let msg = format!("field '{}': {detail}", def.fields[i]);
                        let exc = exceptions::make_exception(vm, "TypeError", msg)?;
                        vm.throw_value(exc)?;
                        return Ok(Value::None);
                    }
                    types::seal_container_contract(vm, val, &resolved);
                }
            }
        }
    }
    let val = Value::Struct(Arc::new(crate::value::StructInstance {
        def,
        slots: SyncCell::new(slots),
        generic_args,
    }));
    vm.track_value(&val);
    if let Value::Struct(s) = &val {
        if vm.struct_has_method(&val, "__init__") {
            let init_args = if let Some(func) = s.def.methods.get("__init__") {
                let n_bind = func
                    .params
                    .iter()
                    .filter(|p| !p.is_variadic && !p.is_kwvariadic)
                    .count()
                    .saturating_sub(1);
                s.slots.borrow().iter().take(n_bind).cloned().collect()
            } else {
                s.slots.borrow().clone()
            };
            vm.call_struct_method(&val, "__init__", init_args)?;
        }
    }
    Ok(val)
}

fn seq_items_for_unpack(v: &Value) -> Result<Vec<Value>> {
    match v {
        Value::List(lst) => Ok(lst.borrow().clone()),
        Value::Tuple(t) => Ok(t.to_vec()),
        _ => Err(RuntimeError::type_err("can only unpack list or tuple")),
    }
}

fn num_to_isize(n: &Num) -> Result<isize> {
    match n {
        Num::Small(i) => Ok(*i as isize),
        Num::Int(i) => i
            .as_ref()
            .try_into()
            .map_err(|_| RuntimeError::index_err("index too large")),
        Num::Rat(r) => {
            if r.denom() == &num_traits::One::one() {
                let i: i64 = r
                    .numer()
                    .try_into()
                    .map_err(|_| RuntimeError::type_err("bad index"))?;
                Ok(i as isize)
            } else {
                Err(RuntimeError::type_err("index must be integer"))
            }
        }
    }
}

/// select 非阻塞试收：沿拉取包装找到底层 Channel；纯列表流视为立即就绪。
fn select_try_recv_from_iter(it: &Shared<IteratorState>) -> Result<Option<Option<Value>>> {
    use crate::value::IteratorKind;
    match &mut it.borrow_mut().kind {
        IteratorKind::Channel { channel } => Ok(channel.borrow_mut().try_recv()),
        IteratorKind::Take { remaining, source } => {
            if *remaining == 0 {
                return Ok(Some(None));
            }
            let source = source.clone();
            match select_try_recv_from_iter(&source)? {
                Some(Some(v)) => {
                    *remaining -= 1;
                    Ok(Some(Some(v)))
                }
                other => Ok(other),
            }
        }
        IteratorKind::Skip { remaining, source } => {
            let source = source.clone();
            while *remaining > 0 {
                match select_try_recv_from_iter(&source)? {
                    Some(Some(_)) => *remaining -= 1,
                    Some(None) => return Ok(Some(None)),
                    None => return Ok(None),
                }
            }
            select_try_recv_from_iter(&source)
        }
        IteratorKind::List { items, index } => {
            if *index >= items.len() {
                Ok(Some(None))
            } else {
                let v = items[*index].clone();
                *index += 1;
                Ok(Some(Some(v)))
            }
        }
        IteratorKind::Map { .. }
        | IteratorKind::Filter { .. }
        | IteratorKind::GenExpr { .. }
        | IteratorKind::Enumerate { .. }
        | IteratorKind::Chain { .. }
        | IteratorKind::User { .. } => Err(RuntimeError::type_err(
            "select recv on mapped/filtered Stream is unsupported; use Channel or bare stream",
        )),
        _ => Err(RuntimeError::type_err(
            "select recv expects Channel-backed Stream",
        )),
    }
}

fn match_values_equal(a: &Value, b: &Value) -> bool {
    use crate::enum_variant::enum_member_numeric_value;

    match (a, b) {
        (Value::None, Value::None) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x.cmp_num(y) == std::cmp::Ordering::Equal,
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::EnumMember(x), Value::EnumMember(y)) => {
            std::sync::Arc::ptr_eq(&x.def, &y.def) && x.member_index == y.member_index
        }
        (Value::EnumMember(m), Value::Num(n)) | (Value::Num(n), Value::EnumMember(m)) => {
            enum_member_numeric_value(m).eq_num(n)
        }
        _ => a.eq(b).unwrap_or(false),
    }
}

fn value_key_to_display(k: &ValueKey) -> String {
    match k {
        ValueKey::Bool(b) => b.to_string(),
        ValueKey::NumInt(n) => n.to_string(),
        ValueKey::Text(s) => s.clone(),
    }
}

fn const_default_value_runtime(expr: &crate::ast::Expr) -> Option<Value> {
    match &expr.kind {
        crate::ast::ExprKind::None => Some(Value::None),
        crate::ast::ExprKind::Bool(b) => Some(Value::Bool(*b)),
        crate::ast::ExprKind::String(s) => Some(Value::Text(s.clone())),
        crate::ast::ExprKind::Number(s) => Num::from_literal(s).ok().map(Value::Num),
        _ => None,
    }
}

fn specialize_generic_runtime(
    vm: &mut Vm,
    template: &crate::opcode::GenericFunctionTemplate,
    type_args: Vec<Value>,
) -> Result<Arc<FunctionObject>> {
    let ctx = crate::protocol::TypeCheckContext::from_vm(vm);
    let mut cache: HashMap<String, Arc<FunctionObject>> =
        vm.functions.snapshot_map().into_iter().collect();
    let func = crate::codegen::Generator::specialize_generic_template(
        template, &type_args, &ctx, &mut cache,
    )?;
    vm.functions.with_mut(|m| {
        for (k, v) in cache {
            m.entry(k).or_insert(v);
        }
    });
    Ok(func)
}

fn infer_generic_type_args_from_values(
    vm: &Vm,
    template: &crate::opcode::GenericFunctionTemplate,
    args: &[Value],
) -> Result<Vec<Value>> {
    use crate::ast::ExprKind;
    let mut inferred: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for (param, arg) in template.params.iter().zip(args.iter()) {
        let Some(ty_expr) = &param.type_expr else {
            continue;
        };
        let ExprKind::Var(name) = &ty_expr.kind else {
            continue;
        };
        if !template.type_params.iter().any(|(p, _)| p == name) {
            continue;
        }
        let ty = types::value_to_type_value(vm, arg);
        if let Some(prev) = inferred.get(name) {
            if !types::type_values_equal(prev, &ty) {
                return Err(RuntimeError::msg(format!(
                    "conflicting inferences for type parameter `{name}` at call to `{}`",
                    template.name
                )));
            }
        } else {
            inferred.insert(name.clone(), ty);
        }
    }
    if inferred.is_empty() && template.type_params.len() == 1 && !args.is_empty() {
        inferred.insert(
            template.type_params[0].0.clone(),
            types::value_to_type_value(vm, &args[0]),
        );
    }
    let mut out = Vec::with_capacity(template.type_params.len());
    for (name, _) in &template.type_params {
        match inferred.get(name) {
            Some(v) => out.push(v.clone()),
            None => {
                return Err(RuntimeError::msg(format!(
                    "cannot infer {} type parameter(s) for `{}`; use {}[...](...)",
                    template.type_params.len(),
                    template.name,
                    template.name
                )));
            }
        }
    }
    Ok(out)
}

fn type_args_from_runtime_index(idx: &Value) -> Result<Vec<Value>> {
    types::type_index_operand_to_args(idx)
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}
