use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use crate::error::RuntimeError;
use crate::opcode::FunctionObject;
use crate::opcode::MacroObject;
use crate::runtime_ast::RuntimeAstNode;
use crate::shared::{Shared, SyncCell};
use crate::Result;

/// `Num::{floor,ceil,trunc,round}_num`：整数原样，有理数走 `BigRational` 对应方法。
macro_rules! num_rat_round {
    ($($name:ident => $rat_method:ident),+ $(,)?) => {
        $(
            pub fn $name(&self) -> Num {
                match self {
                    Num::Small(n) => Num::Small(*n),
                    Num::Int(n) => Num::Int(n.clone()),
                    Num::Rat(r) => Num::from_bigint(r.$rat_method().to_integer()),
                }
            }
        )+
    };
}

/// `Value` 上仅接受 `Num` 的二元运算包装。
macro_rules! value_num_binop {
    ($($name:ident, $helper:ident, $op:literal),+ $(,)?) => {
        $(
            pub fn $name(&self, other: &Value) -> Result<Value> {
                match (self, other) {
                    (Value::Num(a), Value::Num(b)) => Ok(Value::Num($helper(a, b)?)),
                    _ => Err(RuntimeError::unsupported(concat!(
                        "unsupported ",
                        $op,
                        " operation"
                    ))),
                }
            }
        )+
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Num {
    Small(i64),
    /// 堆上整数 — 放在 `Arc` 后以保持 `Value` 紧凑。
    Int(Arc<BigInt>),
    /// 堆上有理数 — 放在 `Arc` 后以保持 `Value` 紧凑。
    Rat(Arc<BigRational>),
}

impl Num {
    #[must_use]
    pub const fn small(n: i64) -> Self {
        Self::Small(n)
    }

    #[must_use]
    pub const fn from_i64(n: i64) -> Self {
        Self::small(n)
    }

    #[inline]
    #[must_use]
    pub fn from_bigint(n: BigInt) -> Self {
        match n.to_i64() {
            Some(i) => Self::Small(i),
            None => Self::Int(Arc::new(n)),
        }
    }

    #[inline]
    #[must_use]
    pub fn from_rational(r: BigRational) -> Self {
        if r.denom() == &One::one() {
            return Self::from_bigint(r.numer().clone());
        }
        Self::Rat(Arc::new(r))
    }

    pub fn from_literal(text: &str) -> Result<Self> {
        let t = text.trim();
        if let Some((numer_text, denom_text)) = t.split_once('/') {
            let numer: BigInt = numer_text
                .trim()
                .parse()
                .map_err(|_| RuntimeError::value_err(format!("invalid rational numerator: {text}")))?;
            let denom: BigInt = denom_text
                .trim()
                .parse()
                .map_err(|_| RuntimeError::value_err(format!("invalid rational denominator: {text}")))?;
            if denom.is_zero() {
                return Err(RuntimeError::value_err(format!("invalid rational literal: {text}")));
            }
            return Ok(Self::from_rational(BigRational::new(numer, denom)));
        }
        if t.contains('.') || t.contains('e') || t.contains('E') || t.starts_with('.') {
            let rat = parse_decimal_literal(t)
                .map_err(|_| RuntimeError::value_err(format!("invalid number literal: {text}")))?;
            return Ok(Self::from_rational(rat));
        }
        if let Ok(n) = t.parse::<i64>() {
            return Ok(Self::Small(n));
        }
        let n: BigInt = t
                .parse()
                .map_err(|_| RuntimeError::value_err(format!("invalid integer literal: {text}")))?;
        Ok(Self::from_bigint(n))
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        match self {
            Self::Small(n) => *n == 0,
            Self::Int(n) => n.is_zero(),
            Self::Rat(r) => r.is_zero(),
        }
    }

    #[must_use]
    pub fn to_rational(&self) -> BigRational {
        match self {
            Self::Small(n) => BigRational::from((BigInt::from(*n), One::one())),
            Self::Int(n) => BigRational::from((n.as_ref().clone(), One::one())),
            Self::Rat(r) => r.as_ref().clone(),
        }
    }

    #[must_use]
    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Self::Small(n) => Some(*n),
            Self::Int(n) => n.to_i64(),
            Self::Rat(r) if r.denom() == &One::one() => r.numer().to_i64(),
            _ => None,
        }
    }

    /// 转为整数 `BigInt`；有理数报错（按位/取模仅支持整数）。
    pub fn to_bigint(&self) -> Result<BigInt> {
        match self {
            Self::Small(n) => Ok(BigInt::from(*n)),
            Self::Int(n) => Ok(n.as_ref().clone()),
            Self::Rat(_) => Err(RuntimeError::type_err(
                "bitwise/modulo operators require integers, got rational",
            )),
        }
    }

    #[must_use]
    pub fn abs_num(&self) -> Self {
        match self {
            Self::Small(n) => Self::Small(n.abs()),
            Self::Int(i) => Self::from_bigint(i.abs()),
            Self::Rat(r) => Self::from_rational(r.abs()),
        }
    }

    // Small/Int 原样；有理数走 BigRational 的 floor/ceil/trunc/round。
    num_rat_round! {
        floor_num => floor,
        ceil_num => ceil,
        trunc_num => trunc,
        round_num => round,
    }

    pub fn to_f64_checked(&self) -> crate::Result<f64> {
        let r = self.to_rational();
        r.to_f64().ok_or_else(|| {
            crate::error::RuntimeError::value_err("number too large for floating-point conversion")
        })
    }

    #[must_use]
    pub fn eq_num(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a.as_ref() == b.as_ref(),
            (Self::Rat(a), Self::Rat(b)) => a.as_ref() == b.as_ref(),
            (Self::Small(a), Self::Int(b)) => match b.to_i64() {
                Some(bi) => a == &bi,
                None => self.to_rational() == other.to_rational(),
            },
            (Self::Int(a), Self::Small(b)) => match a.to_i64() {
                Some(ai) => ai == *b,
                None => self.to_rational() == other.to_rational(),
            },
            (Self::Small(a), Self::Rat(b)) if b.denom() == &One::one() => b
                .numer()
                .to_i64().map_or_else(|| self.to_rational() == **b, |bi| a == &bi),
            (Self::Rat(a), Self::Small(b)) if a.denom() == &One::one() => a
                .numer()
                .to_i64().map_or_else(|| **a == other.to_rational(), |ai| ai == *b),
            _ => self.to_rational() == other.to_rational(),
        }
    }

    #[must_use]
    pub fn cmp_num(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Small(a), Self::Small(b)) => a.cmp(b),
            (Self::Int(a), Self::Int(b)) => a.as_ref().cmp(b.as_ref()),
            (Self::Rat(a), Self::Rat(b)) => a.as_ref().cmp(b.as_ref()),
            (Self::Small(a), Self::Int(b)) => match b.to_i64() {
                Some(bi) => a.cmp(&bi),
                None => self.to_rational().cmp(&other.to_rational()),
            },
            (Self::Int(a), Self::Small(b)) => match a.to_i64() {
                Some(ai) => ai.cmp(b),
                None => self.to_rational().cmp(&other.to_rational()),
            },
            (Self::Small(a), Self::Rat(b)) if b.denom() == &One::one() => {
                if let Some(bi) = b.numer().to_i64() {
                    a.cmp(&bi)
                } else {
                    self.to_rational().cmp(b.as_ref())
                }
            }
            (Self::Rat(a), Self::Small(b)) if a.denom() == &One::one() => {
                if let Some(ai) = a.numer().to_i64() {
                    ai.cmp(b)
                } else {
                    a.as_ref().cmp(&other.to_rational())
                }
            }
            _ => self.to_rational().cmp(&other.to_rational()),
        }
    }
}

impl fmt::Display for Num {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Small(n) => write!(f, "{n}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Rat(r) => {
                if r.denom() == &One::one() {
                    write!(f, "{}", r.numer())
                } else {
                    write!(f, "{r}")
                }
            }
        }
    }
}

pub type BuiltinFn = Arc<dyn Fn(&mut crate::vm::Vm, &[Value]) -> Result<Value> + Send + Sync>;

/// 具名内建：短名用于 repr（`<builtin alloc>`），不含模块点分路径。
#[derive(Clone)]
pub struct BuiltinObject {
    pub name: Arc<str>,
    pub func: BuiltinFn,
}

impl BuiltinObject {
    pub fn new(
        name: impl Into<Arc<str>>,
        func: BuiltinFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            func,
        })
    }

    pub fn call(&self, vm: &mut crate::vm::Vm, args: &[Value]) -> Result<Value> {
        (self.func)(vm, args)
    }

    #[must_use]
    pub fn repr(&self) -> String {
        builtin_repr(&self.name)
    }
}

/// 与 [`BuiltinObject::repr`] 同形：诊断前缀用短名，禁止手写 `"<builtin …>"` 字面量漂移。
#[must_use]
pub fn builtin_repr(name: &str) -> String {
    format!("<builtin {name}>")
}

#[derive(Clone)]
pub struct ModuleObject {
    pub name: String,
    pub exports: HashMap<String, Value>,
    pub children: HashMap<String, Shared<Self>>,
    pub is_user: bool,
}

impl ModuleObject {
    #[must_use]
    pub fn new_user(name: String) -> Self {
        Self {
            name,
            exports: HashMap::new(),
            children: HashMap::new(),
            is_user: true,
        }
    }

    #[must_use]
    pub fn get_export(&self, name: &str) -> Option<Value> {
        self.exports.get(name).cloned()
    }

    #[must_use]
    pub fn get_attr(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.exports.get(name) {
            return Some(v.clone());
        }
        self.children
            .get(name)
            .map(|m| Value::Module(m.clone()))
    }
}

#[derive(Clone)]
pub struct DispatchTable {
    pub name: String,
    pub handlers: Shared<Vec<Value>>,
}

#[derive(Clone)]
pub enum IteratorKind {
    Range {
        current: i64,
        stop: i64,
        step: i64,
    },
    List {
        items: Vec<Value>,
        index: usize,
    },
    Zip {
        children: Vec<Shared<IteratorState>>,
    },
    Map {
        func: Arc<FunctionObject>,
        source: Shared<IteratorState>,
    },
    Filter {
        func: Arc<FunctionObject>,
        source: Shared<IteratorState>,
    },
    /// 生成器推导式：惰性求值 elem，可选 guards。
    GenExpr {
        source: Shared<IteratorState>,
        arity: usize,
        elem: Arc<FunctionObject>,
        guards: Vec<Arc<FunctionObject>>,
    },
    /// `repeat(x[, n])`：`n` 为 `None` 时无限重复。
    Repeat {
        value: Value,
        remaining: Option<usize>,
    },
    /// `cycle(xs)`：循环有限序列；空序列立即耗尽。
    Cycle {
        items: Vec<Value>,
        index: usize,
    },
    /// `for (v in ch)`：阻塞 recv，关闭后结束。
    Channel {
        channel: Shared<ChannelInner>,
    },
    /// `stream_take` / `std.iter.take`：最多产出 `remaining` 个元素。
    Take {
        remaining: usize,
        source: Shared<IteratorState>,
    },
    /// `std.iter.skip` / `drop`：先丢弃 `remaining` 个，再透传源。
    Skip {
        remaining: usize,
        source: Shared<IteratorState>,
    },
    /// `std.iter.enumerate`：产出 `[index, item]`（index 从 0）。
    Enumerate {
        index: usize,
        source: Shared<IteratorState>,
    },
    /// `std.iter.chain`：依次耗尽多个源。
    Chain {
        sources: Vec<Shared<IteratorState>>,
        current: usize,
    },
    /// 用户类型迭代器协议：对 `obj` 反复调用 `__next__`；耗尽时抛 `StopIteration`。
    User {
        obj: Value,
    },
    /// 用户生成器：调用含 `yield` 的 func/do 得到的惰性迭代器。
    Generator {
        func: Arc<FunctionObject>,
        locals: Vec<Value>,
        name_map: Option<FxHashMap<String, usize>>,
        pc: usize,
        exhausted: bool,
        yield_from: Option<Shared<IteratorState>>,
    },
}

#[derive(Clone)]
pub struct IteratorState {
    pub kind: IteratorKind,
}

#[derive(Clone, Default)]
pub struct DictMap {
    map: FxHashMap<ValueKey, Value>,
    order: Vec<ValueKey>,
}

/// 可哈希键的有序集合（保持插入顺序）。
#[derive(Clone, Default)]
pub struct SetMap {
    map: FxHashSet<ValueKey>,
    order: Vec<ValueKey>,
}

impl SetMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: ValueKey) -> bool {
        if self.map.insert(key.clone()) {
            self.order.push(key);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn contains(&self, key: &ValueKey) -> bool {
        self.map.contains(key)
    }

    pub fn remove(&mut self, key: &ValueKey) -> bool {
        if self.map.remove(key) {
            self.order.retain(|k| k != key);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &ValueKey> {
        self.order.iter()
    }
}

impl DictMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: ValueKey, val: Value) {
        if !self.map.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.map.insert(key, val);
    }

    #[must_use]
    pub fn get(&self, key: &ValueKey) -> Option<&Value> {
        self.map.get(key)
    }

    #[must_use]
    pub fn contains_key(&self, key: &ValueKey) -> bool {
        self.map.contains_key(key)
    }

    pub fn remove(&mut self, key: &ValueKey) -> Option<Value> {
        if let Some(val) = self.map.remove(key) {
            self.order.retain(|k| k != key);
            Some(val)
        } else {
            None
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ValueKey, &Value)> {
        self.order
            .iter()
            .filter_map(|k| self.map.get(k).map(|v| (k, v)))
    }

    pub fn keys(&self) -> impl Iterator<Item = &ValueKey> {
        self.order.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.order
            .iter()
            .filter_map(|k| self.map.get(k))
    }
}

#[derive(Clone)]
pub enum TaskState {
    Pending {
        callable: Value,
        args: Vec<Value>,
    },
    Running,
    /// 已挂起；纤程快照由 VM 侧 `task_fibers` 表持有。
    Suspended,
    Done(Value),
    Failed(Value),
}

pub struct TaskInner {
    pub state: TaskState,
    /// 归属的 `TaskGroup`（若有）：任务结束时自动 `done`。
    pub task_group: Option<Shared<SyncInner>>,
    /// 协作式取消请求；在 await / sleep / 调度入口等检查点生效。
    pub cancelled: bool,
    /// 调试器停在该任务上：勿入就绪队列，直至会话 `continue`。
    pub debug_paused: bool,
}

impl TaskInner {
    #[must_use]
    pub const fn pending(callable: Value, args: Vec<Value>) -> Self {
        Self {
            state: TaskState::Pending { callable, args },
            task_group: None,
            cancelled: false,
            debug_paused: false,
        }
    }

    #[must_use]
    pub const fn done(value: Value) -> Self {
        Self {
            state: TaskState::Done(value),
            task_group: None,
            cancelled: false,
            debug_paused: false,
        }
    }

    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub const fn request_cancel(&mut self) {
        self.cancelled = true;
    }
}

/// `capacity`: `None` = 无界；`Some(0)` = rendezvous；`Some(n)` = 有界 n。
#[derive(Clone)]
pub struct ChannelInner {
    pub queue: VecDeque<Value>,
    pub capacity: Option<usize>,
    pub closed: bool,
}

/// Stream 体：缓冲 channel `视图，或拉取包装（map/filter/take/from_gen`）。
#[derive(Clone)]
pub enum StreamInner {
    Channel(Shared<ChannelInner>),
    Iter(Shared<IteratorState>),
}

impl ChannelInner {
    #[must_use]
    pub const fn new(capacity: Option<usize>) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
            closed: false,
        }
    }

    pub fn try_recv(&mut self) -> Option<Option<Value>> {
        if let Some(v) = self.queue.pop_front() {
            return Some(Some(v));
        }
        if self.closed {
            return Some(None);
        }
        None
    }

    /// `Some(Ok(()))` 已发送；`Some(Err(()))` 已关闭；`None` 会阻塞。
    pub fn try_send(&mut self, value: Value) -> Option<std::result::Result<(), ()>> {
        if self.closed {
            return Some(Err(()));
        }
        match self.capacity {
            None => {
                self.queue.push_back(value);
                Some(Ok(()))
            }
            Some(0) => {
                if self.queue.is_empty() {
                    self.queue.push_back(value);
                    Some(Ok(()))
                } else {
                    None
                }
            }
            Some(n) => {
                if self.queue.len() < n {
                    self.queue.push_back(value);
                    Some(Ok(()))
                } else {
                    None
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct MutexInner {
    pub value: Value,
    pub locked: bool,
}

impl MutexInner {
    #[must_use]
    pub const fn new(value: Value) -> Self {
        Self {
            value,
            locked: false,
        }
    }
}

/// `Mutex.lock()` 的守卫载荷：最后一个 `Shared` 释放时自动 `unlock`，
/// 避免 `m.lock().get()` 在 `GetAttr` 丢弃临时守卫后锁泄漏。
pub struct MutexGuardInner {
    mutex: Shared<MutexInner>,
    released: std::sync::atomic::AtomicBool,
}

impl MutexGuardInner {
    #[must_use]
    pub const fn new(mutex: Shared<MutexInner>) -> Self {
        Self {
            mutex,
            released: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn mutex(&self) -> Shared<MutexInner> {
        self.mutex.clone()
    }

    /// 幂等释放；`__exit__` / `unlock` / `Drop` 均可调用。
    pub fn release(&self) {
        if self
            .released
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        self.mutex.borrow_mut().locked = false;
    }

    /// `Cond.wait` 重新获得锁时复活守卫，使后续 Drop/`__exit__` 再次负责 unlock。
    pub fn clear_released(&self) {
        self.released
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Drop for MutexGuardInner {
    fn drop(&mut self) {
        self.release();
    }
}

/// `Once.run` 生命周期。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OncePhase {
    Idle,
    Running,
    Done,
}

/// 其余并发原语（RWMutex / `WaitGroup` / Semaphore / Once / Barrier / Cond / `TaskGroup` / TimeoutCtx）的统一载荷。
#[derive(Clone)]
pub enum SyncInner {
    /// 读写锁：`readers` 为当前活跃读者数，`writer` 为是否有写者持有。
    RWMutex {
        value: Value,
        readers: usize,
        writer: bool,
    },
    /// 等待组：`count` 降到 0 时唤醒所有 `wait()`。
    WaitGroup { count: i64 },
    /// 计数信号量：`permits` 为可用许可数。
    Semaphore { permits: i64 },
    /// 一次性执行：三态，避免并行下「done 但 value 未写入」。
    Once { phase: OncePhase, value: Value },
    /// 屏障：凑齐 `n` 个 `wait()` 后全部放行（`generation` 递增）。
    Barrier { n: i64, waiting: usize, generation: u64 },
    /// 条件变量：`signals` 为待消费的唤醒令牌，`waiters` 为当前等待者数。
    Cond { signals: i64, waiters: i64 },
    /// `std.async.taskgroup()`：作用域等待组；`run` 跟踪子任务，`__exit__` cancel+join。
    TaskGroup {
        count: i64,
        first_error: Option<Value>,
        cancel_requested: bool,
        tasks: Vec<Shared<TaskInner>>,
    },
    /// `std.async.with_timeout(sec)`：截止时刻；`check()` 超时抛 `Cancelled`。
    TimeoutCtx {
        deadline: std::time::Instant,
    },
    /// `std.sync.Atomic.num/bool`：互斥保护的原子槽。
    Atomic {
        value: Value,
    },
}

/// `RWMutex` 的读/写守卫，支持 `with` 自动释放。
#[derive(Clone)]
pub enum SyncGuardInner {
    Read { mu: Shared<SyncInner> },
    Write { mu: Shared<SyncInner> },
}

#[derive(Clone)]
pub enum Value {
    None,
    Bool(bool),
    Num(Num),
    /// 定宽整数 / 浮点（与 `num` 并列）。
    Sized(crate::sized::SizedNum),
    /// 裸指针地址（宿主指针宽度）。
    Ptr(usize),
    /// `C.frompath` 得到的动态库句柄。
    DllHandle(Arc<crate::ffi::DllHandle>),
    Text(String),
    /// 裸类型句柄（`num`、`MyStruct` 等）— 与 `Text` 字符串值区分。
    TypeRef(String),
    List(Shared<Vec<Self>>),
    Dict(Shared<DictMap>),
    /// 可哈希值的有序集合。空集展示为 `{,}`（`{}` 仍是空字典）。
    Set(Shared<SetMap>),
    /// 不可变定长序列。空元组为 `()`。
    Tuple(Arc<[Self]>),
    /// 原始字节缓冲（非 Unicode 文本）。
    Bytes(Arc<Vec<u8>>),
    Iterator(Shared<IteratorState>),
    Function(Arc<FunctionObject>),
    GenericFunction(Arc<crate::opcode::GenericFunctionTemplate>),
    Macro(Arc<MacroObject>),
    Builtin(Arc<BuiltinObject>),
    Struct(Arc<StructInstance>),
    Module(Shared<ModuleObject>),
    RuntimeAst(Arc<RuntimeAstNode>),
    Dispatch(Shared<DispatchTable>),
    Cell(Shared<Self>),
    /// 泛型结构体索引得到的特化类型句柄，如用于 `Box[num](v)` 的 `Box[num]`。
    TypeSpec(Arc<TypeSpecData>),
    /// 数值枚举成员，如 `Color.Red`。
    EnumMember(Arc<EnumMemberData>),
    /// 外层 variant 包装（双层包装）。
    Variant(Arc<VariantInstance>),
    /// 协作式任务句柄（`go` / `await`）。
    Task(Shared<TaskInner>),
    /// 通道（`Channel()` / `Channel(n)`）。
    Channel(Shared<ChannelInner>),
    /// 只拉取流（与 Channel 共用队列实现；无对外 `send`）。
    Stream(Shared<StreamInner>),
    /// 互斥锁（`Mutex(v)`）。
    Mutex(Shared<MutexInner>),
    /// `Mutex.lock()` 得到的守卫，可用于 `with`；最后一个引用释放时自动 unlock。
    MutexGuard(Shared<MutexGuardInner>),
    /// 其余并发原语（RWMutex/WaitGroup/Semaphore/Once/Barrier/Cond）。
    Sync(Shared<SyncInner>),
    /// `RWMutex` 读/写守卫。
    SyncGuard(Shared<SyncGuardInner>),
    /// 一等布局对象（模块属性如 `C.layout`）：规定 struct 字段的存放顺序/对齐规则。
    Layout(std::sync::Arc<crate::ffi_extra::LayoutObject>),
}

/// [`Value::TypeSpec`] 的堆载荷 — 移出 `Value` 枚举以保持栈表示紧凑。
#[derive(Debug, Clone)]
pub struct TypeSpecData {
    pub name: String,
    pub args: Vec<Value>,
}

impl PartialEq for TypeSpecData {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.args.len() == other.args.len()
            && self
                .args
                .iter()
                .zip(other.args.iter())
                .all(|(a, b)| crate::types::type_values_equal(a, b))
    }
}

impl TypeSpecData {
    pub fn new(name: impl Into<String>, args: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            args,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum ValueKey {
    Bool(bool),
    NumInt(BigInt),
    Text(String),
}

impl ValueKey {
    pub fn from_value(v: &Value) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(Self::Bool(*b)),
            Value::Num(Num::Small(n)) => Ok(Self::NumInt((*n).into())),
            Value::Num(Num::Int(n)) => Ok(Self::NumInt(n.as_ref().clone())),
            Value::Text(s) => Ok(Self::Text(s.clone())),
            other => Err(RuntimeError::type_err(format!(
                "unhashable type: {}",
                other.type_name()
            ))),
        }
    }
}

#[derive(Clone)]
pub struct EnumMemberInfo {
    pub name: String,
    pub value: Num,
}

#[derive(Clone)]
pub struct EnumDef {
    pub name: String,
    pub members: Vec<EnumMemberInfo>,
    /// 类型自身的方法表；`Enum.method` 在此查。
    pub methods: std::collections::HashMap<String, std::sync::Arc<crate::opcode::FunctionObject>>,
}

#[derive(Clone)]
pub struct EnumMemberData {
    pub def: Arc<EnumDef>,
    pub member_index: usize,
}

#[derive(Clone)]
pub struct VariantCaseDef {
    pub name: String,
    pub struct_name: String,
}

#[derive(Clone)]
pub struct VariantDef {
    pub name: String,
    pub type_params: Vec<(String, Option<crate::ast::Expr>)>,
    pub cases: Vec<VariantCaseDef>,
}

#[derive(Clone)]
pub struct VariantInstance {
    pub inst_name: String,
    pub def: Arc<VariantDef>,
    pub generic_args: Vec<Value>,
    pub case_idx: usize,
    pub payload: Value,
}

#[derive(Clone, Default)]
pub struct FieldTypeInfo {
    pub type_expr: Option<crate::ast::Expr>,
    pub strict: bool,
}

#[derive(Clone)]
pub struct StructDef {
    pub name: String,
    pub base: Option<String>,
    pub fields: Vec<String>,
    pub mutable_fields: Vec<bool>,
    pub typed: bool,
    pub field_types: Vec<FieldTypeInfo>,
    pub type_params: Vec<(String, Option<crate::ast::Expr>)>,
    /// `typed struct ... : <layout>` 计算出的本地布局；供 load / store / by-value FFI。
    pub native_layout: Option<std::sync::Arc<crate::ffi_extra::NativeStructLayout>>,
    /// 类型自身的方法表；`a.b` 在此查 `b`，不走全局点分键。
    pub methods: std::collections::HashMap<String, std::sync::Arc<crate::opcode::FunctionObject>>,
    pub overloads: std::collections::HashMap<String, Vec<std::sync::Arc<crate::opcode::FunctionObject>>>,
}

#[derive(Clone)]
pub struct StructInstance {
    pub def: Arc<StructDef>,
    pub slots: SyncCell<Vec<Value>>,
    pub generic_args: Vec<Value>,
}

impl Value {
    #[inline]
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn type_ref(name: impl Into<String>) -> Self {
        Self::TypeRef(name.into())
    }

    /// 具名内建（短名；repr 为 `<builtin name>`）。
    pub fn builtin(
        name: impl Into<Arc<str>>,
        f: impl Fn(&mut crate::vm::Vm, &[Value]) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Self::Builtin(BuiltinObject::new(name, Arc::new(f)))
    }

    /// 由 `fn` 指针构造具名内建。
    pub fn builtin_fn(
        name: impl Into<Arc<str>>,
        f: fn(&mut crate::vm::Vm, &[Value]) -> Result<Value>,
    ) -> Self {
        Self::Builtin(BuiltinObject::new(name, Arc::new(f)))
    }

    /// 索引 / 注解用的类型名操作数（`TypeRef`，以及旧式 `Text`）。
    #[must_use]
    pub const fn as_type_name_operand(&self) -> Option<&str> {
        match self {
            Self::TypeRef(n) => Some(n.as_str()),
            Self::Text(n) => Some(n.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Self::None => "nonetype",
            Self::Bool(_) => "bool",
            Self::Num(_) => "num",
            Self::Sized(s) => s.type_name(),
            Self::Ptr(_) => "ptr",
            Self::DllHandle(_) => "DllHandle",
            Self::Text(_) => "text",
            // 类型句柄的值属于元类型 `type`（如 `type(A)` → type，`type(A())` → A）。
            Self::TypeRef(_) => "type",
            Self::List(_) => "list",
            Self::Dict(_) => "dict",
            Self::Set(_) => "set",
            Self::Tuple(_) => "tuple",
            Self::Bytes(_) => "bytes",
            Self::Iterator(_) => "iterator",
            Self::Function(_) => "function",
            Self::GenericFunction(_) => "generic function",
            Self::Macro(_) => "Macro",
            Self::Builtin(_) => "function",
            Self::Dispatch(_) => "friend func",
            Self::Struct(s) => &s.def.name,
            Self::Module(_) => "module",
            Self::RuntimeAst(_) => "AST",
            Self::Cell(_) => "cell",
            Self::TypeSpec(_) => "type",
            Self::EnumMember(m) => m.def.name.as_str(),
            Self::Variant(v) => &v.inst_name,
            Self::Task(_) => "Task",
            Self::Channel(_) => "Channel",
            Self::Stream(_) => "Stream",
            Self::Mutex(_) => "Mutex",
            Self::MutexGuard(_) => "MutexGuard",
            Self::Sync(s) => match &*s.borrow() {
                SyncInner::RWMutex { .. } => "RWMutex",
                SyncInner::WaitGroup { .. } => "WaitGroup",
                SyncInner::Semaphore { .. } => "Semaphore",
                SyncInner::Once { .. } => "Once",
                SyncInner::Barrier { .. } => "Barrier",
                SyncInner::Cond { .. } => "Cond",
                SyncInner::TaskGroup { .. } => "TaskGroup",
                SyncInner::TimeoutCtx { .. } => "TimeoutCtx",
                SyncInner::Atomic { .. } => "Atomic",
            },
            Self::SyncGuard(g) => match &*g.borrow() {
                SyncGuardInner::Read { .. } => "RWMutexReadGuard",
                SyncGuardInner::Write { .. } => "RWMutexWriteGuard",
            },
            Self::Layout(_) => "Layout",
        }
    }

    #[must_use]
    pub fn type_name_string(&self) -> String {
        self.type_name().to_string()
    }

    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::None => false,
            Self::Bool(b) => *b,
            Self::Num(n) => !n.is_zero(),
            Self::Sized(s) => s.is_truthy(),
            Self::Ptr(p) => *p != 0,
            Self::DllHandle(_) => true,
            Self::Text(s) => !s.is_empty(),
            Self::List(v) => !v.borrow().is_empty(),
            Self::Dict(d) => !d.borrow().is_empty(),
            Self::Set(s) => !s.borrow().is_empty(),
            Self::Tuple(t) => !t.is_empty(),
            Self::Bytes(b) => !b.is_empty(),
            Self::Cell(c) => c.borrow().is_truthy(),
            _ => true,
        }
    }

    pub fn display_string(&self) -> String {
        match self {
            Self::None => "none".to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Num(n) => n.to_string(),
            Self::Sized(s) => s.display_string(),
            Self::Ptr(p) => format!("ptr(0x{p:x})"),
            Self::DllHandle(h) => format!("<DllHandle {}>", h.path),
            Self::Text(s) => format!("\"{s}\""),
            Self::TypeRef(n) => n.clone(),
            Self::List(v) => {
                let parts: Vec<_> = v.borrow().iter().map(Self::display_string).collect();
                format!("[{}]", parts.join(", "))
            }
            Self::Dict(d) => {
                let parts: Vec<_> = d
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", key_display(k), v.display_string()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Self::Set(s) => {
                let borrowed = s.borrow();
                if borrowed.is_empty() {
                    "{,}".to_string()
                } else {
                    let parts: Vec<_> = borrowed.iter().map(key_display).collect();
                    format!("{{{}}}", parts.join(", "))
                }
            }
            Self::Tuple(t) => {
                let parts: Vec<_> = t.iter().map(Self::display_string).collect();
                if t.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Self::Bytes(b) => {
                let mut out = String::from("b\"");
                for &byte in b.iter() {
                    match byte {
                        b'\n' => out.push_str("\\n"),
                        b'\t' => out.push_str("\\t"),
                        b'\r' => out.push_str("\\r"),
                        b'"' => out.push_str("\\\""),
                        b'\\' => out.push_str("\\\\"),
                        0x20..=0x7e => out.push(byte as char),
                        _ => out.push_str(&format!("\\x{byte:02x}")),
                    }
                }
                out.push('"');
                out
            }
            Self::Iterator(_) => "<iterator>".to_string(),
            Self::Function(f) => format!("<function {}>", f.name),
            Self::GenericFunction(g) => format!("<generic function {}>", g.name),
            Self::Macro(m) => format!("<macro {}>", m.name),
            Self::Dispatch(d) => format!("<friend func {}>", d.borrow().name),
            Self::Builtin(b) => b.repr(),
            Self::Module(m) => format!("<module {}>", m.borrow().name),
            Self::Struct(s) => {
                let parts: Vec<_> = s
                    .def
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, name)| {
                        format!(
                            "{}: {}",
                            name,
                            s.slots.borrow()[i].display_string()
                        )
                    })
                    .collect();
                format!("{}({})", s.def.name, parts.join(", "))
            }
            Self::RuntimeAst(_) => "<AST>".to_string(),
            Self::Cell(c) => c.borrow().display_string(),
            Self::TypeSpec(spec) => {
                if spec.args.is_empty() {
                    format!("\"{}\"", spec.name)
                } else {
                    let inner: Vec<String> = spec
                        .args
                        .iter()
                        .map(crate::types::type_expr_display)
                        .collect();
                    format!("{}[{}]", spec.name, inner.join(", "))
                }
            }
            Self::EnumMember(m) => format!(
                "{}.{}",
                m.def.name, m.def.members[m.member_index].name
            ),
            Self::Variant(v) => format!(
                "{}({})",
                v.inst_name,
                v.payload.display_string()
            ),
            Self::Task(_) => "<Task>".to_string(),
            Self::Channel(_) => "<Channel>".to_string(),
            Self::Stream(_) => "<Stream>".to_string(),
            Self::Mutex(_) => "<Mutex>".to_string(),
            Self::MutexGuard(_) => "<MutexGuard>".to_string(),
            Self::Sync(s) => match &*s.borrow() {
                SyncInner::RWMutex { .. } => "<RWMutex>".to_string(),
                SyncInner::WaitGroup { .. } => "<WaitGroup>".to_string(),
                SyncInner::Semaphore { .. } => "<Semaphore>".to_string(),
                SyncInner::Once { .. } => "<Once>".to_string(),
                SyncInner::Barrier { .. } => "<Barrier>".to_string(),
                SyncInner::Cond { .. } => "<Cond>".to_string(),
                SyncInner::TaskGroup { .. } => "<TaskGroup>".to_string(),
                SyncInner::TimeoutCtx { .. } => "<TimeoutCtx>".to_string(),
                SyncInner::Atomic { .. } => "<Atomic>".to_string(),
            },
            Self::SyncGuard(g) => match &*g.borrow() {
                SyncGuardInner::Read { .. } => "<RWMutexReadGuard>".to_string(),
                SyncGuardInner::Write { .. } => "<RWMutexWriteGuard>".to_string(),
            },
            Self::Layout(_) => "<Layout>".to_string(),
        }
    }

    #[must_use]
    pub fn print_string(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            other => other.display_string(),
        }
    }

    pub fn add(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(add_num(a, b))),
            (Self::Text(a), Self::Text(b)) => Ok(Self::Text(format!("{a}{b}"))),
            (Self::List(a), Self::List(b)) => {
                let mut out = a.borrow().clone();
                out.extend(b.borrow().iter().cloned());
                Ok(Self::List(Shared::new(out)))
            }
            (Self::Tuple(a), Self::Tuple(b)) => {
                let mut out = a.to_vec();
                out.extend(b.iter().cloned());
                Ok(Self::Tuple(Arc::from(out.into_boxed_slice())))
            }
            (Self::Bytes(a), Self::Bytes(b)) => {
                let mut out = a.as_ref().clone();
                out.extend_from_slice(b.as_ref());
                Ok(Self::Bytes(Arc::new(out)))
            }
            (Self::Set(a), Self::Set(b)) => {
                let mut out = a.borrow().clone();
                for k in b.borrow().iter() {
                    out.insert(k.clone());
                }
                Ok(Self::Set(Shared::new(out)))
            }
            _ => Err(RuntimeError::unsupported(format!(
                "unsupported + between {} and {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }

    pub fn sub(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(sub_num(a, b))),
            _ => Err(RuntimeError::unsupported("unsupported - operation")),
        }
    }

    pub fn mul(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => Ok(Self::Num(mul_num(a, b))),
            _ => Err(RuntimeError::unsupported("unsupported * operation")),
        }
    }

    pub fn div(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => {
                let rb = b.to_rational();
                if rb.is_zero() {
                    return Err(RuntimeError::zero_div_diag());
                }
                let ra = a.to_rational();
                Ok(Self::Num(Num::from_rational(ra / rb)))
            }
            _ => Err(RuntimeError::unsupported("unsupported / operation")),
        }
    }

    value_num_binop! {
        pow, pow_num, "**",
        rem, rem_num, "%",
        bitand, bitand_num, "&",
        bitor, bitor_num, "|",
        bitxor, bitxor_num, "^",
        lshift, lshift_num, "<<",
        rshift, rshift_num, ">>",
    }

    pub fn neg(&self) -> Result<Self> {
        match self {
            Self::Num(n) => Ok(Self::Num(neg_num(n))),
            _ => Err(RuntimeError::unsupported("unsupported unary -")),
        }
    }

    pub fn invert(&self) -> Result<Self> {
        match self {
            Self::Num(n) => Ok(Self::Num(invert_num(n)?)),
            _ => Err(RuntimeError::unsupported("unsupported unary ~")),
        }
    }

    /// `is` / `is not` 的同一性比较（非 `==`）。
    #[must_use]
    pub fn identical(&self, other: &Self) -> bool {
        values_identical(self, other)
    }

    pub fn eq(&self, other: &Self) -> Result<bool> {
        match (self, other) {
            (Self::None, Self::None) => Ok(true),
            (Self::None, _) | (_, Self::None) => Ok(false),
            (Self::Bool(a), Self::Bool(b)) => Ok(a == b),
            (Self::Num(a), Self::Num(b)) => Ok(a.eq_num(b)),
            (Self::Sized(a), Self::Sized(b)) => Ok(a == b),
            (Self::Ptr(a), Self::Ptr(b)) => Ok(a == b),
            (Self::Text(a), Self::Text(b)) => Ok(a == b),
            (Self::TypeRef(a), Self::TypeRef(b)) => Ok(a == b),
            (Self::TypeRef(a), Self::Text(b)) | (Self::Text(b), Self::TypeRef(a)) => Ok(a == b),
            (Self::List(a), Self::List(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                if aa.len() != bb.len() {
                    return Ok(false);
                }
                for (x, y) in aa.iter().zip(bb.iter()) {
                    if !x.eq(y)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Self::Dict(a), Self::Dict(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                if aa.len() != bb.len() {
                    return Ok(false);
                }
                for (k, va) in aa.iter() {
                    match bb.get(k) {
                        Some(vb) if va.eq(vb)? => {}
                        _ => return Ok(false),
                    }
                }
                Ok(true)
            }
            (Self::Set(a), Self::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                if aa.len() != bb.len() {
                    return Ok(false);
                }
                let ok = aa.iter().all(|k| bb.contains(k));
                Ok(ok)
            }
            (Self::Tuple(a), Self::Tuple(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for (x, y) in a.iter().zip(b.iter()) {
                    if !x.eq(y)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Self::Bytes(a), Self::Bytes(b)) => Ok(a.as_ref() == b.as_ref()),
            (Self::Struct(a), Self::Struct(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::Iterator(a), Self::Iterator(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::RuntimeAst(a), Self::RuntimeAst(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::Dispatch(a), Self::Dispatch(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Macro(a), Self::Macro(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::Cell(a), Self::Cell(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Task(a), Self::Task(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Channel(a), Self::Channel(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Stream(a), Self::Stream(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Mutex(a), Self::Mutex(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::MutexGuard(a), Self::MutexGuard(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::Sync(a), Self::Sync(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::SyncGuard(a), Self::SyncGuard(b)) => Ok(Shared::ptr_eq(a, b)),
            (Self::TypeSpec(a), Self::TypeSpec(b)) => {
                Ok(a.as_ref() == b.as_ref())
            }
            (Self::EnumMember(a), Self::EnumMember(b)) => {
                Ok(Arc::ptr_eq(&a.def, &b.def) && a.member_index == b.member_index)
            }
            (Self::EnumMember(m), Self::Num(n)) | (Self::Num(n), Self::EnumMember(m)) => {
                Ok(crate::enum_variant::enum_member_numeric_value(m).eq_num(n))
            }
            (Self::Variant(a), Self::Variant(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::Layout(a), Self::Layout(b)) => Ok(Arc::ptr_eq(a, b) || a.strategy == b.strategy),
            (Self::Builtin(a), Self::Builtin(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::Function(a), Self::Function(b)) => Ok(Arc::ptr_eq(a, b)),
            (Self::GenericFunction(a), Self::GenericFunction(b)) => Ok(Arc::ptr_eq(a, b)),
            _ => Err(RuntimeError::unsupported(format!(
                "unsupported == between {} and {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }
}

/// `is` / `is not` 的同一性比较。
#[must_use]
pub fn values_identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::None, Value::None) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x.eq_num(y),
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::TypeRef(x), Value::TypeRef(y)) => x == y,
        (Value::List(x), Value::List(y)) => Shared::ptr_eq(x, y),
        (Value::Dict(x), Value::Dict(y)) => Shared::ptr_eq(x, y),
        (Value::Set(x), Value::Set(y)) => Shared::ptr_eq(x, y),
        (Value::Tuple(x), Value::Tuple(y)) => Arc::ptr_eq(x, y),
        (Value::Bytes(x), Value::Bytes(y)) => Arc::ptr_eq(x, y),
        (Value::Struct(x), Value::Struct(y)) => Arc::ptr_eq(x, y),
        (Value::Iterator(x), Value::Iterator(y)) => Shared::ptr_eq(x, y),
        (Value::RuntimeAst(x), Value::RuntimeAst(y)) => Arc::ptr_eq(x, y),
        (Value::Dispatch(x), Value::Dispatch(y)) => Shared::ptr_eq(x, y),
        (Value::Macro(x), Value::Macro(y)) => Arc::ptr_eq(x, y),
        (Value::Cell(x), Value::Cell(y)) => Shared::ptr_eq(x, y),
        (Value::Task(x), Value::Task(y)) => Shared::ptr_eq(x, y),
        (Value::Channel(x), Value::Channel(y)) => Shared::ptr_eq(x, y),
        (Value::Stream(x), Value::Stream(y)) => Shared::ptr_eq(x, y),
        (Value::Mutex(x), Value::Mutex(y)) => Shared::ptr_eq(x, y),
        (Value::MutexGuard(x), Value::MutexGuard(y)) => Shared::ptr_eq(x, y),
        (Value::Sync(x), Value::Sync(y)) => Shared::ptr_eq(x, y),
        (Value::SyncGuard(x), Value::SyncGuard(y)) => Shared::ptr_eq(x, y),
        (Value::Function(x), Value::Function(y)) => Arc::ptr_eq(x, y),
        (Value::GenericFunction(x), Value::GenericFunction(y)) => Arc::ptr_eq(x, y),
        (Value::Builtin(x), Value::Builtin(y)) => Arc::ptr_eq(x, y),
        (Value::Module(x), Value::Module(y)) => Shared::ptr_eq(x, y),
        (Value::TypeSpec(a), Value::TypeSpec(b)) => a.as_ref() == b.as_ref(),
        (Value::EnumMember(x), Value::EnumMember(y)) => Arc::ptr_eq(x, y),
        (Value::Variant(x), Value::Variant(y)) => Arc::ptr_eq(x, y),
        (Value::Layout(x), Value::Layout(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

fn add_num(a: &Num, b: &Num) -> Num {
    match (a, b) {
        (Num::Small(x), Num::Small(y)) => x
            .checked_add(*y).map_or_else(|| Num::from_bigint(BigInt::from(*x) + BigInt::from(*y)), Num::Small),
        (Num::Int(x), Num::Int(y)) => Num::from_bigint(x.as_ref() + y.as_ref()),
        (Num::Small(x), Num::Int(y)) => match y.to_i64() {
            Some(yi) => add_num(&Num::Small(*x), &Num::Small(yi)),
            None => Num::from_bigint(BigInt::from(*x) + y.as_ref()),
        },
        (Num::Int(x), Num::Small(y)) => match x.to_i64() {
            Some(xi) => add_num(&Num::Small(xi), &Num::Small(*y)),
            None => Num::from_bigint(x.as_ref() + BigInt::from(*y)),
        },
        _ => {
            let sum = a.to_rational() + b.to_rational();
            Num::from_rational(sum)
        }
    }
}

fn sub_num(a: &Num, b: &Num) -> Num {
    match (a, b) {
        (Num::Small(x), Num::Small(y)) => x
            .checked_sub(*y).map_or_else(|| Num::from_bigint(BigInt::from(*x) - BigInt::from(*y)), Num::Small),
        (Num::Int(x), Num::Int(y)) => Num::from_bigint(x.as_ref() - y.as_ref()),
        (Num::Small(x), Num::Int(y)) => match y.to_i64() {
            Some(yi) => sub_num(&Num::Small(*x), &Num::Small(yi)),
            None => Num::from_bigint(BigInt::from(*x) - y.as_ref()),
        },
        (Num::Int(x), Num::Small(y)) => match x.to_i64() {
            Some(xi) => sub_num(&Num::Small(xi), &Num::Small(*y)),
            None => Num::from_bigint(x.as_ref() - BigInt::from(*y)),
        },
        _ => {
            let diff = a.to_rational() - b.to_rational();
            Num::from_rational(diff)
        }
    }
}

fn mul_num(a: &Num, b: &Num) -> Num {
    match (a, b) {
        (Num::Small(x), Num::Small(y)) => x
            .checked_mul(*y).map_or_else(|| Num::from_bigint(BigInt::from(*x) * BigInt::from(*y)), Num::Small),
        (Num::Int(x), Num::Int(y)) => Num::from_bigint(x.as_ref() * y.as_ref()),
        (Num::Small(x), Num::Int(y)) => match y.to_i64() {
            Some(yi) => mul_num(&Num::Small(*x), &Num::Small(yi)),
            None => Num::from_bigint(BigInt::from(*x) * y.as_ref()),
        },
        (Num::Int(x), Num::Small(y)) => match x.to_i64() {
            Some(xi) => mul_num(&Num::Small(xi), &Num::Small(*y)),
            None => Num::from_bigint(x.as_ref() * BigInt::from(*y)),
        },
        _ => Num::from_rational(a.to_rational() * b.to_rational()),
    }
}

fn pow_num(base: &Num, exp: &Num) -> Result<Num> {
    use num_traits::Pow;
    let exp_r = exp.to_rational();
    if exp_r.denom() == &One::one() {
        let e = exp_r.numer();
        if e.is_negative() {
            let pos = Num::from_bigint(-e );
            let powered = pow_num(base, &pos)?;
            if powered.is_zero() {
                return Err(RuntimeError::zero_div("0.0 cannot be raised to a negative power"));
            }
            return Ok(Num::from_rational(BigRational::from_integer(One::one()) / powered.to_rational()));
        }
        if let Some(e_u32) = e.to_u32() {
            let base_r = base.to_rational();
            if base_r.denom() == &One::one() {
                return Ok(Num::from_bigint(base_r.numer().clone().pow(e_u32)));
            }
            let numer = base_r.numer().clone().pow(e_u32);
            let denom = base_r.denom().clone().pow(e_u32);
            return Ok(Num::from_rational(BigRational::new(numer, denom)));
        }
    }
    let a = base.to_f64_checked()?;
    let b = exp.to_f64_checked()?;
    let f = a.powf(b);
    BigRational::from_float(f)
        .map(Num::from_rational)
        .ok_or_else(|| RuntimeError::value_err("non-finite floating-point result"))
}

fn neg_num(n: &Num) -> Num {
    match n {
        Num::Small(i) => Num::Small(-*i),
        Num::Int(i) => Num::from_bigint(-i.as_ref()),
        Num::Rat(r) => Num::from_rational(-r.as_ref()),
    }
}

fn rem_num(a: &Num, b: &Num) -> Result<Num> {
    let ai = a.to_bigint()?;
    let bi = b.to_bigint()?;
    if bi.is_zero() {
        return Err(RuntimeError::zero_div_diag());
    }
    // Python-style：余数符号跟随除数
    let mut r = &ai % &bi;
    if (r.is_negative() && bi.is_positive()) || (r.is_positive() && bi.is_negative()) {
        r += &bi;
    }
    Ok(Num::from_bigint(r))
}

fn bitand_num(a: &Num, b: &Num) -> Result<Num> {
    Ok(Num::from_bigint(a.to_bigint()? & b.to_bigint()?))
}

fn bitor_num(a: &Num, b: &Num) -> Result<Num> {
    Ok(Num::from_bigint(a.to_bigint()? | b.to_bigint()?))
}

fn bitxor_num(a: &Num, b: &Num) -> Result<Num> {
    Ok(Num::from_bigint(a.to_bigint()? ^ b.to_bigint()?))
}

fn shift_amount(n: &Num) -> Result<u32> {
    let bi = n.to_bigint()?;
    if bi.is_negative() {
        return Err(RuntimeError::value_err("negative shift count"));
    }
    bi.to_u32()
        .ok_or_else(|| RuntimeError::value_err("shift count too large"))
}

fn lshift_num(a: &Num, b: &Num) -> Result<Num> {
    let amount = shift_amount(b)?;
    Ok(Num::from_bigint(a.to_bigint()? << amount))
}

fn rshift_num(a: &Num, b: &Num) -> Result<Num> {
    let amount = shift_amount(b)?;
    Ok(Num::from_bigint(a.to_bigint()? >> amount))
}

fn invert_num(n: &Num) -> Result<Num> {
    Ok(Num::from_bigint(!n.to_bigint()?))
}

fn parse_decimal_literal(text: &str) -> Result<BigRational> {
    let lower = text.trim().to_ascii_lowercase();
    if let Some((mantissa, exponent)) = lower.split_once('e') {
        let mant = parse_decimal_plain(mantissa)?;
        let exp: i32 = exponent
            .parse()
            .map_err(|_| RuntimeError::value_err("invalid scientific exponent"))?;
        let ten = BigRational::from((BigInt::from(10), One::one()));
        let scale = ten.pow(exp.unsigned_abs() as i32);
        if exp >= 0 {
            return Ok(mant * scale);
        }
        return Ok(mant / scale);
    }
    parse_decimal_plain(&lower)
}

fn parse_decimal_plain(text: &str) -> Result<BigRational> {
    let t = text.trim();
    if let Some((int_part, frac_part)) = t.split_once('.') {
        let int_digits = if int_part.is_empty() {
            "0".to_string()
        } else {
            int_part.to_string()
        };
        if !frac_part.chars().all(|c| c.is_ascii_digit()) {
            return Err(RuntimeError::value_err("invalid decimal fraction"));
        }
        if !int_digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
        {
            return Err(RuntimeError::value_err("invalid decimal integer part"));
        }
        let frac_len = frac_part.len();
        let combined = format!("{int_digits}{frac_part}");
        let numer: BigInt = combined
            .parse()
            .map_err(|_| RuntimeError::value_err("invalid decimal"))?;
        let denom = BigInt::from(10).pow(frac_len as u32);
        return Ok(BigRational::new(numer, denom));
    }
    let n: BigInt = t
        .parse()
        .map_err(|_| RuntimeError::value_err("invalid integer"))?;
    Ok(BigRational::from((n, One::one())))
}

impl IteratorState {
    #[must_use]
    pub const fn from_range(start: i64, stop: i64, stride: i64) -> Self {
        Self {
            kind: IteratorKind::Range {
                current: start,
                stop,
                step: stride,
            },
        }
    }

    #[must_use]
    pub const fn from_list(items: Vec<Value>) -> Self {
        Self {
            kind: IteratorKind::List { items, index: 0 },
        }
    }

    #[must_use]
    pub const fn from_zip(children: Vec<Shared<Self>>) -> Self {
        Self {
            kind: IteratorKind::Zip { children },
        }
    }

    #[must_use]
    pub fn into_value(self) -> Value {
        Value::Iterator(Shared::new(self))
    }

    pub fn next_value(&mut self, vm: &mut crate::vm::Vm) -> crate::Result<Option<Value>> {
        match &mut self.kind {
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
                Ok(Some(val))
            }
            IteratorKind::List { items, index } => {
                if *index >= items.len() {
                    return Ok(None);
                }
                let val = items[*index].clone();
                *index += 1;
                Ok(Some(val))
            }
            IteratorKind::Zip { children } => {
                let mut out = Vec::new();
                for child in children.iter() {
                    let mut c = child.borrow_mut();
                    match c.next_value(vm)? {
                        Some(v) => out.push(v),
                        None => return Ok(None),
                    }
                }
                Ok(Some(Value::List(Shared::new(out))))
            }
            IteratorKind::Map { func, source } => {
                let mut src = source.borrow_mut();
                match src.next_value(vm)? {
                    Some(item) => {
                        let mapped = vm.call_user_function(func.clone(), vec![item])?;
                        Ok(Some(mapped))
                    }
                    None => Ok(None),
                }
            },
            IteratorKind::Filter { func, source } => loop {
                let mut src = source.borrow_mut();
                match src.next_value(vm)? {
                    Some(item) => {
                        let keep = vm.call_user_function(func.clone(), vec![item.clone()])?;
                        if keep.is_truthy() {
                            return Ok(Some(item));
                        }
                    }
                    None => return Ok(None),
                }
            },
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
                loop {
                    let item = {
                        let mut src = source.borrow_mut();
                        src.next_value(vm)?
                    };
                    let Some(item) = item else {
                        return Ok(None);
                    };
                    let args = unpack_genexpr_args(item, arity)?;
                    let mut keep = true;
                    for g in &guards {
                        if !vm
                            .call_user_function(g.clone(), args.clone())?
                            .is_truthy()
                        {
                            keep = false;
                            break;
                        }
                    }
                    if keep {
                        return Ok(Some(vm.call_user_function(elem, args)?));
                    }
                }
            }
            IteratorKind::Repeat { value, remaining } => match remaining {
                None => Ok(Some(value.clone())),
                Some(0) => Ok(None),
                Some(n) => {
                    *n -= 1;
                    Ok(Some(value.clone()))
                }
            },
            IteratorKind::Cycle { items, index } => {
                if items.is_empty() {
                    return Ok(None);
                }
                let i = *index;
                *index = (i + 1) % items.len();
                Ok(Some(items[i].clone()))
            }
            IteratorKind::Channel { channel } => {
                let ch = channel.clone();
                let v = vm.channel_recv(&ch)?;
                if matches!(v, Value::None) && ch.borrow().closed {
                    Ok(None)
                } else {
                    Ok(Some(v))
                }
            }
            IteratorKind::Take { remaining, source } => {
                if *remaining == 0 {
                    return Ok(None);
                }
                let source = source.clone();
                let next = source.borrow_mut().next_value(vm)?;
                match next {
                    Some(v) => {
                        *remaining -= 1;
                        Ok(Some(v))
                    }
                    None => Ok(None),
                }
            }
            IteratorKind::Skip { remaining, source } => {
                let source = source.clone();
                while *remaining > 0 {
                    let next = source.borrow_mut().next_value(vm)?;
                    match next {
                        Some(_) => *remaining -= 1,
                        None => return Ok(None),
                    }
                }
                let next = source.borrow_mut().next_value(vm)?;
                Ok(next)
            }
            IteratorKind::Enumerate { index, source } => {
                let source = source.clone();
                let next = source.borrow_mut().next_value(vm)?;
                match next {
                    Some(item) => {
                        let i = *index;
                        *index = index.saturating_add(1);
                        Ok(Some(Value::List(Shared::new(vec![
                            Value::Num(Num::Small(i as i64)),
                            item,
                        ]))))
                    }
                    None => Ok(None),
                }
            }
            IteratorKind::Chain { sources, current } => loop {
                if *current >= sources.len() {
                    return Ok(None);
                }
                let src = sources[*current].clone();
                let next = src.borrow_mut().next_value(vm)?;
                match next {
                    Some(v) => return Ok(Some(v)),
                    None => *current += 1,
                }
            },
            IteratorKind::User { obj } => {
                let obj = obj.clone();
                match vm.try_call_magic(&obj, "__next__", vec![]) {
                    Some(Ok(v)) => Ok(Some(v)),
                    Some(Err(e))
                        if e.kind() == crate::error::ExceptionKind::StopIteration =>
                    {
                        vm.active_exception = None;
                        Ok(None)
                    }
                    Some(Err(e)) => Err(e),
                    None => Err(RuntimeError::type_err(
                        "iterator protocol requires __next__",
                    )),
                }
            }
            IteratorKind::Generator { .. } => Err(RuntimeError::msg(
                "internal: generator iteration must use Vm::advance_iterator",
            )),
        }
    }
}

/// 与 `value_to_iterable` 类似，但 Stream 拉取体与 Iterator 共享同一状态（不克隆游标）。
pub fn value_to_iterator_shared(v: &Value) -> crate::Result<Shared<IteratorState>> {
    match v {
        Value::Iterator(it) => Ok(it.clone()),
        Value::Stream(s) => match &*s.borrow() {
            StreamInner::Channel(ch) => Ok(Shared::new(IteratorState {
                kind: IteratorKind::Channel {
                    channel: ch.clone(),
                },
            })),
            StreamInner::Iter(it) => Ok(it.clone()),
        },
        other => Ok(Shared::new(value_to_iterable(other)?)),
    }
}

fn unpack_genexpr_args(item: Value, arity: usize) -> crate::Result<Vec<Value>> {
    if arity <= 1 {
        return Ok(vec![item]);
    }
    match item {
        Value::List(l) => {
            let items = l.borrow().clone();
            if items.len() != arity {
                return Err(crate::error::RuntimeError::msg(format!(
                    "generator expression expected {arity} values, got {}",
                    items.len()
                )));
            }
            Ok(items)
        }
        Value::Tuple(t) => {
            if t.len() != arity {
                return Err(crate::error::RuntimeError::msg(format!(
                    "generator expression expected {arity} values, got {}",
                    t.len()
                )));
            }
            Ok(t.to_vec())
        }
        _ => Err(crate::error::RuntimeError::type_err(
            "generator expression multi-binding requires list/tuple item",
        )),
    }
}

pub fn value_to_iterable(v: &Value) -> crate::Result<IteratorState> {
    match v {
        Value::Iterator(it) => Ok(it.borrow().clone()),
        Value::List(list) => Ok(IteratorState::from_list(list.borrow().clone())),
        Value::Tuple(t) => Ok(IteratorState::from_list(t.to_vec())),
        Value::Set(s) => {
            let items: Vec<Value> = s.borrow().iter().map(value_key_to_value).collect();
            Ok(IteratorState::from_list(items))
        }
        Value::Bytes(b) => Ok(IteratorState::from_list(
            b.iter()
                .map(|&byte| Value::Num(Num::Small(i64::from(byte))))
                .collect(),
        )),
        Value::Text(s) => Ok(IteratorState::from_list(
            s.chars().map(|c| Value::Text(c.to_string())).collect(),
        )),
        Value::Channel(ch) => Ok(IteratorState {
            kind: IteratorKind::Channel {
                channel: ch.clone(),
            },
        }),
        Value::Stream(s) => match &*s.borrow() {
            StreamInner::Channel(ch) => Ok(IteratorState {
                kind: IteratorKind::Channel {
                    channel: ch.clone(),
                },
            }),
            // 拉取游标不可克隆：调用方应走 `value_to_iterator_shared`。
            StreamInner::Iter(it) => Ok(it.borrow().clone()),
        },
        Value::Dict(d) => {
            let items: Vec<Value> = d
                .borrow()
                .iter()
                .map(|(k, v)| {
                    Value::List(Shared::new(vec![
                        value_key_to_value(k),
                        v.clone(),
                    ]))
                })
                .collect();
            Ok(IteratorState::from_list(items))
        }
        _ => Err(crate::error::RuntimeError::type_err("object is not iterable")),
    }
}

#[must_use]
pub fn value_key_to_value(k: &ValueKey) -> Value {
    match k {
        ValueKey::Bool(b) => Value::Bool(*b),
        ValueKey::NumInt(n) => match n.to_i64() {
            Some(i) => Value::Num(Num::Small(i)),
            None => Value::Num(Num::from_bigint(n.clone())),
        },
        ValueKey::Text(s) => Value::Text(s.clone()),
    }
}

pub fn hash_value(v: &Value) -> crate::Result<i64> {
    match v {
        Value::None => Ok(0x6a09_e667),
        Value::Bool(false) => Ok(0xbb67_ae85),
        Value::Bool(true) => Ok(0x3c6e_f372),
        Value::Num(Num::Small(n)) => Ok(*n),
        Value::Num(Num::Int(n)) => {
            use std::hash::{Hash, Hasher};
            let mut h = rustc_hash::FxHasher::default();
            n.hash(&mut h);
            Ok(h.finish() as i64)
        }
        Value::Num(Num::Rat(r)) => {
            use std::hash::{Hash, Hasher};
            let mut h = rustc_hash::FxHasher::default();
            r.numer().hash(&mut h);
            r.denom().hash(&mut h);
            Ok(h.finish() as i64)
        }
        Value::Text(s) => {
            let mut h: i64 = 0;
            for c in s.chars() {
                h = h.wrapping_mul(31).wrapping_add(c as i64);
            }
            Ok(h)
        }
        Value::Bytes(b) => {
            let mut h: i64 = 0;
            for &byte in b.iter() {
                h = h.wrapping_mul(31).wrapping_add(i64::from(byte));
            }
            Ok(h)
        }
        Value::Tuple(t) => {
            let mut h: i64 = 0x9e37_79b9;
            for elem in t.iter() {
                h = h.wrapping_mul(31).wrapping_add(hash_value(elem)?);
            }
            Ok(h)
        }
        Value::Struct(s) => Err(crate::error::RuntimeError::msg(format!(
            "unhashable type: {}",
            s.def.name
        ))),
        other => Err(crate::error::RuntimeError::msg(format!(
            "unhashable type: {}",
            other.type_name()
        ))),
    }
}

fn key_display(k: &ValueKey) -> String {
    match k {
        ValueKey::Bool(b) => b.to_string(),
        ValueKey::NumInt(n) => n.to_string(),
        ValueKey::Text(s) => format!("\"{s}\""),
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_string())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_string())
    }
}

/// 将值切片转为空格分隔的显示文本，供 `print`/`eprint` 等使用。
#[must_use]
pub fn args_join_space(args: &[Value]) -> String {
    args.iter().map(Value::print_string).collect::<Vec<_>>().join(" ")
}

/// 将 `Value` 解析为 `i64`（`WaitGroup.add`、`range`、`randint` 等共用）。
pub fn expect_i64(name: &str, v: &Value) -> Result<i64> {
    match v {
        Value::Num(n) => n
            .to_i64()
            .ok_or_else(|| RuntimeError::type_err(format!("{name}: expected integer"))),
        _ => Err(RuntimeError::type_err(format!("{name}: expected integer"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    #[test]
    fn num_from_int_literal() {
        let n = Num::from_literal("42").unwrap();
        assert_eq!(n.to_string(), "42");
    }

    #[test]
    fn num_from_negative() {
        let n = Num::from_literal("-7").unwrap();
        assert_eq!(n.to_string(), "-7");
    }

    #[test]
    fn num_from_rational() {
        let n = Num::from_literal("1/2").unwrap();
        assert_eq!(n.to_string(), "1/2");
    }

    #[test]
    fn num_is_zero() {
        assert!(Num::from_bigint(BigInt::from(0)).is_zero());
        assert!(!Num::from_bigint(BigInt::from(1)).is_zero());
    }

    #[test]
    fn value_truthy_none() {
        assert!(!Value::None.is_truthy());
    }

    #[test]
    fn value_truthy_empty_list() {
        assert!(!Value::List(Shared::new(vec![])).is_truthy());
    }

    #[test]
    fn value_add_nums() {
        let a = Value::Num(Num::Small(2));
        let b = Value::Num(Num::Small(3));
        assert_eq!(a.add(&b).unwrap().to_string(), "5");
    }

    #[test]
    fn value_add_strings() {
        let a = Value::Text("a".into());
        let b = Value::Text("b".into());
        assert_eq!(a.add(&b).unwrap().display_string(), "\"ab\"");
    }

    #[test]
    fn value_eq_ints() {
        let a = Value::Num(Num::Small(1));
        let b = Value::Num(Num::Small(1));
        assert!(a.eq(&b).unwrap());
    }

    #[test]
    fn value_key_from_text() {
        let v = Value::Text("k".into());
        assert!(ValueKey::from_value(&v).is_ok());
    }
}
