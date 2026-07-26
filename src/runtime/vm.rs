use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::builtins;
use crate::error::RuntimeError;
use crate::exceptions;
use crate::module;
use crate::runtime_ast::{self, RuntimeAstNode};
use crate::traceback;
use crate::type_registry;
use crate::types::{self, type_expr_display};
use crate::ast::TypeExpr;
use crate::opcode::{CompiledProgram, FunctionObject, Instruction, MacroObject, ModuleGlobalEnv};
use crate::value::{BuiltinFn, DictMap, DispatchTable, IteratorKind, IteratorState, ModuleObject, Num, Value, ValueKey, values_identical};
use crate::Result;

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
    Heap(Box<Value>),
}

/// 热循环控制流（模块级；不可放在 impl 内）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum HotFlow {
    Cont,
    Cold,
    PendingRet,
    Fail,
}

impl StackVal {
    #[inline]
    fn from_value(v: Value) -> Self {
        match v {
            Value::None => StackVal::Empty,
            Value::Bool(b) => StackVal::Bool(b),
            Value::Num(Num::Small(n)) => StackVal::Int(n),
            other => StackVal::Heap(Box::new(other)),
        }
    }

    #[inline]
    fn into_value(self) -> Value {
        match self {
            StackVal::Empty => Value::None,
            StackVal::Bool(b) => Value::Bool(b),
            StackVal::Int(n) => Value::Num(Num::Small(n)),
            StackVal::Heap(b) => *b,
        }
    }

    #[inline]
    fn to_value(&self) -> Value {
        match self {
            StackVal::Empty => Value::None,
            StackVal::Bool(b) => Value::Bool(*b),
            StackVal::Int(n) => Value::Num(Num::Small(*n)),
            StackVal::Heap(b) => (**b).clone(),
        }
    }

    /// 复制栈槽：内联变体按位复制，堆变体对内部 Value 做 Clone。
    #[inline(always)]
    fn copy_imm(&self) -> Self {
        match self {
            StackVal::Empty => StackVal::Empty,
            StackVal::Bool(b) => StackVal::Bool(*b),
            StackVal::Int(n) => StackVal::Int(*n),
            StackVal::Heap(v) => StackVal::Heap(Box::new((**v).clone())),
        }
    }

    #[inline(always)]
    fn is_truthy(&self) -> bool {
        match self {
            StackVal::Empty => false,
            StackVal::Bool(b) => *b,
            StackVal::Int(n) => *n != 0,
            StackVal::Heap(b) => b.is_truthy(),
        }
    }
}

/// 用户调用与轻量 CallSelf 的最大嵌套深度（防无限递归占满栈）。
/// 可用环境变量 `OPTIVE_MAX_CALL_DEPTH` 覆盖。
fn max_call_depth() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("OPTIVE_MAX_CALL_DEPTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(10_000)
    })
}

/// 加载期一次性校验热字节码结构。畸形字节码在此干净报错，
/// 让主循环的安全索引（`ops[pc]` 等）有了显式边界保证。
fn validate_hot_bytecode(hot: &crate::hot_code::HotCode) -> Result<()> {
    use crate::hot_code::*;
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
        let is_jump = matches!(
            op,
            H_GOTO | H_GOTO_IF | H_GOTO_IF_NOT
        );
        if is_jump {
            let target = arg;
            if target < 0 || (target as usize) >= n {
                return Err(RuntimeError::msg(format!(
                    "internal: hot bytecode jump at pc={} targets out-of-range {} (len={})",
                    pc, target, n
                )));
            }
        }
    }
    Ok(())
}

/// GC 跟踪表超过此阈值时自动触发环收集。
/// 可用环境变量 `OPTIVE_GC_THRESHOLD` 覆盖。
fn gc_auto_threshold() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("OPTIVE_GC_THRESHOLD")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(8_192)
    })
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
    /// 进入 try 时的轻量 CallSelf 深度（`fast_ret_sp`）。
    fast_ret_sp: usize,
}

#[derive(Clone)]
struct UserCallFrame {
    saved_code: Rc<Vec<Instruction>>,
    saved_hot_ops: Rc<[u8]>,
    saved_hot_args: Rc<[i64]>,
    saved_pc: usize,
    saved_line_map: Rc<Vec<usize>>,
    saved_column_map: Rc<Vec<usize>>,
    func: Rc<FunctionObject>,
    pushed_func_stack: bool,
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
    pub source: Option<Rc<str>>,
}

pub struct Vm {
    pub code: Rc<Vec<Instruction>>,
    /// 与 code 等长的紧凑热操作码。
    hot_ops: Rc<[u8]>,
    hot_args: Rc<[i64]>,
    /// 主操作数栈存储区；有效元素个数由 stack_sp 限定（超出部分为复用缓冲）。
    stack: Vec<StackVal>,
    /// 逻辑栈顶下标；热路径用下标读写替代 Vec::push/pop。
    stack_sp: usize,
    pub globals: FxHashMap<String, Value>,
    pub locals_stack: Vec<Vec<Value>>,
    pub name_to_slot: Vec<Option<FxHashMap<String, usize>>>,
    pub func_stack: Vec<Rc<FunctionObject>>,
    pub func_frames: Vec<FuncFrame>,
    pub pc: usize,
    pub active_line_map: Rc<Vec<usize>>,
    pub active_column_map: Rc<Vec<usize>>,
    pub struct_defs: FxHashMap<String, Rc<crate::value::StructDef>>,
    pub enum_defs: FxHashMap<String, Rc<crate::value::EnumDef>>,
    pub variant_defs: FxHashMap<String, Rc<crate::value::VariantDef>>,
    pub functions: FxHashMap<String, Rc<FunctionObject>>,
    pub macros: FxHashMap<String, Rc<MacroObject>>,
    pub(crate) try_stack: Vec<TryFrame>,
    pub(crate) active_exception: Option<Value>,
    pub(crate) iterators: Vec<ActiveIter>,
    pub(crate) const_names: FxHashSet<String>,
    /// 已声明但尚未执行到对应 store 的 const 名（允许先引用后赋值）。
    pub(crate) pending_const: FxHashSet<String>,
    pub module_cache: FxHashMap<String, Rc<RefCell<ModuleObject>>>,
    pub builtin_modules: FxHashMap<String, Rc<RefCell<ModuleObject>>>,
    pub module_init_exports: Option<Rc<RefCell<HashMap<String, Value>>>>,
    macro_eval_scopes: Vec<EvalSnapshot>,
    convert_tables: FxHashMap<String, Rc<RefCell<DispatchTable>>>,
    pub source_file: String,
    /// 当前执行中的顶层代码块源文本（REPL / 脚本）。
    pub(crate) current_source: Option<Rc<str>>,
    /// 运行失败 unwind 前捕获；供错误格式化消费。
    pub(crate) last_error_stack: Vec<ErrorStackFrame>,
    pub import_base: std::path::PathBuf,
    /// 依赖可见性：`(parent_package_id, name) → 包根`
    pub dep_map: std::collections::HashMap<(String, String), DepPackage>,
    /// 当前执行模块所属包（`__root__` 或 content id）
    pub current_package_id: String,
    /// 当前包根目录（依赖包内模块解析用）
    pub package_root: Option<std::path::PathBuf>,
    pub overload_tables: FxHashMap<String, Vec<Rc<FunctionObject>>>,
    pub(crate) primitive_methods: FxHashMap<String, FxHashMap<String, BuiltinFn>>,
    user_call_frames: Vec<UserCallFrame>,
    user_call_deferred: bool,
    script_global_names: Vec<String>,
    script_globals: Vec<Value>,
    local_frame_pool: Vec<Vec<Value>>,
    call_args_buf: Vec<Value>,
    /// 轻量 CallSelf 调用链保存的返回 PC（缓冲复用，见 fast_ret_sp）。
    fast_ret_pcs: Vec<usize>,
    /// fast_ret_pcs 已用深度；push/pop 不走 Vec::push/pop。
    fast_ret_sp: usize,
    /// 轻量 CallSelf 使用的快局部槽数组。
    lw_slots: Vec<StackVal>,
    /// 每层快路径帧在 lw_slots 中的起始下标栈（缓冲复用）。
    lw_bases: Vec<usize>,
    /// lw_bases 已用深度。
    lw_bases_sp: usize,
    /// lw_slots 已用长度；截断时只改此计数，避免对尾部槽位 drop/resize。
    lw_sp: usize,
    /// 当前帧在 lw_slots 中的基址；LoadFast 相对此偏移取槽。
    lw_base: usize,
    /// 嵌套 CallSelf 深度；为 0 表示未进入快路径局部帧。
    lw_depth: usize,
    /// CallSelf 进入时的入口 PC；返回时与 func_stack 等状态一并恢复。
    lw_entry_pc: usize,
    lw_frame_slots: usize,
    /// 热路径 Ret 延迟完成：保存 (leave_scope, result) 待外层解释循环处理。
    pending_ret: Option<(bool, StackVal)>,
    /// 热路径是否已失败；用 bool 避免每次错误路径都 Option::take。
    hot_failed: bool,
    /// 与 hot_failed 配套的详细错误；失败时由外层取出。
    hot_error: Option<RuntimeError>,
    /// 堆对象 Weak 跟踪表；与 VM 根标记配合，在 gc_collect 时清扫环。
    pub gc: crate::gc::GcTracker,
    /// `:: list[T]` 等强绑定后挂在列表对象上的元素契约（按 Rc 指针键）。
    pub(crate) list_element_contracts: FxHashMap<usize, crate::ast::TypeExpr>,
    pub(crate) dict_contracts: FxHashMap<usize, (crate::ast::TypeExpr, crate::ast::TypeExpr)>,
    pub(crate) set_element_contracts: FxHashMap<usize, crate::ast::TypeExpr>,
    /// 已编译程序中的协议定义（供运行时 `is_a` / `:: Protocol`）。
    pub(crate) protocols: FxHashMap<String, Rc<crate::protocol::ProtocolDef>>,
}

#[derive(Clone)]
pub(crate) struct EvalSnapshot {
    pub(crate) globals: FxHashMap<String, Value>,
    pub(crate) locals_stack: Vec<Vec<Value>>,
    pub(crate) name_to_slot: Vec<Option<FxHashMap<String, usize>>>,
    code: Rc<Vec<Instruction>>,
    pc: usize,
    stack: Vec<StackVal>,
    functions: FxHashMap<String, Rc<FunctionObject>>,
    macros: FxHashMap<String, Rc<MacroObject>>,
    struct_defs: FxHashMap<String, Rc<crate::value::StructDef>>,
    enum_defs: FxHashMap<String, Rc<crate::value::EnumDef>>,
    variant_defs: FxHashMap<String, Rc<crate::value::VariantDef>>,
    script_global_names: Vec<String>,
    script_globals: Vec<Value>,
}

pub(crate) struct ActiveIter {
    state: Rc<RefCell<IteratorState>>,
}

enum StepAction {
    Push(Value),
    PushSmall(i64),
    Pop,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Neg,
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
    NewVar { name: String, is_const: bool },
    NewVarOrLoad(String),
    LoadFast(usize),
    StoreFast(usize),
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
    Call { argc: usize },
    CallSelf { argc: usize },
    CallList,
    CallEx,
    MacroCall { argc: usize },
    ListAppend,
    ListExtend,
    DictSet,
    SetAdd,
    Ret,
    RetFast,
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
    StructNew { name: String, argc: usize },
    VariantNew { name: String },
    SetField(String),
    IterNew,
    IterNext,
    IterEnd,
    Throw,
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
    UnpackRest { before: usize, after: usize },
    Rethrow,
    TypeCheck(TypeExpr),
    FindMod(String),
    RegisterExport(String),
}

pub struct ModuleInitSnapshot {
    pub(crate) globals: FxHashMap<String, Value>,
    pub(crate) functions: FxHashMap<String, Rc<FunctionObject>>,
    pub(crate) macros: FxHashMap<String, Rc<MacroObject>>,
    pub(crate) struct_defs: FxHashMap<String, Rc<crate::value::StructDef>>,
    pub(crate) const_names: FxHashSet<String>,
    pub(crate) module_init_exports: Option<Rc<RefCell<HashMap<String, Value>>>>,
    pub(crate) code: Rc<Vec<Instruction>>,
    pub(crate) pc: usize,
    pub(crate) script_global_names: Vec<String>,
    pub(crate) script_globals: Vec<Value>,
}

impl Vm {
    pub fn new() -> Self {
        let mut vm = Self {
            code: Rc::new(Vec::new()),
            hot_ops: Rc::from([]),
            hot_args: Rc::from([]),
            stack: Vec::with_capacity(256),
            stack_sp: 0,
            globals: FxHashMap::default(),
            locals_stack: Vec::new(),
            name_to_slot: Vec::new(),
            func_stack: Vec::new(),
            func_frames: Vec::new(),
            pc: 0,
            active_line_map: Rc::new(Vec::new()),
            active_column_map: Rc::new(Vec::new()),
            struct_defs: FxHashMap::default(),
            enum_defs: FxHashMap::default(),
            variant_defs: FxHashMap::default(),
            functions: FxHashMap::default(),
            macros: FxHashMap::default(),
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
            overload_tables: FxHashMap::default(),
            primitive_methods: FxHashMap::default(),
            user_call_frames: Vec::new(),
            user_call_deferred: false,
            script_global_names: Vec::new(),
            script_globals: Vec::new(),
            local_frame_pool: Vec::new(),
            call_args_buf: Vec::with_capacity(8),
            fast_ret_pcs: Vec::with_capacity(128),
            fast_ret_sp: 0,
            lw_slots: Vec::with_capacity(1024),
            lw_bases: Vec::with_capacity(128),
            lw_bases_sp: 0,
            lw_sp: 0,
            lw_base: 0,
            lw_depth: 0,
            lw_entry_pc: 0,
            lw_frame_slots: 0,
            pending_ret: None,
            hot_failed: false,
            hot_error: None,
            gc: crate::gc::GcTracker::new(),
            list_element_contracts: FxHashMap::default(),
            dict_contracts: FxHashMap::default(),
            set_element_contracts: FxHashMap::default(),
            protocols: FxHashMap::default(),
        };
        builtins::install_globals(&mut vm);
        type_registry::install_core_types(&mut vm);
        module::install_std(&mut vm);
        vm
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
        if self.gc.tracked_count() >= gc_auto_threshold() {
            self.gc_collect();
        }
    }

    /// 标记-清扫环收集：从根出发标记可达堆对象，再清空不可达容器内部。
    /// 需临时取出 tracker（避免 RefCell 与 &mut self 别名冲突）。
    pub fn gc_collect(&mut self) -> usize {
        // 先将 tracker 移出 self（collect 需要 &Vm），处理完再挂回；否则无法同时持有 &mut self.gc 与 &Vm。
        let mut tracker = std::mem::take(&mut self.gc);
        let cleared = tracker.collect(self);
        self.gc = tracker;
        cleared
    }

    /// 将 VM 中所有 GC 根加入工作表并标记到 marked。
    /// 由 GcTracker::collect 调用；传入 &Vm 以便遍历各根集。
    pub(crate) fn gc_mark_roots(&self, marked: &mut FxHashSet<usize>) {
        let mut worklist: Vec<Value> = Vec::new();

        // 操作数栈 / 快局部
        for v in self.stack.get(..self.stack_sp).unwrap_or(&[]) {
            worklist.push(v.to_value());
        }
        for v in &self.lw_slots[..self.lw_sp.min(self.lw_slots.len())] {
            worklist.push(v.to_value());
        }
        // 局部变量帧
        for frame in &self.locals_stack {
            for v in frame {
                worklist.push(v.clone());
            }
        }
        // 脚本顶层全局
        for v in &self.script_globals {
            worklist.push(v.clone());
        }
        // globals 表
        for v in self.globals.values() {
            worklist.push(v.clone());
        }
        // 已注册函数及其闭包捕获
        for f in self.functions.values() {
            worklist.push(Value::Function(f.clone()));
            if let Some(cap) = &f.captured {
                for v in cap.values() {
                    worklist.push(v.clone());
                }
            }
        }
        // 当前调用栈上的函数对象
        for f in &self.func_stack {
            worklist.push(Value::Function(f.clone()));
            if let Some(cap) = &f.captured {
                for v in cap.values() {
                    worklist.push(v.clone());
                }
            }
        }
        // 活动异常
        if let Some(exc) = &self.active_exception {
            worklist.push(exc.clone());
        }
        // 进行中的 for 循环迭代器
        for it in &self.iterators {
            worklist.push(Value::Iterator(it.state.clone()));
        }
        // convert 分发表
        for dt in self.convert_tables.values() {
            worklist.push(Value::Dispatch(dt.clone()));
        }
        // 重载分发表
        for fns in self.overload_tables.values() {
            for f in fns {
                worklist.push(Value::Function(f.clone()));
                if let Some(cap) = &f.captured {
                    for v in cap.values() {
                        worklist.push(v.clone());
                    }
                }
            }
        }
        // 已加载模块
        for m in self.module_cache.values() {
            worklist.push(Value::Module(m.clone()));
        }
        for m in self.builtin_modules.values() {
            worklist.push(Value::Module(m.clone()));
        }
        // 模块 init 导出表
        if let Some(e) = &self.module_init_exports {
            for v in e.borrow().values() {
                worklist.push(v.clone());
            }
        }
        // 宏求值快照中的 globals/locals/functions/stack 等
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
                    for v in cap.values() {
                        worklist.push(v.clone());
                    }
                }
            }
        }
        // 用户调用帧中挂起的闭包捕获
        for fr in &self.user_call_frames {
            if let Some(cap) = &fr.func.captured {
                for v in cap.values() {
                    worklist.push(v.clone());
                }
            }
        }

        // 广度优先遍历并从各根标记可达堆对象
        while let Some(v) = worklist.pop() {
            crate::gc::mark_value(&v, marked, &mut worklist);
        }
    }

    pub fn register_builtin_module(&mut self, name: &str, module: Rc<RefCell<ModuleObject>>) {
        self.builtin_modules.insert(name.to_string(), module);
    }

    pub(crate) fn snapshot_for_module_init(&self) -> ModuleInitSnapshot {
        ModuleInitSnapshot {
            globals: self.globals.clone(),
            functions: self.functions.clone(),
            macros: self.macros.clone(),
            struct_defs: self.struct_defs.clone(),
            const_names: self.const_names.clone(),
            module_init_exports: self.module_init_exports.clone(),
            code: self.code.clone(),
            pc: self.pc,
            script_global_names: self.script_global_names.clone(),
            script_globals: self.script_globals.clone(),
        }
    }

    pub(crate) fn begin_module_init(
        &mut self,
        snap: &ModuleInitSnapshot,
        package_name: &str,
    ) -> Rc<RefCell<HashMap<String, Value>>> {
        self.globals.clear();
        self.const_names.clear();
        self.pending_const.clear();
        self.op_clear();
        self.locals_stack.clear();
        self.name_to_slot.clear();
        self.func_stack.clear();
        self.func_frames.clear();
        self.active_line_map = Rc::new(Vec::new());
        self.active_column_map = Rc::new(Vec::new());
        self.try_stack.clear();
        self.active_exception = None;
        self.iterators.clear();

        builtins::install_globals(self);
        type_registry::install_core_types(self);
        self.globals.insert(
            "__package__".into(),
            Value::Text(package_name.to_string()),
        );

        let exports = Rc::new(RefCell::new(HashMap::new()));
        self.module_init_exports = Some(exports.clone());

        self.functions = snap.functions.clone();
        self.macros = snap.macros.clone();
        self.struct_defs = snap.struct_defs.clone();
        exports
    }

    pub(crate) fn finish_module_init(
        &mut self,
        snap: ModuleInitSnapshot,
        new_functions: HashMap<String, Rc<FunctionObject>>,
        new_macros: HashMap<String, Rc<MacroObject>>,
        new_struct_defs: HashMap<String, Rc<crate::value::StructDef>>,
    ) {
        self.globals = snap.globals;
        self.functions = snap.functions;
        self.macros = snap.macros;
        self.struct_defs = snap.struct_defs;
        self.const_names = snap.const_names;
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
        self.active_line_map = Rc::new(Vec::new());
        self.active_column_map = Rc::new(Vec::new());
        self.try_stack.clear();
        self.active_exception = None;
        self.iterators.clear();
        self.functions.extend(new_functions);
        self.macros.extend(new_macros);
        self.struct_defs.extend(new_struct_defs);
        self.script_global_names = snap.script_global_names;
        self.script_globals = snap.script_globals;
    }

    pub fn load_program(&mut self, program: CompiledProgram) -> Result<()> {
        // 先做一次性结构校验：畸形字节码在进入主循环前就干净报错，
        // 让热路径的安全索引有了显式保证（纵深防御）。
        validate_hot_bytecode(&program.hot)?;
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

        self.code = Rc::new(program.code);
        self.hot_ops = program.hot.ops.clone();
        self.hot_args = program.hot.args.clone();
        self.active_line_map = Rc::new(program.line_map);
        self.active_column_map = Rc::new(program.column_map);
        self.struct_defs.extend(program.struct_defs);
        self.enum_defs.extend(program.enum_defs);
        self.variant_defs.extend(program.variant_defs);
        self.functions.extend(program.functions);
        self.macros.extend(program.macros);
        self.overload_tables.extend(program.overload_tables);
        self.protocols.extend(program.protocols);
        self.globals.insert(
            "__package__".into(),
            Value::Text("__main__".into()),
        );
        self.pc = 0;
        self.init_script_globals(program.global_names);
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
    }

    pub(crate) fn snapshot_module_global_env(&self) -> ModuleGlobalEnv {
        ModuleGlobalEnv {
            global_names: self.script_global_names.clone(),
            globals: self
                .globals
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn init_script_globals(&mut self, names: Vec<String>) {
        self.script_global_names = names;
        self.script_globals = self
            .script_global_names
            .iter()
            .map(|name| {
                self.globals
                    .get(name)
                    .cloned()
                    .unwrap_or(Value::None)
            })
            .collect();
    }

    fn sync_script_global(&mut self, idx: usize, val: Value) {
        if idx < self.script_globals.len() {
            self.script_globals[idx] = val.clone();
            if let Some(name) = self.script_global_names.get(idx) {
                if let Some(Value::Cell(cell)) = self.globals.get(name.as_str()) {
                    *cell.borrow_mut() = val;
                } else {
                    self.globals.insert(name.clone(), val);
                }
            }
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
            let Some(name) = env.global_names.get(idx) else {
                return Err(RuntimeError::msg(format!(
                    "internal: LoadGlobal({idx}) out of range for function global table (len {})",
                    env.global_names.len()
                )));
            };
            // 优先用模块快照；否则回退到活动 globals，以便 REPL 前向引用
            //（先定义 `a` 再定义 `b`）仍能解析，并使 LoadGlobal 下标绑定到
            // 函数编译期的名字，即使后来 `load_program` 替换了 `script_global_names`。
            if let Some(v) = env.globals.get(name.as_str()) {
                return Ok(match v {
                    Value::Cell(c) => c.borrow().clone(),
                    other => other.clone(),
                });
            }
            return match self.globals.get(name.as_str()) {
                Some(Value::Cell(c)) => Ok(c.borrow().clone()),
                Some(v) => Ok(v.clone()),
                None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
            };
        }
        let Some(name) = self.script_global_names.get(idx) else {
            return Err(RuntimeError::msg(format!(
                "internal: LoadGlobal({idx}) out of range for script global table (len {})",
                self.script_global_names.len()
            )));
        };
        match self.globals.get(name.as_str()) {
            Some(Value::Cell(c)) => Ok(c.borrow().clone()),
            Some(v) => Ok(v.clone()),
            None => Err(RuntimeError::name_err(format!("undefined name: {name}"))),
        }
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
    fn jump_to_pc(&mut self, pc: usize) {
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
            // 覆盖旧槽：Heap 会被 Drop，Int/Bool/Empty 无开销。
            // SAFETY: sp < stack.len() 刚检查。
            self.stack[sp] = v;
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
            self.stack[sp] = StackVal::Int(n);
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
            self.stack[sp] = StackVal::Bool(b);
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
        // SAFETY: sp < stack_sp（旧值）<= stack.len()。
        std::mem::replace(&mut self.stack[sp], StackVal::Empty)
    }

    /// 二元 Int 运算就地完成：读 TOS/TOS1，写回 TOS1，sp-=1。成功返回 true。
    #[inline(always)]
    fn binop_ints_inplace(&mut self, f: impl FnOnce(i64, i64) -> Option<i64>) -> bool {
        let sp = self.stack_sp;
        if sp < 2 {
            return false;
        }
        // SAFETY: sp >= 2 且 sp <= stack.len()，故 sp-2、sp-1 合法。
        // 先把 i64 拷出来（Copy），结束对 stack 的不可变借用，再写回，避免借用冲突。
        let (xr, yr) = match (&self.stack[sp - 2], &self.stack[sp - 1]) {
            (StackVal::Int(x), StackVal::Int(y)) => (*x, *y),
            _ => return false,
        };
        if let Some(r) = f(xr, yr) {
            self.stack[sp - 2] = StackVal::Int(r);
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
        // SAFETY: sp >= 2 且 sp <= stack.len()。
        let (xr, yr) = match (&self.stack[sp - 2], &self.stack[sp - 1]) {
            (StackVal::Int(x), StackVal::Int(y)) => (*x, *y),
            _ => return false,
        };
        self.stack[sp - 2] = StackVal::Bool(f(xr, yr));
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
        if self.lw_depth >= max_call_depth() {
            self.set_hot_error(RuntimeError::msg("RecursionError: maximum recursion depth exceeded"));
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
            self.lw_slots[idx] = val;
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
    fn load_fast_sv(&self, slot: usize) -> StackVal {
        if self.lw_depth != 0 {
            let idx = self.lw_base + slot;
            if idx < self.lw_sp {
                // SAFETY: idx < lw_sp <= lw_slots.len()。
                return self.lw_slots[idx].copy_imm();
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
        for i in base..self.lw_sp {
            self.lw_slots[i] = StackVal::Empty;
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
            _ => self.dispatch_binary_arith(&av, &bv, "__add__", "__radd__", |x, y| x.add(y))?,
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
            _ => self.dispatch_binary_arith(&av, &bv, "__sub__", "__rsub__", |x, y| x.sub(y))?,
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
                let val = Value::List(Rc::new(RefCell::new(out)));
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
        let b = self.pop_hot();
        let a = self.pop_hot();
        match (a, b) {
            (StackVal::Int(x), StackVal::Int(y)) => match x.checked_mul(y) {
                Some(s) => self.op_push(StackVal::Int(s)),
                None => self.push_value(Value::Num(Num::from_bigint(
                    num_bigint::BigInt::from(x) * num_bigint::BigInt::from(y),
                ))),
            },
            (a, b) => {
                let av = a.into_value();
                let bv = b.into_value();
                let result = match (&av, &bv) {
                    (Value::Num(_), Value::Num(_)) => av.mul(&bv)?,
                    _ => self.dispatch_binary_arith(&av, &bv, "__mul__", "__rmul__", |x, y| {
                        x.mul(y)
                    })?,
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
            _ => self.dispatch_binary_arith(&a, &b, "__truediv__", "__rtruediv__", |x, y| {
                x.div(y)
            })?,
        };
        self.push_value(result);
        Ok(())
    }

    #[inline]
    fn exec_cmp_num(&mut self, pred: impl Fn(std::cmp::Ordering) -> bool) -> Result<()> {
        let b = self.pop_hot();
        let a = self.pop_hot();
        let result = match (&a, &b) {
            (StackVal::Int(x), StackVal::Int(y)) => pred(x.cmp(y)),
            _ => {
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
                    _ => return Err(RuntimeError::type_err("comparison requires num")),
                }
            }
        };
        self.op_push(StackVal::Bool(result));
        Ok(())
    }

    #[inline]
    fn exec_eq_num(&mut self) -> Result<()> {
        let b = self.pop_hot();
        let a = self.pop_hot();
        let result = match (&a, &b) {
            (StackVal::Int(x), StackVal::Int(y)) => x == y,
            _ => {
                let av = a.to_value();
                let bv = b.to_value();
                self.dispatch_eq(&av, &bv)?
            }
        };
        self.op_push(StackVal::Bool(result));
        Ok(())
    }

    #[inline]
    fn exec_ne_num(&mut self) -> Result<()> {
        let b = self.pop_hot();
        let a = self.pop_hot();
        let result = match (&a, &b) {
            (StackVal::Int(x), StackVal::Int(y)) => x != y,
            _ => {
                let av = a.to_value();
                let bv = b.to_value();
                !self.dispatch_eq(&av, &bv)?
            }
        };
        self.op_push(StackVal::Bool(result));
        Ok(())
    }

    /// 紧凑 u8 热分派。Int 热路径就地改栈，轻量 Ret 不搬返回值。
    #[inline(always)]
    fn dispatch_hot_u8(&mut self, ops: &[u8], args: &[i64], pc: usize) -> HotFlow {
        use crate::hot_code::*;
        // SAFETY（已由外层 `'hot` 循环 `if pc >= code_len { break }` 保证）：
        // 进入本函数时 `pc < ops.len()`，且 `HotCode::encode` 保证 `ops.len() == args.len()`。
        // 此处用安全索引替代 `get_unchecked`：边界已隐式满足，分支可被优化器消除。
        let op = ops[pc];
        let arg = args[pc];
        match op {
            H_PUSH_SMALL => {
                self.pc = pc + 1;
                self.op_push_int(arg);
                HotFlow::Cont
            }
            H_LOAD_FAST => {
                self.pc = pc + 1;
                // 轻量帧：绝大多数为 Int，避免 copy_imm 总匹配。
                if self.lw_depth != 0 {
                    let idx = self.lw_base + (arg as usize);
                    if idx < self.lw_sp {
                        // SAFETY: idx < lw_sp <= lw_slots.len()（lw_sp 仅在 resize 后推进）。
                        return match &self.lw_slots[idx] {
                            StackVal::Int(n) => {
                                self.op_push_int(*n);
                                HotFlow::Cont
                            }
                            other => {
                                self.op_push(other.copy_imm());
                                HotFlow::Cont
                            }
                        }
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
                    (Value::Num(_), Value::Num(_)) => compare_num(&av, &bv).map(|c| c <= 0),
                    _ => self.dispatch_compare(&av, &bv, "__le__", |x, y| {
                        Ok(compare_num(x, y)? <= 0)
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
                if let Err(e) = self.exec_cmp_num(|c| c == std::cmp::Ordering::Less) {
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
                if let Err(e) = self.exec_cmp_num(|c| c == std::cmp::Ordering::Greater) {
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
                if let Err(e) = self.exec_cmp_num(|c| c != std::cmp::Ordering::Less) {
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
                let b = self.pop_hot();
                let a = self.pop_hot();
                self.op_push(a);
                self.op_push(b);
                if let Err(e) = self.exec_eq_num() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_NE => {
                self.pc = pc + 1;
                if self.cmp_ints_inplace(|x, y| x != y) {
                    return HotFlow::Cont;
                }
                let b = self.pop_hot();
                let a = self.pop_hot();
                self.op_push(a);
                self.op_push(b);
                if let Err(e) = self.exec_ne_num() {
                    self.set_hot_error(e);
                    return HotFlow::Fail;
                }
                HotFlow::Cont
            }
            H_GOTO => {
                self.pc = arg as usize;
                HotFlow::Cont
            }
            H_GOTO_IF_NOT => {
                // 常见：比较结果 Bool 在 TOS
                self.pc = pc + 1;
                let sp = self.stack_sp;
                if sp > 0 {
                    // SAFETY: sp > 0 且 sp <= stack.len()，故 sp-1 合法。
                    match &self.stack[sp - 1] {
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
                    match &self.stack[sp - 1] {
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
                    if !func.lightweight {
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
            _ => HotFlow::Cold,
        }
    }

    #[inline(always)]
    fn call_self_lw1(&mut self, entry_pc: usize) {
        if self.lw_depth >= max_call_depth() {
            self.set_hot_error(RuntimeError::msg("RecursionError: maximum recursion depth exceeded"));
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
        match self.run_interpreter(None)? {
            Some(v) => Ok(v),
            None => Ok(self.stack_top()),
        }
    }

    fn run_interpreter(&mut self, until_depth: Option<usize>) -> Result<Option<Value>> {
        self.ensure_op_stack(256);
        'outer: loop {
            // 仅在外层刷新切片；CallSelf/Cont 热路径不碰 Rc / ptr_eq。
            let hot_ops = Rc::clone(&self.hot_ops);
            let hot_args = Rc::clone(&self.hot_args);
            let ops = hot_ops.as_ref();
            let args = hot_args.as_ref();
            let code_len = ops.len();

            'hot: loop {
                let pc = self.pc;
                if pc >= code_len {
                    break 'hot;
                }
                match self.dispatch_hot_u8(ops, args, pc) {
                    HotFlow::Cont => continue 'hot,
                    HotFlow::Fail => {
                        self.hot_failed = false;
                        let e = self
                            .hot_error
                            .take()
                            .unwrap_or_else(|| RuntimeError::msg("hot path error"));
                        match self.handle_or_promote_error(&e)? {
                            true => continue 'outer,
                            false => {
                                self.record_error_stack();
                                self.unwind_user_calls_on_error()?;
                                return Err(self.finalize_runtime_error(e));
                            }
                        }
                    }
                    HotFlow::PendingRet => {
                        let (leave, result_sv) = self
                            .pending_ret
                            .take()
                            .expect("pending_ret set under HotFlow::PendingRet (theoretically unreachable)");
                        let result = result_sv.into_value();
                        if let Some(ret) = self.complete_user_return_instruction(leave, result)? {
                            return Ok(Some(ret));
                        }
                        if until_depth.is_some_and(|d| self.user_call_frames.len() == d) {
                            return Ok(self.op_last_value());
                        }
                        continue 'outer;
                    }
                    HotFlow::Cold => {
                        if let Err(e) = self.step() {
                            match self.handle_or_promote_error(&e)? {
                                true => continue 'outer,
                                false => {
                                    self.record_error_stack();
                                    self.unwind_user_calls_on_error()?;
                                    return Err(self.finalize_runtime_error(e));
                                }
                            }
                        }
                        continue 'outer;
                    }
                }
            }

            // pc 已越界（pc >= code_len）
            if until_depth.is_none() && self.user_call_frames.is_empty() {
                return Ok(self.op_last_value());
            }
            if until_depth.is_some_and(|d| self.user_call_frames.len() <= d) {
                return Ok(self.op_last_value());
            }
            if !self.user_call_frames.is_empty() {
                if let Some(ret) = self.complete_user_return_instruction(false, Value::None)? {
                    return Ok(Some(ret));
                }
                continue 'outer;
            }
            break;
        }
        Ok(None)
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

    /// 未捕获脚本异常时，用异常种类与文案覆盖原始宿主错误。
    fn finalize_runtime_error(&self, fallback: RuntimeError) -> RuntimeError {
        if let Some(exc) = self.active_exception.as_ref() {
            let kind = exceptions::kind_of_value(exc).unwrap_or(crate::error::ExceptionKind::Runtime);
            RuntimeError::typed(kind, exceptions::format_uncaught(exc))
        } else {
            fallback
        }
    }

    fn dispatch_to_handler(&mut self) -> Result<bool> {
        let Some(frame) = self.try_stack.last().cloned() else {
            return Ok(false);
        };
        // 先展开轻量 CallSelf，再展开完整用户调用帧，恢复 try 所在代码对象。
        while self.fast_ret_sp > frame.fast_ret_sp {
            self.fast_ret_sp -= 1;
            self.pop_lightweight_frame();
        }
        while self.user_call_frames.len() > frame.user_call_depth {
            let Some(ucf) = self.user_call_frames.pop() else {
                break;
            };
            self.leave_scope();
            if ucf.func.track_frames {
                self.func_frames.pop();
            }
            if ucf.pushed_func_stack {
                self.func_stack.pop();
            }
            self.restore_user_call_frame(ucf)?;
        }
        self.op_truncate(frame.stack_sp);
        self.iterators.truncate(frame.iterators_len);
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
        self.active_exception = Some(exc);
        if self.dispatch_to_handler()? {
            return Ok(());
        }
        let (kind, msg) = match self.active_exception.as_ref() {
            Some(exc) => (
                exceptions::kind_of_value(exc).unwrap_or(crate::error::ExceptionKind::Runtime),
                exceptions::format_uncaught(exc),
            ),
            None => (crate::error::ExceptionKind::Runtime, String::new()),
        };
        Err(RuntimeError::typed(kind, msg))
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
            I::Pow | I::PowNumNum => StepAction::Pow,
            I::Neg => StepAction::Neg,
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
            I::Call { argc } => StepAction::Call { argc: *argc },
            I::CallSelf { argc } => StepAction::CallSelf { argc: *argc },
            I::CallList => StepAction::CallList,
            I::CallEx => StepAction::CallEx,
            I::MacroCall { argc } => StepAction::MacroCall { argc: *argc },
            I::ListAppend => StepAction::ListAppend,
            I::ListExtend => StepAction::ListExtend,
            I::DictSet => StepAction::DictSet,
            I::SetAdd => StepAction::SetAdd,
            I::Ret => StepAction::Ret,
            I::RetFast(_) => StepAction::RetFast,
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
            I::TypeCheck(ty) => StepAction::TypeCheck(ty.clone()),
            I::FindMod(name) => StepAction::FindMod(name.clone()),
            I::RegisterExport(name) => StepAction::RegisterExport(name.clone()),
        }
    }

    fn step(&mut self) -> Result<()> {
        let pc = self.pc;
        self.pc += 1;
        let action = Self::decode_step_action(&self.code[pc]);
        self.run_step_action(action)
    }

    fn run_step_action(&mut self, action: StepAction) -> Result<()> {
        match action {
            StepAction::Push(v) => self.push_value(v),
            StepAction::PushSmall(n) => self.push_int(n),
            StepAction::Pop => {
                if self.stack_sp > 0 { self.op_pop(); }
            }
            StepAction::Add => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match (&a, &b) {
                    (Value::Num(Num::Small(x)), Value::Num(Num::Small(y))) => Value::Num(
                        x.checked_add(*y)
                            .map(Num::Small)
                            .unwrap_or_else(|| {
                                Num::from_bigint(
                                    num_bigint::BigInt::from(*x)
                                        + num_bigint::BigInt::from(*y),
                                )
                            }),
                    ),
                    (Value::Num(_), Value::Num(_)) => a.add(&b)?,
                    _ => self.dispatch_binary_arith(&a, &b, "__add__", "__radd__", |x, y| {
                        x.add(y)
                    })?,
                };
                self.push_value(result);
            }
            StepAction::Sub => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match (&a, &b) {
                    (Value::Num(Num::Small(x)), Value::Num(Num::Small(y))) => Value::Num(
                        x.checked_sub(*y)
                            .map(Num::Small)
                            .unwrap_or_else(|| {
                                Num::from_bigint(
                                    num_bigint::BigInt::from(*x)
                                        - num_bigint::BigInt::from(*y),
                                )
                            }),
                    ),
                    (Value::Num(_), Value::Num(_)) => a.sub(&b)?,
                    _ => self.dispatch_binary_arith(&a, &b, "__sub__", "__rsub__", |x, y| {
                        x.sub(y)
                    })?,
                };
                self.push_value(result);
            }
            StepAction::Mul => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.mul(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__mul__", "__rmul__", |x, y| {
                        x.mul(y)
                    })?
                };
                self.push_value(result);
            }
            StepAction::Div => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.div(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__div__", "__rdiv__", |x, y| {
                        x.div(y)
                    })?
                };
                self.push_value(result);
            }
            StepAction::Pow => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    a.pow(&b)?
                } else {
                    self.dispatch_binary_arith(&a, &b, "__pow__", "__rpow__", |x, y| {
                        x.pow(y)
                    })?
                };
                self.push_value(result);
            }
            StepAction::Neg => {
                let a = self.pop()?;
                let result = self.dispatch_neg(&a)?;
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
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    compare_num(&a, &b)? < 0
                } else {
                    self.dispatch_compare(&a, &b, "__lt__", |x, y| {
                        Ok(compare_num(x, y)? < 0)
                    })?
                };
                self.push_bool(result);
            }
            StepAction::Le => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match (&a, &b) {
                    (Value::Num(Num::Small(x)), Value::Num(Num::Small(y))) => x <= y,
                    (Value::Num(_), Value::Num(_)) => compare_num(&a, &b)? <= 0,
                    _ => self.dispatch_compare(&a, &b, "__le__", |x, y| {
                        Ok(compare_num(x, y)? <= 0)
                    })?,
                };
                self.push_bool(result);
            }
            StepAction::Gt => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    compare_num(&a, &b)? > 0
                } else {
                    self.dispatch_compare(&a, &b, "__gt__", |x, y| {
                        Ok(compare_num(x, y)? > 0)
                    })?
                };
                self.push_bool(result);
            }
            StepAction::Ge => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = if matches!((&a, &b), (Value::Num(_), Value::Num(_))) {
                    compare_num(&a, &b)? >= 0
                } else {
                    self.dispatch_compare(&a, &b, "__ge__", |x, y| {
                        Ok(compare_num(x, y)? >= 0)
                    })?
                };
                self.push_bool(result);
            }
            StepAction::In => {
                let container = self.pop()?;
                let item = self.pop()?;
                let contained = self.value_contains(&container, &item)?;
                self.push_bool(contained);
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
                if let Some(name) = self.script_global_names.get(idx) {
                    if self.const_names.contains(name) {
                        return Err(RuntimeError::msg(format!(
                            "cannot assign to const binding: {name}"
                        )));
                    }
                }
                self.sync_script_global(idx, val);
                if let Some(name) = self.script_global_names.get(idx).cloned() {
                    self.finalize_const_init(&name);
                }
            }
            StepAction::NewVar { name, is_const } => {
                if is_const {
                    self.pending_const.insert(name.clone());
                }
                if self.locals_stack.is_empty() {
                    if !self.globals.contains_key(name.as_str()) {
                        self.globals.insert(name.clone(), Value::None);
                    }
                } else {
                    let frame = self.locals_stack.len() - 1;
                    let names = self.scope_name_map_mut(frame);
                    if !names.contains_key(name.as_str()) {
                        let slot = names.len();
                        names.insert(name.clone(), slot);
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
                let val = self.pop().map_err(|_| {
                    RuntimeError::msg("internal: BindFast with empty stack")
                })?;
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
                if !self.user_call_deferred && self.active_exception.is_none() {
                    self.push_value(result);
                }
            }
            StepAction::CallSelf { argc } => {
                let func = self
                    .func_stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::msg("CallSelf outside function"))?;
                if func.lightweight {
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
                if !self.user_call_deferred && self.active_exception.is_none() {
                    self.push_value(result);
                }
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
                if !self.user_call_deferred && self.active_exception.is_none() {
                    self.push_value(result);
                }
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
            StepAction::RetFast => {
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
                let val = Value::List(Rc::new(RefCell::new(elems)));
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
                let val = Value::Dict(Rc::new(RefCell::new(map)));
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
                let val = Value::Set(Rc::new(RefCell::new(set)));
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
                index_set(self, &obj, &idx, val)?;
                self.push_none();
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
                self.push_none();
            }
            StepAction::DelIndex => {
                let idx = self.pop()?;
                let obj = self.pop()?;
                del_index(self, &obj, &idx)?;
                self.push_none();
            }
            StepAction::DelName(name) => {
                self.delete_name(&name)?;
                self.push_none();
            }
            StepAction::DelAttr(field) => {
                let obj = self.pop()?;
                del_attr(self, &obj, &field)?;
                self.push_none();
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
                self.push_none();
            }
            StepAction::IterNew => {
                let obj = self.pop()?;
                let state = crate::value::value_to_iterable(&obj)?;
                let rc = Rc::new(RefCell::new(state));
                self.gc.track_iter(&rc);
                self.iterators.push(ActiveIter {
                    state: rc,
                });
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
                        self.push_value(val);
                        self.push_bool(true);
                    }
                    Ok(None) => {
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
                let frame = self
                    .try_stack
                    .last()
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
                    .map(|e| exceptions::struct_is_a(self, e, &type_name))
                    .unwrap_or(false);
                self.push_bool(matched);
            }
            StepAction::IsList => {
                let v = self.pop()?;
                self.push_bool(matches!(v, Value::List(_)));
            }
            StepAction::ListLen => {
                let v = self.pop()?;
                let n = match &v {
                    Value::List(lst) => lst.borrow().len(),
                    _ => return Err(RuntimeError::type_err("ListLen requires list")),
                };
                self.push_int(n as i64);
            }
            StepAction::IsInstance(type_name) => {
                let v = self.pop()?;
                self.push_bool(types::instance_is_a(
                    self, &v, &type_name,
                ));
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
                self.push_value(Value::List(Rc::new(RefCell::new(rest))));
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
                        let msg = self
                            .active_exception
                            .as_ref()
                            .map(exceptions::format_uncaught)
                            .unwrap_or_default();
                        Err(RuntimeError::msg(msg))
                    }
                });
            }
            StepAction::TypeCheck(ty) => {
                let val = self.pop()?;
                if let Some(msg) = types::type_check_error(self, &val, &ty) {
                    return self.raise_type_error(msg);
                }
                types::seal_container_contract(self, &val, &ty);
                self.push_value(val);
            }
            StepAction::FindMod(name) => {
                let module = module::find_module(self, &name)?;
                self.push_value(module);
            }
            StepAction::RegisterExport(name) => {
                if let Some(ref exports) = self.module_init_exports {
                    let val = if let Some(v) = self.load_script_global_by_name(&name) {
                        v
                    } else {
                        self.load_name(&name)?
                    };
                    exports.borrow_mut().insert(name.clone(), val);
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

    pub(crate) fn register_enum_def(&mut self, name: String, def: Rc<crate::value::EnumDef>) {
        for (func_name, func) in crate::enum_variant::builtin_enum_method_entries(&name, &def) {
            self.functions.insert(func_name, func);
        }
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
        match self.globals.get(name) {
            Some(Value::Cell(c)) => Ok(c.borrow().clone()),
            Some(v) => Ok(v.clone()),
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
            .cloned()
            .ok_or_else(|| RuntimeError::name_err(format!("undefined name: {name}")))
    }

    pub(crate) fn upgrade_binding_to_cell(&mut self, name: &str) -> Result<Rc<RefCell<Value>>> {
        if let Value::Cell(cell) = self.get_binding(name)? {
            return Ok(cell);
        }
        let val = self.load_name(name)?;
        let cell = Rc::new(RefCell::new(val));
        self.gc.track_cell(&cell);
        self.set_binding_raw(name, Value::Cell(cell.clone()))?;
        Ok(cell)
    }

    fn set_binding_raw(&mut self, name: &str, val: Value) -> Result<()> {
        for i in (0..self.name_to_slot.len()).rev() {
            if let Some(map) = &self.name_to_slot[i] {
                if let Some(slot) = map.get(name) {
                    let slot = *slot;
                    if slot >= self.locals_stack[i].len() {
                        self.locals_stack[i].resize(slot + 1, Value::None);
                    }
                    self.locals_stack[i][slot] = val;
                    return Ok(());
                }
            }
        }
        self.globals.insert(name.to_string(), val);
        Ok(())
    }

    pub(crate) fn load_macro(&self, name: &str) -> Result<Value> {
        if let Some(m) = self.macros.get(name) {
            return Ok(Value::Macro(m.clone()));
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
            self.const_names.insert(name.to_string());
        }
    }

    /// 拒绝向以 `const` 绑定的槽执行 `StoreFast`（热/冷路径共用）。
    #[inline]
    fn reject_const_fast_store(&self, slot: usize) -> Result<()> {
        if self.const_names.is_empty() {
            return Ok(());
        }
        if let Some(map) = self.name_to_slot.last().and_then(|m| m.as_ref()) {
            for (name, &s) in map.iter() {
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
        if let Some(Value::Cell(cell)) = self.globals.get(name) {
            *cell.borrow_mut() = val.clone();
        } else {
            self.globals.insert(name.to_string(), val.clone());
        }
        if let Some(idx) = self.script_global_names.iter().position(|n| n == name) {
            if idx < self.script_globals.len() {
                self.script_globals[idx] = val;
            }
        }
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
        if self.globals.remove(name).is_some() {
            if let Some(idx) = self.script_global_names.iter().position(|n| n == name) {
                if idx < self.script_globals.len() {
                    self.script_globals[idx] = Value::None;
                }
            }
            return Ok(());
        }
        Err(RuntimeError::name_err(format!("name not found: {name}")))
    }

    pub(crate) fn struct_has_method(&self, obj: &Value, method: &str) -> bool {
        let Value::Struct(s) = obj else {
            return false;
        };
        self.functions
            .contains_key(&format!("{}.{}", s.def.name, method))
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

    fn call_struct_method(
        &mut self,
        obj: &Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value> {
        let Value::Struct(s) = obj else {
            return Err(RuntimeError::msg("expected struct instance"));
        };
        let method_name = format!("{}.{}", s.def.name, method);
        let func = self
            .functions
            .get(&method_name)
            .cloned()
            .ok_or_else(|| RuntimeError::attr_err(format!("no method {method_name}")))?;
        let mut full_args = vec![obj.clone()];
        full_args.extend(args);
        self.call_user_function(func, full_args)
    }

    pub(crate) fn call_method(&mut self, obj: &Value, method: &str, args: Vec<Value>) -> Result<Value> {
        let method_val = get_attr(self, obj, method)?;
        let mut call_args = vec![obj.clone()];
        call_args.extend(args);
        self.call_value(method_val, call_args)
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
        if !kwargs.is_empty()
            && !matches!(
                callee,
                Value::Function(_) | Value::GenericFunction(_)
            )
        {
            return Err(RuntimeError::type_err(
                "keyword arguments only supported for user functions",
            ));
        }
        let args = positional;
        match callee {
            Value::Struct(ref _s) if self.struct_has_method(&callee, "__call__") => {
                self.call_struct_method(&callee, "__call__", args)
            }
            Value::Builtin(f) => f(self, &args),
            Value::Function(func) => {
                let bound = self.bind_call_arguments(&func, args, kwargs)?;
                self.setup_user_call(func, bound, false)?;
                self.user_call_deferred = true;
                Ok(Value::None)
            }
            Value::GenericFunction(template) => {
                let type_args = infer_generic_type_args_from_values(&template, &args)?;
                let func = specialize_generic_runtime(self, &template, type_args)?;
                let bound = self.bind_call_arguments(&func, args, kwargs)?;
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
                    args.into_iter().next().expect("variant arg count checked above (theoretically unreachable)"),
                )
            }
            Value::TypeRef(ref type_name) => {
                let argc = args.len();
                if let Some(result) = type_registry::call_primitive_ctor(self, type_name, args) {
                    return result;
                }
                Err(RuntimeError::type_err(format!(
                    "TypeError: {type_name} is not callable with {argc} argument(s)"
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
            let list = Value::List(Rc::new(RefCell::new(rest)));
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
            let dict = Value::Dict(Rc::new(RefCell::new(kwargs)));
            self.track_value(&dict);
            bound[ki] = Some(dict);
        } else if !kwargs.is_empty() {
            let names: Vec<String> = kwargs
                .keys()
                .map(value_key_to_display)
                .collect();
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
                let list = Value::List(Rc::new(RefCell::new(Vec::new())));
                self.track_value(&list);
                bound[pi] = Some(list);
            } else if param.is_kwvariadic {
                let dict = Value::Dict(Rc::new(RefCell::new(DictMap::new())));
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
            .map(|v| {
                v.ok_or_else(|| RuntimeError::msg("internal: unbound argument slot"))
            })
            .collect()
    }
    fn resolve_macro_args(
        &self,
        mac: &MacroObject,
        args: Vec<Value>,
    ) -> Result<Vec<Value>> {
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
            resolved[vi] = Value::List(Rc::new(RefCell::new(packed)));
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
            for (i, arg) in args.into_iter().enumerate() {
                resolved[i] = arg;
            }
            Ok(resolved)
        }
    }

    fn call_macro(&mut self, mac: Rc<MacroObject>, args: Vec<Value>) -> Result<Value> {
        for (i, arg) in args.iter().enumerate() {
            if !matches!(arg, Value::RuntimeAst(_)) {
                return Err(RuntimeError::type_err(format!(
                    "macro argument {} must be AST (frozen at parse time), got {}",
                    i,
                    arg.type_name()
                )));
            }
        }
        let resolved = self.resolve_macro_args(&mac, args)?;

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

    fn run_macro_body(&mut self, mac: Rc<MacroObject>, args: Vec<Value>) -> Result<Value> {
        self.enter_scope();
        for (i, param) in mac.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::None);
            if let Some(locals) = self.locals_stack.last_mut() {
                locals.insert(i, val);
            }
            if !self.name_to_slot.is_empty() {
                let frame = self.locals_stack.len() - 1;
                self.scope_name_map_mut(frame)
                    .insert(param.name.clone(), i);
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
            Ok(v) => v.unwrap_or_else(|| self.stack_top()),
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
            return r.map(|v| v.is_truthy()).unwrap_or(false);
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
                if let Value::List(v) = self.iterable_values(container)? {
                    for elem in v.borrow().iter() {
                        if self.dispatch_eq(elem, item)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn iterable_values(&mut self, val: &Value) -> Result<Value> {
        if let Some(r) = self.try_call_magic(val, "__iter__", vec![]) {
            let iter_val = r?;
            if let Value::List(l) = iter_val {
                return Ok(Value::List(l));
            }
        }
        match val {
            Value::List(l) => Ok(Value::List(l.clone())),
            Value::Text(s) => Ok(Value::List(Rc::new(RefCell::new(
                s.chars().map(|c| Value::Text(c.to_string())).collect(),
            )))),
            _ => Err(RuntimeError::type_err("object is not iterable")),
        }
    }

    pub(crate) fn get_or_create_dispatch(&mut self, name: &str) -> Rc<RefCell<DispatchTable>> {
        if let Some(t) = self.globals.get(name).and_then(|v| {
            if let Value::Dispatch(t) = v {
                Some(t.clone())
            } else {
                None
            }
        }) {
            return t;
        }
        let table = Rc::new(RefCell::new(DispatchTable {
            name: name.to_string(),
            handlers: Rc::new(RefCell::new(Vec::new())),
        }));
        self.globals
            .insert(name.to_string(), Value::Dispatch(table.clone()));
        table
    }

    pub(crate) fn get_or_create_convert(&mut self, type_name: &str) -> Rc<RefCell<DispatchTable>> {
        let key = format!("__convert__:{type_name}");
        self.convert_tables
            .entry(key.clone())
            .or_insert_with(|| {
                Rc::new(RefCell::new(DispatchTable {
                    name: key,
                    handlers: Rc::new(RefCell::new(Vec::new())),
                }))
            })
            .clone()
    }

    fn call_dispatch(&mut self, table: &Rc<RefCell<DispatchTable>>, args: Vec<Value>) -> Result<Value> {
        enum DispatchTarget {
            Function(Rc<FunctionObject>),
            Builtin(BuiltinFn),
        }

        let handlers = table.borrow().handlers.borrow().clone();
        let mut best: Option<(usize, usize, DispatchTarget)> = None;
        for (idx, handler_val) in handlers.iter().enumerate() {
            let (score, target) = match handler_val {
                Value::Function(func) => {
                    let Some(score) = types::dispatch_match_score(self, func, &args) else {
                        continue;
                    };
                    (score, DispatchTarget::Function(func.clone()))
                }
                Value::Builtin(f) => (usize::MAX, DispatchTarget::Builtin(f.clone())),
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
                DispatchTarget::Function(func) => self.call_user_function(func, args),
                DispatchTarget::Builtin(f) => f(self, &args),
            };
        }
        let table_name = table.borrow().name.clone();
        if let Some(type_name) = table_name.strip_prefix("__convert__:") {
            let src = args.get(1).map(|v| v.type_name()).unwrap_or("?");
            return Err(RuntimeError::type_err(format!(
                "TypeError: no matching __convert__ handler for {type_name} from {src}"
            )));
        }
        Err(RuntimeError::type_err(format!(
            "TypeError: no matching __dispatch__ for {table_name}"
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
            pc: self.pc,
            stack: self.stack.get(..self.stack_sp).unwrap_or(&[]).to_vec(),
            functions: self.functions.clone(),
            macros: self.macros.clone(),
            struct_defs: self.struct_defs.clone(),
            enum_defs: self.enum_defs.clone(),
            variant_defs: self.variant_defs.clone(),
            script_global_names: self.script_global_names.clone(),
            script_globals: self.script_globals.clone(),
        }
    }

    pub(crate) fn restore_eval_snapshot(&mut self, snap: EvalSnapshot) {
        self.globals = snap.globals;
        self.locals_stack = snap.locals_stack;
        self.name_to_slot = snap.name_to_slot;
        self.code = snap.code;
        let hot = crate::hot_code::HotCode::encode(&self.code);
        self.hot_ops = hot.ops;
        self.hot_args = hot.args;
        self.pc = snap.pc;
        self.stack = snap.stack;
        self.stack_sp = self.stack.len();
        self.functions = snap.functions;
        self.macros = snap.macros;
        self.struct_defs = snap.struct_defs;
        self.enum_defs = snap.enum_defs;
        self.variant_defs = snap.variant_defs;
        self.script_global_names = snap.script_global_names;
        self.script_globals = snap.script_globals;
    }

    pub(crate) fn run_snippet(&mut self, program: CompiledProgram) -> Result<()> {
        let saved = self.snapshot_for_eval();
        self.functions.extend(program.functions);
        self.macros.extend(program.macros);
        self.struct_defs.extend(program.struct_defs);
        self.enum_defs.extend(program.enum_defs);
        self.variant_defs.extend(program.variant_defs);
        self.init_script_globals(program.global_names);
        self.code = Rc::new(program.code);
        self.hot_ops = program.hot.ops.clone();
        self.hot_args = program.hot.args.clone();
        self.pc = 0;
        self.op_clear();
        if let Err(e) = self.run_interpreter(None) {
            self.restore_eval_snapshot(saved);
            return Err(e);
        }
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
        func: Rc<FunctionObject>,
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

    fn check_strong_params(&mut self, func: &FunctionObject, args: &[Value]) -> Result<()> {
        for (i, param) in func.params.iter().enumerate() {
            if param.is_variadic {
                continue;
            }
            if let (Some(ty), true) = (&param.type_expr, param.type_strong) {
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
            let Some(ty) = &param.type_expr else {
                continue;
            };
            let Some(val) = args.get(i).cloned() else {
                continue;
            };
            if types::type_accepts(self, &val, ty) {
                continue;
            }
            let type_name = match ty {
                crate::ast::TypeExpr::Name(n) => n.clone(),
                crate::ast::TypeExpr::Attr { .. } => {
                    crate::types::resolve_type_expr_name(self, ty)?
                }
                crate::ast::TypeExpr::Generic { name, .. } => name.clone(),
            };
            match self.convert_type(Value::type_ref(type_name), val) {
                Ok(converted) => {
                    args[i] = converted;
                }
                Err(e) => {
                    let msg = format!(
                        "parameter '{}': implicit convert failed: {e}",
                        param.name
                    );
                    let exc = exceptions::make_exception(self, "TypeError", msg)?;
                    self.throw_value(exc)?;
                }
            }
        }
        Ok(args)
    }

    pub(crate) fn check_list_element_write(
        &mut self,
        list: &Rc<RefCell<Vec<Value>>>,
        elem: &Value,
    ) -> Result<()> {
        let ptr = Rc::as_ptr(list) as usize;
        let Some(ty) = self.list_element_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&ty, elem, "[*]")
    }

    pub(crate) fn check_dict_write(
        &mut self,
        dict: &Rc<RefCell<crate::value::DictMap>>,
        key: &Value,
        val: &Value,
    ) -> Result<()> {
        let ptr = Rc::as_ptr(dict) as usize;
        let Some((kty, vty)) = self.dict_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&kty, key, "[key]")?;
        self.check_element_against(&vty, val, &format!("[{}]", key.print_string()))
    }

    pub(crate) fn check_set_element_write(
        &mut self,
        set: &Rc<RefCell<crate::value::SetMap>>,
        elem: &Value,
    ) -> Result<()> {
        let ptr = Rc::as_ptr(set) as usize;
        let Some(ty) = self.set_element_contracts.get(&ptr).cloned() else {
            return Ok(());
        };
        self.check_element_against(&ty, elem, "{*}")
    }

    fn check_element_against(
        &mut self,
        ty: &crate::ast::TypeExpr,
        elem: &Value,
        path: &str,
    ) -> Result<()> {
        if types::type_accepts(self, elem, ty) {
            return Ok(());
        }
        let msg = format!(
            "expected {}, got {} at {path}",
            type_expr_display(ty),
            elem.type_name()
        );
        self.raise_type_error(msg)
    }

    pub(crate) fn call_user_function(&mut self, func: Rc<FunctionObject>, args: Vec<Value>) -> Result<Value> {
        let bound = self.bind_call_arguments(&func, args, DictMap::new())?;
        let stack_base = self.stack_sp;
        let stop_depth = self.user_call_frames.len();
        self.setup_user_call(func, bound, false)?;
        let result = self.run_interpreter(Some(stop_depth))?.unwrap_or_else(|| Value::None);
        self.op_truncate(stack_base);
        Ok(result)
    }

    fn setup_user_call(
        &mut self,
        func: Rc<FunctionObject>,
        args: Vec<Value>,
        reenter: bool,
    ) -> Result<()> {
        if self.user_call_frames.len() >= max_call_depth() {
            return Err(RuntimeError::msg("RecursionError: maximum recursion depth exceeded"));
        }
        let args = self.apply_implicit_param_converts(&func, args)?;
        if self.active_exception.is_some() {
            return Ok(());
        }
        self.check_strong_params(&func, &args)?;
        if self.active_exception.is_some() {
            return Ok(());
        }

        if func.track_frames {
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

        if func.uses_name_map {
            self.name_to_slot.push(Some(FxHashMap::default()));
        } else {
            self.name_to_slot.push(None);
        }

        let captured_len = func.captured.as_ref().map(|c| c.len()).unwrap_or(0);
        let frame_size = func
            .frame_slots
            .max(func.params.len() + captured_len);
        let mut locals = self.alloc_local_frame(frame_size);

        let mut slot = func.params.len();
        if func.uses_name_map {
            if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                if let Some(captured) = &func.captured {
                    for (name, val) in captured {
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
            for (name, val) in captured {
                if func.params.iter().any(|p| p.name == *name) {
                    continue;
                }
                if slot < locals.len() {
                    locals[slot] = val.clone();
                }
                slot += 1;
            }
        }

        for (i, val) in args.into_iter().enumerate() {
            if i < locals.len() {
                locals[i] = val;
            }
            if func.uses_name_map {
                if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                    if let Some(param) = func.params.get(i) {
                        names.insert(param.name.clone(), i);
                    }
                }
            }
        }
        // 确保所有形参名都在 name map 中（含仅默认值 / *args / **kwargs）。
        if func.uses_name_map {
            if let Some(names) = self.name_to_slot.last_mut().and_then(|m| m.as_mut()) {
                for (i, param) in func.params.iter().enumerate() {
                    names.entry(param.name.clone()).or_insert(i);
                }
            }
        }
        self.locals_stack.push(locals);

        self.user_call_frames.push(UserCallFrame {
            saved_code: self.code.clone(),
            saved_hot_ops: self.hot_ops.clone(),
            saved_hot_args: self.hot_args.clone(),
            saved_pc: self.pc,
            saved_line_map: self.active_line_map.clone(),
            saved_column_map: self.active_column_map.clone(),
            func: func.clone(),
            pushed_func_stack: !reenter,
        });
        self.code = func.body.clone();
        self.hot_ops = func.hot.ops.clone();
        self.hot_args = func.hot.args.clone();
        self.active_line_map = func.line_map.clone();
        self.active_column_map = func.column_map.clone();
        self.pc = 0;
        Ok(())
    }

    fn restore_user_call_frame(&mut self, frame: UserCallFrame) -> Result<()> {
        self.code = frame.saved_code;
        self.hot_ops = frame.saved_hot_ops;
        self.hot_args = frame.saved_hot_args;
        self.pc = frame.saved_pc;
        self.active_line_map = frame.saved_line_map;
        self.active_column_map = frame.saved_column_map;
        Ok(())
    }

    fn complete_user_return_instruction(
        &mut self,
        leave_scope: bool,
        result: Value,
    ) -> Result<Option<Value>> {
        if leave_scope {
            self.leave_scope();
        }

        if self.user_call_frames.is_empty() {
            self.push_value(result);
            self.pc = self.code.len();
            return Ok(Some(self.stack_top()));
        }

        let frame = self
            .user_call_frames
            .pop()
            .expect("user_call_frames non-empty on return (theoretically unreachable)");
        let func = frame.func.clone();
        if func.return_strong {
            if let Some(ref ty) = func.return_type {
                if let Some(detail) = types::type_check_error(self, &result, ty) {
                    let msg = format!("return: {detail}");
                    self.leave_scope();
                    if frame.func.track_frames {
                        self.func_frames.pop();
                    }
                    if frame.pushed_func_stack {
                        self.func_stack.pop();
                    }
                    self.restore_user_call_frame(frame)?;
                    let exc = exceptions::make_exception(self, "TypeError", msg)?;
                    self.throw_value(exc)?;
                    return Ok(None);
                }
                types::seal_container_contract(self, &result, ty);
            }
        }

        self.leave_scope();
        if frame.func.track_frames {
            self.func_frames.pop();
        }
        if frame.pushed_func_stack {
            self.func_stack.pop();
        }
        self.restore_user_call_frame(frame)?;
        self.push_value(result);
        Ok(None)
    }

    fn unwind_user_calls_on_error(&mut self) -> Result<()> {
        while self.fast_ret_sp > 0 {
            self.fast_ret_sp -= 1;
            self.pop_lightweight_frame();
        }
        while let Some(frame) = self.user_call_frames.pop() {
            self.leave_scope();
            if frame.func.track_frames {
                self.func_frames.pop();
            }
            if frame.pushed_func_stack {
                self.func_stack.pop();
            }
            self.restore_user_call_frame(frame)?;
        }
        Ok(())
    }

    fn line_from_map(line_map: &[usize], pc: usize) -> usize {
        if pc == 0 {
            return line_map.first().copied().unwrap_or(0);
        }
        line_map
            .get(pc.saturating_sub(1))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn current_column(&self) -> usize {
        if self.pc == 0 {
            return self.active_column_map.first().copied().unwrap_or(1);
        }
        self.active_column_map
            .get(self.pc.saturating_sub(1))
            .copied()
            .unwrap_or(1)
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

    pub(crate) fn dispatch_overload(
        &mut self,
        overloads: &[Rc<FunctionObject>],
        args: &[Value],
    ) -> Result<Value> {
        let mut best: Option<(usize, Rc<FunctionObject>)> = None;
        for func in overloads {
            if let Some(score) = types::dispatch_match_score(self, func, args) {
                if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
                    best = Some((score, func.clone()));
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
        state: &Rc<RefCell<IteratorState>>,
    ) -> Result<Option<Value>> {
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
                    return Ok(Some(Value::List(Rc::new(RefCell::new(out)))));
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
                            if self.call_user_function(func, vec![item.clone()])?.is_truthy() {
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
            }
        }
    }

    pub(crate) fn zip_iterables(&self, iterables: Vec<Value>) -> Result<Value> {
        let mut children = Vec::new();
        for it in iterables {
            let state = crate::value::value_to_iterable(&it)?;
            children.push(Rc::new(RefCell::new(state)));
        }
        Ok(IteratorState::from_zip(children).as_value())
    }
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
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => Ok(match x.cmp_num(y) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        _ => Err(RuntimeError::type_err("comparison requires num")),
    }
}

fn index_value(vm: &mut Vm, obj: &Value, idx: &Value) -> Result<Value> {
    match (obj, idx) {
        (Value::TypeRef(ref type_name), idx) if types::is_generic_type_formable(vm, type_name) => {
            let args = types::type_index_operand_to_args(idx)?;
            let def = vm.struct_defs.get(type_name).ok_or_else(|| {
                RuntimeError::msg(format!("unknown generic struct type: {type_name}"))
            })?;
            if args.len() != def.type_params.len() {
                return Err(RuntimeError::type_err(format!(
                    "struct {type_name} expects {} type argument(s), got {}",
                    def.type_params.len(),
                    args.len()
                )));
            }
            Ok(Value::TypeSpec(crate::value::TypeSpecData::new(
                type_name.clone(),
                args,
            )))
        }
        (Value::List(v), Value::Num(n)) => {
            let i = num_to_isize(n)?;
            let borrowed = v.try_borrow().map_err(|_| {
                RuntimeError::msg("RuntimeError: list is already borrowed")
            })?;
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
            Ok(Value::Num(Num::Small(b[idx as usize] as i64)))
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

fn slice_get(
    vm: &mut Vm,
    obj: &Value,
    start: &Value,
    end: &Value,
    step: &Value,
) -> Result<Value> {
    match obj {
        Value::List(v) => {
            let len = v.borrow().len() as isize;
            let indices = compute_slice_indices(len, start, end, step)?;
            let out: Vec<Value> = indices
                .into_iter()
                .map(|i| v.borrow()[i].clone())
                .collect();
            Ok(Value::List(Rc::new(RefCell::new(out))))
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
            Ok(Value::Bytes(Rc::new(out)))
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
        Value::Text(_) => Err(RuntimeError::value_err("text does not support slice assignment")),
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

fn is_slice_omitted(v: &Value) -> bool {
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

fn try_variant_case_convert(vm: &Vm, case_struct_name: &str, value: &Value) -> Option<Result<Value>> {
    let parent_variant = vm
        .variant_defs
        .values()
        .find(|vdef| vdef.cases.iter().any(|c| c.struct_name == case_struct_name))?;
    let Value::Variant(v) = value else {
        return Some(Err(type_registry::type_convert_error(case_struct_name, value)));
    };
    if v.def.name != parent_variant.name && v.inst_name != parent_variant.name {
        return Some(Err(RuntimeError::type_err(format!(
            "TypeError: cannot convert variant {} to {case_struct_name}",
            v.inst_name
        ))));
    }
    let Value::Struct(s) = &v.payload else {
        return Some(Err(RuntimeError::type_err(format!(
            "TypeError: variant payload is not a case struct for {case_struct_name}"
        ))));
    };
    if s.def.name != case_struct_name {
        return Some(Err(RuntimeError::type_err(format!(
            "TypeError: variant case {} does not match {case_struct_name}",
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
    generic_args: Option<Vec<crate::ast::TypeExpr>>,
    payload: Value,
) -> Result<Value> {
    let vdef = vm
        .variant_defs
        .get(inst_name)
        .cloned()
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
        .cloned()
        .ok_or_else(|| RuntimeError::msg(format!("unknown variant: {variant_name}")))?;
    if let Some(case) = vdef.cases.iter().find(|c| c.name == field) {
        let struct_name = case.struct_name.clone();
        return Ok(Value::Builtin(Rc::new(move |vm, args| {
            make_struct(vm, &struct_name, args.to_vec(), None)
        })));
    }
    Err(RuntimeError::attr_err(format!(
        "variant {variant_name} has no case {field}"
    )))
}

fn enum_type_attr(vm: &mut Vm, enum_name: &str, field: &str) -> Result<Value> {
    let def = vm
        .enum_defs
        .get(enum_name)
        .cloned()
        .ok_or_else(|| RuntimeError::msg(format!("unknown enum: {enum_name}")))?;
    let method_name = format!("{enum_name}.{field}");
    if let Some(func) = vm.functions.get(&method_name) {
        let cls = Value::type_ref(enum_name);
        let func = func.clone();
        return Ok(Value::Builtin(Rc::new(move |vm, args| {
            let mut full_args = vec![cls.clone()];
            full_args.extend_from_slice(args);
            vm.call_user_function(func.clone(), full_args)
        })));
    }
    if field == "name_of" {
        let enum_name = enum_name.to_string();
        return Ok(Value::Builtin(Rc::new(move |vm, args| {
            crate::enum_variant::enum_name_of(vm, &enum_name, args)
        })));
    }
    if let Some(idx) = def.members.iter().position(|m| m.name == field) {
        return Ok(crate::enum_variant::enum_member_value(&def, idx));
    }
    Err(RuntimeError::attr_err(format!("enum {enum_name} has no member or method {field}")))
}

fn type_spec_attr(
    vm: &mut Vm,
    name: &str,
    type_args: &[crate::ast::TypeExpr],
    field: &str,
) -> Result<Value> {
    if let Some(vdef) = vm.variant_defs.get(name) {
        if vdef.cases.iter().any(|c| c.name == field) {
            let struct_name = format!("{name}.{field}");
            let generic_args = type_args.to_vec();
            return Ok(Value::Builtin(Rc::new(move |vm, args| {
                make_struct(vm, &struct_name, args.to_vec(), Some(generic_args.clone()))
            })));
        }
    }
    if vm.struct_defs.contains_key(name) {
        return Err(RuntimeError::attr_err(format!("type spec has no attribute {field}")));
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
    Err(RuntimeError::attr_err(format!("type {type_name} has no attribute {field}")))
}

fn get_attr(vm: &mut Vm, obj: &Value, field: &str) -> Result<Value> {
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
        Value::EnumMember(_) => Err(RuntimeError::attr_err(format!("enum member has no field {field}"))),
        Value::Variant(v) if field == "value" || field == "__payload__" => Ok(v.payload.clone()),
        Value::List(list) => type_registry::get_list_method(list, field),
        Value::Dict(dict) => type_registry::get_dict_method(dict, field),
        Value::Set(set) => type_registry::get_set_method(set, field),
        Value::Tuple(tuple) => type_registry::get_tuple_method(tuple, field),
        Value::Bytes(bytes) => type_registry::get_bytes_method(bytes, field),
        Value::Struct(s) => {
            if let Some(idx) = s.def.fields.iter().position(|f| f == field) {
                return Ok(s.slots.borrow()[idx].clone());
            }
            let method_name = format!("{}.{}", s.def.name, field);
            if let Some(func) = vm.functions.get(&method_name) {
                let self_val = obj.clone();
                let func = func.clone();
                return Ok(Value::Builtin(Rc::new(move |vm, args| {
                    let mut full_args = vec![self_val.clone()];
                    full_args.extend_from_slice(args);
                    vm.call_user_function(func.clone(), full_args)
                })));
            }
            if let Some(overloads) = vm.overload_tables.get(&method_name) {
                let self_val = obj.clone();
                let overloads = overloads.clone();
                return Ok(Value::Builtin(Rc::new(move |vm, args| {
                    let mut full_args = vec![self_val.clone()];
                    full_args.extend_from_slice(args);
                    vm.dispatch_overload(&overloads, &full_args)
                })));
            }
            Err(RuntimeError::attr_err(format!("no field {field}")))
        }
        _ => {
            if let Some(f) = vm.functions.get(field) {
                return Ok(Value::Function(f.clone()));
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
            return Err(RuntimeError::attr_err(format!("field {field} is not mutable")));
        }
        if let Some(info) = s.def.field_types.get(idx) {
            if info.strict {
                if let Some(ref ty) = info.type_expr {
                    if let Some(detail) = types::type_check_error(vm, &val, ty) {
                        let msg = format!("field '{field}': {detail}");
                        let exc = exceptions::make_exception(vm, "TypeError", msg)?;
                        vm.throw_value(exc)?;
                        return Ok(());
                    }
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
    explicit_generics: Option<Vec<crate::ast::TypeExpr>>,
) -> Result<Value> {
    let def = vm
        .struct_defs
        .get(name)
        .cloned()
        .ok_or_else(|| RuntimeError::msg(format!("unknown struct: {name}")))?;
    let allow_partial = def.fields.len() == 2
        && def.fields.get(1).map(|f| f.as_str()) == Some("traceback")
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

    let generic_args: Vec<crate::ast::TypeExpr> = if let Some(explicit) = explicit_generics {
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
                    .unwrap_or(TypeExpr::Name(p.clone()))
            })
            .collect()
    } else {
        Vec::new()
    };

    if !def.type_params.is_empty()
        && !types::check_type_param_bounds(vm, &def.type_params, &generic_args)
    {
        let exc = exceptions::make_exception(vm, "TypeError", format!("type argument out of bounds for {name}"))?;
        vm.throw_value(exc)?;
        return Ok(Value::None);
    }

    let subs: std::collections::HashMap<String, crate::ast::TypeExpr> = def
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
                if let Some(ref ty) = info.type_expr {
                    let resolved = types::substitute_type_expr(ty, &subs);
                    if let Some(detail) = types::type_check_error(vm, val, &resolved) {
                        let msg = format!("field '{}': {detail}", def.fields[i]);
                        let exc = exceptions::make_exception(vm, "TypeError", msg)?;
                        vm.throw_value(exc)?;
                        return Ok(Value::None);
                    }
                }
            }
        }
    }
    let val = Value::Struct(Rc::new(crate::value::StructInstance {
        def,
        slots: RefCell::new(slots),
        generic_args,
    }));
    vm.track_value(&val);
    if let Value::Struct(s) = &val {
        if vm.struct_has_method(&val, "__init__") {
            let init_name = format!("{}.__init__", s.def.name);
            let init_args = if let Some(func) = vm.functions.get(&init_name) {
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
        _ => Err(RuntimeError::type_err(
            "can only unpack list or tuple",
        )),
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
                let i: i64 = r.numer().try_into().map_err(|_| RuntimeError::type_err("bad index"))?;
                Ok(i as isize)
            } else {
                Err(RuntimeError::type_err("index must be integer"))
            }
        }
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
            std::rc::Rc::ptr_eq(&x.def, &y.def) && x.member_index == y.member_index
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
    type_args: Vec<TypeExpr>,
) -> Result<Rc<FunctionObject>> {
    let ctx = crate::protocol::TypeCheckContext::from_vm(vm);
    let mut cache: HashMap<String, Rc<FunctionObject>> = vm
        .functions
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let func = crate::codegen::Generator::specialize_generic_template(
        template,
        type_args,
        &ctx,
        &mut cache,
    )?;
    for (k, v) in cache {
        vm.functions.entry(k).or_insert(v);
    }
    Ok(func)
}

fn infer_generic_type_args_from_values(
    template: &crate::opcode::GenericFunctionTemplate,
    args: &[Value],
) -> Result<Vec<TypeExpr>> {
    if template.type_params.len() != 1 {
        return Err(RuntimeError::msg(format!(
            "cannot infer {} type parameter(s) for `{}`; use {}[...](...)",
            template.type_params.len(),
            template.name,
            template.name
        )));
    }
    if args.is_empty() {
        return Err(RuntimeError::type_err(format!(
            "{} expects at least one argument for type inference",
            template.name
        )));
    }
    Ok(vec![TypeExpr::Name(args[0].type_name().to_string())])
}

fn type_args_from_runtime_index(idx: &Value) -> Result<Vec<TypeExpr>> {
    match idx {
        Value::TypeRef(name) => Ok(vec![TypeExpr::Name(name.clone())]),
        Value::TypeSpec(spec) => Ok(vec![TypeExpr::Generic {
            name: spec.name.clone(),
            params: spec.args.clone(),
        }]),
        Value::List(items) => {
            let borrowed = items.borrow();
            if borrowed.is_empty() {
                return Err(RuntimeError::type_err(
                    "generic type argument list cannot be empty",
                ));
            }
            borrowed
                .iter()
                .map(|v| {
                    type_args_from_runtime_index(v).and_then(|mut args| {
                        if args.len() == 1 {
                            Ok(args
                                .pop()
                                .expect("single type arg checked above (theoretically unreachable)"))
                        } else {
                            Err(RuntimeError::type_err(
                                "invalid nested type argument in generic index",
                            ))
                        }
                    })
                })
                .collect()
        }
        Value::Text(name) => Ok(vec![TypeExpr::Name(name.clone())]),
        other => Err(RuntimeError::type_err(format!(
            "expected a type in generic index (e.g. num, text, list), got {}",
            other.type_name()
        ))),
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}
