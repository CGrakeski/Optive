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

#[derive(Debug, Clone, PartialEq)]
pub enum Num {
    Small(i64),
    /// 堆上整数 — 放在 `Arc` 后以保持 `Value` 紧凑。
    Int(Arc<BigInt>),
    /// 堆上有理数 — 放在 `Arc` 后以保持 `Value` 紧凑。
    Rat(Arc<BigRational>),
}

impl Num {
    pub fn small(n: i64) -> Self {
        Num::Small(n)
    }

    pub fn from_i64(n: i64) -> Self {
        Self::small(n)
    }

    #[inline]
    pub fn from_bigint(n: BigInt) -> Self {
        match n.to_i64() {
            Some(i) => Num::Small(i),
            None => Num::Int(Arc::new(n)),
        }
    }

    #[inline]
    pub fn from_rational(r: BigRational) -> Self {
        if r.denom() == &One::one() {
            return Self::from_bigint(r.numer().clone());
        }
        Num::Rat(Arc::new(r))
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
            return Ok(Num::from_rational(BigRational::new(numer, denom)));
        }
        if t.contains('.') || t.contains('e') || t.contains('E') || t.starts_with('.') {
            let rat = parse_decimal_literal(t)
                .map_err(|_| RuntimeError::value_err(format!("invalid number literal: {text}")))?;
            return Ok(Num::from_rational(rat));
        }
        if let Ok(n) = t.parse::<i64>() {
            return Ok(Num::Small(n));
        }
        let n: BigInt = t
                .parse()
                .map_err(|_| RuntimeError::value_err(format!("invalid integer literal: {text}")))?;
        Ok(Num::from_bigint(n))
    }

    pub fn is_zero(&self) -> bool {
        match self {
            Num::Small(n) => *n == 0,
            Num::Int(n) => n.is_zero(),
            Num::Rat(r) => r.is_zero(),
        }
    }

    pub fn to_rational(&self) -> BigRational {
        match self {
            Num::Small(n) => BigRational::from((BigInt::from(*n), One::one())),
            Num::Int(n) => BigRational::from((n.as_ref().clone(), One::one())),
            Num::Rat(r) => r.as_ref().clone(),
        }
    }

    pub fn to_i64(&self) -> Option<i64> {
        match self {
            Num::Small(n) => Some(*n),
            Num::Int(n) => n.to_i64(),
            Num::Rat(r) if r.denom() == &One::one() => r.numer().to_i64(),
            _ => None,
        }
    }

    /// 转为整数 `BigInt`；有理数报错（按位/取模仅支持整数）。
    pub fn to_bigint(&self) -> Result<BigInt> {
        match self {
            Num::Small(n) => Ok(BigInt::from(*n)),
            Num::Int(n) => Ok(n.as_ref().clone()),
            Num::Rat(_) => Err(RuntimeError::type_err(
                "bitwise/modulo operators require integers, got rational",
            )),
        }
    }

    pub fn abs_num(&self) -> Num {
        match self {
            Num::Small(n) => Num::Small(n.abs()),
            Num::Int(i) => Num::from_bigint(i.abs()),
            Num::Rat(r) => Num::from_rational(r.abs()),
        }
    }

    pub fn floor_num(&self) -> Num {
        match self {
            Num::Small(n) => Num::Small(*n),
            Num::Int(n) => Num::Int(n.clone()),
            Num::Rat(r) => Num::from_bigint(r.floor().to_integer()),
        }
    }

    pub fn ceil_num(&self) -> Num {
        match self {
            Num::Small(n) => Num::Small(*n),
            Num::Int(n) => Num::Int(n.clone()),
            Num::Rat(r) => Num::from_bigint(r.ceil().to_integer()),
        }
    }

    pub fn trunc_num(&self) -> Num {
        match self {
            Num::Small(n) => Num::Small(*n),
            Num::Int(n) => Num::Int(n.clone()),
            Num::Rat(r) => Num::from_bigint(r.trunc().to_integer()),
        }
    }

    pub fn round_num(&self) -> Num {
        match self {
            Num::Small(n) => Num::Small(*n),
            Num::Int(n) => Num::Int(n.clone()),
            Num::Rat(r) => Num::from_bigint(r.round().to_integer()),
        }
    }

    pub fn to_f64_checked(&self) -> crate::Result<f64> {
        let r = self.to_rational();
        r.to_f64().ok_or_else(|| {
            crate::error::RuntimeError::value_err("number too large for floating-point conversion")
        })
    }

    pub fn eq_num(&self, other: &Self) -> bool {
        match (self, other) {
            (Num::Small(a), Num::Small(b)) => a == b,
            (Num::Int(a), Num::Int(b)) => a.as_ref() == b.as_ref(),
            (Num::Rat(a), Num::Rat(b)) => a.as_ref() == b.as_ref(),
            (Num::Small(a), Num::Int(b)) => match b.to_i64() {
                Some(bi) => a == &bi,
                None => self.to_rational() == other.to_rational(),
            },
            (Num::Int(a), Num::Small(b)) => match a.to_i64() {
                Some(ai) => ai == *b,
                None => self.to_rational() == other.to_rational(),
            },
            (Num::Small(a), Num::Rat(b)) if b.denom() == &One::one() => b
                .numer()
                .to_i64()
                .map(|bi| a == &bi)
                .unwrap_or_else(|| self.to_rational() == **b),
            (Num::Rat(a), Num::Small(b)) if a.denom() == &One::one() => a
                .numer()
                .to_i64()
                .map(|ai| ai == *b)
                .unwrap_or_else(|| **a == other.to_rational()),
            _ => self.to_rational() == other.to_rational(),
        }
    }

    pub fn cmp_num(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Num::Small(a), Num::Small(b)) => a.cmp(b),
            (Num::Int(a), Num::Int(b)) => a.as_ref().cmp(b.as_ref()),
            (Num::Rat(a), Num::Rat(b)) => a.as_ref().cmp(b.as_ref()),
            (Num::Small(a), Num::Int(b)) => match b.to_i64() {
                Some(bi) => a.cmp(&bi),
                None => self.to_rational().cmp(&other.to_rational()),
            },
            (Num::Int(a), Num::Small(b)) => match a.to_i64() {
                Some(ai) => ai.cmp(b),
                None => self.to_rational().cmp(&other.to_rational()),
            },
            (Num::Small(a), Num::Rat(b)) if b.denom() == &One::one() => {
                if let Some(bi) = b.numer().to_i64() {
                    a.cmp(&bi)
                } else {
                    self.to_rational().cmp(b.as_ref())
                }
            }
            (Num::Rat(a), Num::Small(b)) if a.denom() == &One::one() => {
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
            Num::Small(n) => write!(f, "{n}"),
            Num::Int(n) => write!(f, "{n}"),
            Num::Rat(r) => {
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

#[derive(Clone)]
pub struct ModuleObject {
    pub name: String,
    pub full_name: String,
    pub exports: HashMap<String, Value>,
    pub children: HashMap<String, Shared<ModuleObject>>,
    pub is_user: bool,
}

impl ModuleObject {
    pub fn new_user(name: String, full_name: String) -> Self {
        Self {
            name,
            full_name,
            exports: HashMap::new(),
            children: HashMap::new(),
            is_user: true,
        }
    }

    pub fn get_export(&self, name: &str) -> Option<Value> {
        self.exports.get(name).cloned()
    }

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

    pub fn len(&self) -> usize {
        self.map.len()
    }

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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: ValueKey, val: Value) {
        if !self.map.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.map.insert(key, val);
    }

    pub fn get(&self, key: &ValueKey) -> Option<&Value> {
        self.map.get(key)
    }

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

    pub fn len(&self) -> usize {
        self.map.len()
    }

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

#[derive(Clone)]
pub struct TaskInner {
    pub state: TaskState,
}

impl TaskInner {
    pub fn pending(callable: Value, args: Vec<Value>) -> Self {
        Self {
            state: TaskState::Pending { callable, args },
        }
    }

    pub fn done(value: Value) -> Self {
        Self {
            state: TaskState::Done(value),
        }
    }
}

/// `capacity`: `None` = 无界；`Some(0)` = rendezvous；`Some(n)` = 有界 n。
#[derive(Clone)]
pub struct ChannelInner {
    pub queue: VecDeque<Value>,
    pub capacity: Option<usize>,
    pub closed: bool,
}

impl ChannelInner {
    pub fn new(capacity: Option<usize>) -> Self {
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
    pub fn new(value: Value) -> Self {
        Self {
            value,
            locked: false,
        }
    }
}

/// `Once.run` 生命周期。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OncePhase {
    Idle,
    Running,
    Done,
}

/// 其余并发原语（RWMutex / WaitGroup / Semaphore / Once / Barrier / Cond）的统一载荷。
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
}

/// RWMutex 的读/写守卫，支持 `with` 自动释放。
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
    List(Shared<Vec<Value>>),
    Dict(Shared<DictMap>),
    /// 可哈希值的有序集合。空集用 `set()`，不是 `{}`（空字典）。
    Set(Shared<SetMap>),
    /// 不可变定长序列。空元组为 `()`。
    Tuple(Arc<[Value]>),
    /// 原始字节缓冲（非 Unicode 文本）。
    Bytes(Arc<Vec<u8>>),
    Iterator(Shared<IteratorState>),
    Function(Arc<FunctionObject>),
    GenericFunction(Arc<crate::opcode::GenericFunctionTemplate>),
    Macro(Arc<MacroObject>),
    Builtin(BuiltinFn),
    Struct(Arc<StructInstance>),
    Module(Shared<ModuleObject>),
    RuntimeAst(Arc<RuntimeAstNode>),
    Dispatch(Shared<DispatchTable>),
    Cell(Shared<Value>),
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
    /// 互斥锁（`Mutex(v)`）。
    Mutex(Shared<MutexInner>),
    /// `Mutex.lock()` 得到的守卫，可用于 `with`。
    MutexGuard(Shared<MutexInner>),
    /// 其余并发原语（RWMutex/WaitGroup/Semaphore/Once/Barrier/Cond）。
    Sync(Shared<SyncInner>),
    /// RWMutex 读/写守卫。
    SyncGuard(Shared<SyncGuardInner>),
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
            Value::Bool(b) => Ok(ValueKey::Bool(*b)),
            Value::Num(Num::Small(n)) => Ok(ValueKey::NumInt((*n).into())),
            Value::Num(Num::Int(n)) => Ok(ValueKey::NumInt(n.as_ref().clone())),
            Value::Text(s) => Ok(ValueKey::Text(s.clone())),
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
}

#[derive(Clone)]
pub struct EnumMemberData {
    pub def: Arc<EnumDef>,
    pub member_index: usize,
    pub type_name: String,
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
    /// `typed struct … : C.layout` 时填充；供 `C.load` / `C.store` / `C.alloc(T)`。
    pub c_layout: Option<std::sync::Arc<crate::ffi_extra::CStructLayout>>,
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
        Value::Text(s.into())
    }

    pub fn type_ref(name: impl Into<String>) -> Self {
        Value::TypeRef(name.into())
    }

    /// 索引 / 注解用的类型名操作数（`TypeRef`，以及旧式 `Text`）。
    pub fn as_type_name_operand(&self) -> Option<&str> {
        match self {
            Value::TypeRef(n) => Some(n.as_str()),
            Value::Text(n) => Some(n.as_str()),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &str {
        match self {
            Value::None => "nonetype",
            Value::Bool(_) => "bool",
            Value::Num(_) => "num",
            Value::Sized(s) => s.type_name(),
            Value::Ptr(_) => "ptr",
            Value::DllHandle(_) => "DllHandle",
            Value::Text(_) => "text",
            // 类型句柄的值属于元类型 `type`（如 `type(A)` → type，`type(A())` → A）。
            Value::TypeRef(_) => "type",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Set(_) => "set",
            Value::Tuple(_) => "tuple",
            Value::Bytes(_) => "bytes",
            Value::Iterator(_) => "iterator",
            Value::Function(_) => "function",
            Value::GenericFunction(_) => "generic function",
            Value::Macro(_) => "Macro",
            Value::Builtin(_) => "function",
            Value::Dispatch(_) => "friend func",
            Value::Struct(s) => &s.def.name,
            Value::Module(_) => "module",
            Value::RuntimeAst(_) => "AST",
            Value::Cell(_) => "cell",
            Value::TypeSpec(_) => "type",
            Value::EnumMember(m) => &m.type_name,
            Value::Variant(v) => &v.inst_name,
            Value::Task(_) => "Task",
            Value::Channel(_) => "Channel",
            Value::Mutex(_) => "Mutex",
            Value::MutexGuard(_) => "MutexGuard",
            Value::Sync(s) => match &*s.borrow() {
                SyncInner::RWMutex { .. } => "RWMutex",
                SyncInner::WaitGroup { .. } => "WaitGroup",
                SyncInner::Semaphore { .. } => "Semaphore",
                SyncInner::Once { .. } => "Once",
                SyncInner::Barrier { .. } => "Barrier",
                SyncInner::Cond { .. } => "Cond",
            },
            Value::SyncGuard(g) => match &*g.borrow() {
                SyncGuardInner::Read { .. } => "RWMutexReadGuard",
                SyncGuardInner::Write { .. } => "RWMutexWriteGuard",
            },
        }
    }

    pub fn type_name_string(&self) -> String {
        self.type_name().to_string()
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Num(n) => !n.is_zero(),
            Value::Sized(s) => s.is_truthy(),
            Value::Ptr(p) => *p != 0,
            Value::DllHandle(_) => true,
            Value::Text(s) => !s.is_empty(),
            Value::List(v) => !v.borrow().is_empty(),
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::Cell(c) => c.borrow().is_truthy(),
            _ => true,
        }
    }

    pub fn display_string(&self) -> String {
        match self {
            Value::None => "none".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => n.to_string(),
            Value::Sized(s) => s.display_string(),
            Value::Ptr(p) => format!("ptr(0x{p:x})"),
            Value::DllHandle(h) => format!("<DllHandle {}>", h.path),
            Value::Text(s) => format!("\"{s}\""),
            Value::TypeRef(n) => n.clone(),
            Value::List(v) => {
                let parts: Vec<_> = v.borrow().iter().map(|x| x.display_string()).collect();
                format!("[{}]", parts.join(", "))
            }
            Value::Dict(d) => {
                let parts: Vec<_> = d
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("{}: {}", key_display(k), v.display_string()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Value::Set(s) => {
                let borrowed = s.borrow();
                if borrowed.is_empty() {
                    "set()".to_string()
                } else {
                    let parts: Vec<_> = borrowed.iter().map(key_display).collect();
                    format!("{{{}}}", parts.join(", "))
                }
            }
            Value::Tuple(t) => {
                let parts: Vec<_> = t.iter().map(|x| x.display_string()).collect();
                if t.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Value::Bytes(b) => {
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
            Value::Iterator(_) => "<iterator>".to_string(),
            Value::Function(f) => format!("<function {}>", f.name),
            Value::GenericFunction(g) => format!("<generic function {}>", g.name),
            Value::Macro(m) => format!("<macro {}>", m.name),
            Value::Dispatch(d) => format!("<friend func {}>", d.borrow().name),
            Value::Builtin(_) => "<builtin function>".to_string(),
            Value::Module(m) => format!("<module {}>", m.borrow().full_name),
            Value::Struct(s) => {
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
            Value::RuntimeAst(_) => "<AST>".to_string(),
            Value::Cell(c) => c.borrow().display_string(),
            Value::TypeSpec(spec) => {
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
            Value::EnumMember(m) => format!(
                "{}.{}",
                m.def.name, m.def.members[m.member_index].name
            ),
            Value::Variant(v) => format!(
                "{}({})",
                v.inst_name,
                v.payload.display_string()
            ),
            Value::Task(_) => "<Task>".to_string(),
            Value::Channel(_) => "<Channel>".to_string(),
            Value::Mutex(_) => "<Mutex>".to_string(),
            Value::MutexGuard(_) => "<MutexGuard>".to_string(),
            Value::Sync(s) => match &*s.borrow() {
                SyncInner::RWMutex { .. } => "<RWMutex>".to_string(),
                SyncInner::WaitGroup { .. } => "<WaitGroup>".to_string(),
                SyncInner::Semaphore { .. } => "<Semaphore>".to_string(),
                SyncInner::Once { .. } => "<Once>".to_string(),
                SyncInner::Barrier { .. } => "<Barrier>".to_string(),
                SyncInner::Cond { .. } => "<Cond>".to_string(),
            },
            Value::SyncGuard(g) => match &*g.borrow() {
                SyncGuardInner::Read { .. } => "<RWMutexReadGuard>".to_string(),
                SyncGuardInner::Write { .. } => "<RWMutexWriteGuard>".to_string(),
            },
        }
    }

    pub fn print_string(&self) -> String {
        match self {
            Value::Text(s) => s.clone(),
            other => other.display_string(),
        }
    }

    pub fn add(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(add_num(a, b))),
            (Value::Text(a), Value::Text(b)) => Ok(Value::Text(format!("{a}{b}"))),
            (Value::List(a), Value::List(b)) => {
                let mut out = a.borrow().clone();
                out.extend(b.borrow().iter().cloned());
                Ok(Value::List(Shared::new(out)))
            }
            (Value::Tuple(a), Value::Tuple(b)) => {
                let mut out = a.to_vec();
                out.extend(b.iter().cloned());
                Ok(Value::Tuple(Arc::from(out.into_boxed_slice())))
            }
            (Value::Bytes(a), Value::Bytes(b)) => {
                let mut out = a.as_ref().clone();
                out.extend_from_slice(b.as_ref());
                Ok(Value::Bytes(Arc::new(out)))
            }
            (Value::Set(a), Value::Set(b)) => {
                let mut out = a.borrow().clone();
                for k in b.borrow().iter() {
                    out.insert(k.clone());
                }
                Ok(Value::Set(Shared::new(out)))
            }
            _ => Err(RuntimeError::unsupported(format!(
                "unsupported + between {} and {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }

    pub fn sub(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(sub_num(a, b))),
            _ => Err(RuntimeError::unsupported("unsupported - operation")),
        }
    }

    pub fn mul(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(mul_num(a, b))),
            _ => Err(RuntimeError::unsupported("unsupported * operation")),
        }
    }

    pub fn div(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => {
                let rb = b.to_rational();
                if rb.is_zero() {
                    return Err(RuntimeError::zero_div("division by zero"));
                }
                let ra = a.to_rational();
                Ok(Value::Num(Num::from_rational(ra / rb)))
            }
            _ => Err(RuntimeError::unsupported("unsupported / operation")),
        }
    }

    pub fn pow(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(pow_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported ** operation")),
        }
    }

    pub fn rem(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(rem_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported % operation")),
        }
    }

    pub fn bitand(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(bitand_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported & operation")),
        }
    }

    pub fn bitor(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(bitor_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported | operation")),
        }
    }

    pub fn bitxor(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(bitxor_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported ^ operation")),
        }
    }

    pub fn lshift(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(lshift_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported << operation")),
        }
    }

    pub fn rshift(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => Ok(Value::Num(rshift_num(a, b)?)),
            _ => Err(RuntimeError::unsupported("unsupported >> operation")),
        }
    }

    pub fn neg(&self) -> Result<Value> {
        match self {
            Value::Num(n) => Ok(Value::Num(neg_num(n))),
            _ => Err(RuntimeError::unsupported("unsupported unary -")),
        }
    }

    pub fn invert(&self) -> Result<Value> {
        match self {
            Value::Num(n) => Ok(Value::Num(invert_num(n)?)),
            _ => Err(RuntimeError::unsupported("unsupported unary ~")),
        }
    }

    /// `is` / `is not` 的同一性比较（非 `==`）。
    pub fn identical(&self, other: &Value) -> bool {
        values_identical(self, other)
    }

    pub fn eq(&self, other: &Value) -> Result<bool> {
        match (self, other) {
            (Value::None, Value::None) => Ok(true),
            (Value::None, _) | (_, Value::None) => Ok(false),
            (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
            (Value::Num(a), Value::Num(b)) => Ok(a.eq_num(b)),
            (Value::Sized(a), Value::Sized(b)) => Ok(a == b),
            (Value::Ptr(a), Value::Ptr(b)) => Ok(a == b),
            (Value::Text(a), Value::Text(b)) => Ok(a == b),
            (Value::TypeRef(a), Value::TypeRef(b)) => Ok(a == b),
            (Value::TypeRef(a), Value::Text(b)) | (Value::Text(b), Value::TypeRef(a)) => Ok(a == b),
            (Value::List(a), Value::List(b)) => {
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
            (Value::Dict(a), Value::Dict(b)) => {
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
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                if aa.len() != bb.len() {
                    return Ok(false);
                }
                let ok = aa.iter().all(|k| bb.contains(k));
                Ok(ok)
            }
            (Value::Tuple(a), Value::Tuple(b)) => {
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
            (Value::Bytes(a), Value::Bytes(b)) => Ok(a.as_ref() == b.as_ref()),
            (Value::Struct(a), Value::Struct(b)) => Ok(Arc::ptr_eq(a, b)),
            (Value::Iterator(a), Value::Iterator(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::RuntimeAst(a), Value::RuntimeAst(b)) => Ok(Arc::ptr_eq(a, b)),
            (Value::Dispatch(a), Value::Dispatch(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::Macro(a), Value::Macro(b)) => Ok(Arc::ptr_eq(a, b)),
            (Value::Cell(a), Value::Cell(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::Task(a), Value::Task(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::Channel(a), Value::Channel(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::Mutex(a), Value::Mutex(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::MutexGuard(a), Value::MutexGuard(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::Sync(a), Value::Sync(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::SyncGuard(a), Value::SyncGuard(b)) => Ok(Shared::ptr_eq(a, b)),
            (Value::TypeSpec(a), Value::TypeSpec(b)) => {
                Ok(a.as_ref() == b.as_ref())
            }
            (Value::EnumMember(a), Value::EnumMember(b)) => {
                Ok(Arc::ptr_eq(&a.def, &b.def) && a.member_index == b.member_index)
            }
            (Value::EnumMember(m), Value::Num(n)) | (Value::Num(n), Value::EnumMember(m)) => {
                Ok(crate::enum_variant::enum_member_numeric_value(m).eq_num(n))
            }
            (Value::Variant(a), Value::Variant(b)) => Ok(Arc::ptr_eq(a, b)),
            _ => Err(RuntimeError::unsupported(format!(
                "unsupported == between {} and {}",
                self.type_name(),
                other.type_name()
            ))),
        }
    }
}

/// `is` / `is not` 的同一性比较。
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
        (Value::Mutex(x), Value::Mutex(y)) => Shared::ptr_eq(x, y),
        (Value::MutexGuard(x), Value::MutexGuard(y)) => Shared::ptr_eq(x, y),
        (Value::Sync(x), Value::Sync(y)) => Shared::ptr_eq(x, y),
        (Value::SyncGuard(x), Value::SyncGuard(y)) => Shared::ptr_eq(x, y),
        (Value::Function(x), Value::Function(y)) => Arc::ptr_eq(x, y),
        (Value::GenericFunction(x), Value::GenericFunction(y)) => Arc::ptr_eq(x, y),
        (Value::Builtin(x), Value::Builtin(y)) => std::ptr::eq(x, y),
        (Value::Module(x), Value::Module(y)) => Shared::ptr_eq(x, y),
        (Value::TypeSpec(a), Value::TypeSpec(b)) => a.as_ref() == b.as_ref(),
        (Value::EnumMember(x), Value::EnumMember(y)) => Arc::ptr_eq(x, y),
        (Value::Variant(x), Value::Variant(y)) => Arc::ptr_eq(x, y),
        _ => false,
    }
}

fn add_num(a: &Num, b: &Num) -> Num {
    match (a, b) {
        (Num::Small(x), Num::Small(y)) => x
            .checked_add(*y)
            .map(Num::Small)
            .unwrap_or_else(|| Num::from_bigint(BigInt::from(*x) + BigInt::from(*y))),
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
            .checked_sub(*y)
            .map(Num::Small)
            .unwrap_or_else(|| Num::from_bigint(BigInt::from(*x) - BigInt::from(*y))),
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
            .checked_mul(*y)
            .map(Num::Small)
            .unwrap_or_else(|| Num::from_bigint(BigInt::from(*x) * BigInt::from(*y))),
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
            let pos = Num::from_bigint((-e).clone());
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
        return Err(RuntimeError::zero_div("division by zero"));
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
    pub fn from_range(start: i64, stop: i64, step: i64) -> Self {
        Self {
            kind: IteratorKind::Range {
                current: start,
                stop,
                step,
            },
        }
    }

    pub fn from_list(items: Vec<Value>) -> Self {
        Self {
            kind: IteratorKind::List { items, index: 0 },
        }
    }

    pub fn from_zip(children: Vec<Shared<IteratorState>>) -> Self {
        Self {
            kind: IteratorKind::Zip { children },
        }
    }

    pub fn as_value(self) -> Value {
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
            IteratorKind::Generator { .. } => Err(RuntimeError::msg(
                "internal: generator iteration must use Vm::advance_iterator",
            )),
        }
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
                .map(|&byte| Value::Num(Num::Small(byte as i64)))
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
                h = h.wrapping_mul(31).wrapping_add(byte as i64);
            }
            Ok(h)
        }
        Value::Tuple(t) => {
            let mut h: i64 = 0x9e3779b9;
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
pub fn args_join_space(args: &[Value]) -> String {
    args.iter().map(|v| v.print_string()).collect::<Vec<_>>().join(" ")
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
