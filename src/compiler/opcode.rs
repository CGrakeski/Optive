use std::collections::HashMap;
use std::sync::Arc;

use crate::ast::FuncParam;
use crate::shared::SyncCell;
use crate::value::Value;

#[derive(Debug, Clone)]
pub enum Instruction {
    Push(Value),
    /// 压入小整数常量，避免克隆完整 `Value`。
    PushSmall(i64),
    Pop,
    Add,
    /// 已证两侧为 `Num` 的加法（可走无标签分发的快路径）。
    AddNumNum,
    /// 已证两侧为 `Text` 的拼接。
    AddTextText,
    /// 已证两侧为 `List` 的拼接。
    AddListList,
    Sub,
    /// 已证两侧为 `Num` 的减法。
    SubNumNum,
    Mul,
    /// 已证两侧为 `Num` 的乘法。
    MulNumNum,
    Div,
    /// 已证两侧为 `Num` 的除法。
    DivNumNum,
    Mod,
    /// 已证两侧为 `Num` 的取模。
    ModNumNum,
    Pow,
    /// 已证两侧为 `Num` 的幂运算。
    PowNumNum,
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
    /// 已证两侧为 `Num` 的相等比较。
    EqNumNum,
    Ne,
    NeNumNum,
    Lt,
    LtNumNum,
    Le,
    LeNumNum,
    Gt,
    GtNumNum,
    Ge,
    GeNumNum,
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
    /// 融合：`LoadFast(slot); PushSmall(imm); Sub` → 压入 `lw_slots[slot] - imm`。
    LoadFastSubImm { slot: usize, imm: i64 },
    /// 融合：`LoadFast(slot); PushSmall(imm); Le` → 压入 `lw_slots[slot] <= imm`。
    LoadFastLeImm { slot: usize, imm: i64 },
    BindFast {
        slot: usize,
        name: String,
        is_const: bool,
    },
    EnterScope,
    LeaveScope,
    Label(usize),
    Goto(usize),
    GotoIf(usize),
    GotoIfNot(usize),
    /// 计数循环：栈顶为计数器。`<= 0` 时弹出并跳到目标；否则计数器减 1。
    LoopCountdown(usize),
    Call { argc: usize },
    CallSelf { argc: usize },
    CallList,
    /// 扩展调用：栈为 `args_list, kwargs_dict, callee`。
    CallEx,
    MacroCall { argc: usize },
    ListAppend,
    ListExtend,
    /// `dict[key] = val`，栈：dict, key, val → dict（就地写入并留下 dict）。
    DictSet,
    /// `set.add(val)`，栈：set, val → set。
    SetAdd,
    Ret,
    /// 直接返回快局部槽，无需先压栈再 Ret。
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
    /// 将 list/tuple 按精确长度拆到栈上（先压入的元素在栈底，末元素在栈顶）。
    UnpackExact(usize),
    /// 将 list/tuple 拆为 `before` + rest(list) + `after`；栈顶为最后一个 after 元素。
    UnpackRest { before: usize, after: usize },
    Rethrow,
    /// 栈：… value, type_val → … value（硬检查；type_val 须为类型）。
    TypeCheck,
    /// 栈顶 `Function`：在定义处求值并绑定所有参数/返回类型注解（须为类型值）。
    ResolveFuncTypes,
    FindMod(String),
    RegisterExport(String),
    /// `go f(args)`：栈为 args… + callee → Task。
    GoCall(usize),
    /// 将栈顶值包装为已完成的 Task。
    GoValue,
    /// 若为 Task 则 join；否则原样留下。
    Await,
    /// 协作式挂起（`suspend` / 调度器预算）。
    Suspend,
    /// 生成器产出：弹出栈顶作为下一迭代值并挂起。
    Yield,
    /// 生成器委托：弹出可迭代对象，逐项产出。
    YieldFrom,
    /// 非阻塞试收：Channel → (value?, ready:bool)。ready 时栈顶为 bool，其下为值（关闭则为 none）。
    SelectTryRecv,
    /// 非阻塞试发：Channel, value → ready:bool。
    SelectTrySend,
    /// 轮询 Task：Task → (value?, ready:bool)。Failed 时抛出。
    SelectPollTask,
    /// 秒数 → 截止时间戳（毫秒）。
    MakeDeadline,
    /// 截止时间戳 → ready:bool。
    SelectPollDeadline,
}

/// 将跳转指令中的标签 id 就地解析为绝对 PC。
pub fn resolve_labels_in_place(code: &mut [Instruction]) -> Result<(), String> {
    use std::collections::HashMap;

    let mut labels = HashMap::new();
    for (i, ins) in code.iter().enumerate() {
        if let Instruction::Label(id) = ins {
            labels.insert(*id, i);
        }
    }

    let resolve = |label_id: usize| -> Result<usize, String> {
        labels
            .get(&label_id)
            .copied()
            .ok_or_else(|| format!("undefined label {label_id}"))
    };

    for ins in code.iter_mut() {
        match ins {
            Instruction::Goto(target)
            | Instruction::GotoIf(target)
            | Instruction::GotoIfNot(target)
            | Instruction::LoopCountdown(target) => {
                *target = resolve(*target)?;
            }
            Instruction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                *catch_label = resolve(*catch_label)?;
                if *else_label != 0 {
                    *else_label = resolve(*else_label)?;
                }
                *end_label = resolve(*end_label)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// 字节码紧化：在 `resolve_labels_in_place` 之后运行。
///
/// 1. 将所有跳转目标解析到 Label 链之后的真实指令（跳过连续 Label）。
/// 2. 移除 `Label` 空操作。
/// 3. 移除终止指令（`Ret`/`RetFast`/`RetLeave`/`Throw`）之后的死 `Goto`。
/// 4. 移除跳转到下一条指令的冗余 `Goto`。
/// 5. 重映射所有跳转目标到紧化后的 PC。
///
/// 必须在 `specialize_*` 之后调用（specialize 依赖 `Label` 清空标签栈）。
///
/// 返回紧化后的代码及 `old_pc → new_pc` 映射（长度 n+1，含哨兵），
/// 供调用方同步紧化 `line_map` / `column_map`。
pub fn compact_bytecode(code: Vec<Instruction>) -> (Vec<Instruction>, Vec<usize>) {
    let n = code.len();
    if n == 0 {
        return (code, vec![0]);
    }

    // 1. 为每个 Label PC 计算其后第一条非 Label 指令的 PC。
    let mut label_target = vec![0usize; n];
    let mut next_real = n; // 越界 = 跳到代码末尾（等价于退出）
    for i in (0..n).rev() {
        if matches!(code[i], Instruction::Label(_)) {
            label_target[i] = next_real;
        } else {
            label_target[i] = i;
            next_real = i;
        }
    }

    // 2. 解析跳转目标：如果目标是 Label，跳到 label_target。
    let resolve = |pc: usize| -> usize {
        if pc >= n {
            return pc;
        }
        if matches!(code[pc], Instruction::Label(_)) {
            label_target[pc]
        } else {
            pc
        }
    };

    // 3. 标记需要移除的指令。
    let mut keep = vec![true; n];
    for i in 0..n {
        if matches!(code[i], Instruction::Label(_)) {
            keep[i] = false;
        }
    }
    // 移除终止指令后的死 Goto。
    let mut prev_terminator = false;
    for i in 0..n {
        if prev_terminator && matches!(code[i], Instruction::Goto(_)) {
            keep[i] = false;
        }
        prev_terminator = matches!(
            code[i],
            Instruction::Ret | Instruction::RetFast(_) | Instruction::RetLeave | Instruction::Throw
        );
    }

    // 4. 构建 old_pc → new_pc 映射（含 n → new_len 的哨兵）。
    let mut new_pc = vec![0usize; n + 1];
    let mut count = 0;
    for i in 0..n {
        new_pc[i] = count;
        if keep[i] {
            count += 1;
        }
    }
    new_pc[n] = count;
    let new_len = count;

    // 5. 生成紧化后的代码，重映射跳转目标。
    let mut result = Vec::with_capacity(new_len);
    for i in 0..n {
        if !keep[i] {
            continue;
        }
        let mut ins = code[i].clone();
        match &mut ins {
            Instruction::Goto(t)
            | Instruction::GotoIf(t)
            | Instruction::GotoIfNot(t)
            | Instruction::LoopCountdown(t) => {
                let resolved = resolve(*t);
                *t = if resolved >= n {
                    new_len
                } else {
                    new_pc[resolved]
                };
            }
            Instruction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                *catch_label = new_pc[resolve(*catch_label)];
                if *else_label != 0 {
                    *else_label = new_pc[resolve(*else_label)];
                }
                *end_label = new_pc[resolve(*end_label)];
            }
            _ => {}
        }
        result.push(ins);
    }

    // 6. 第二遍：移除跳转到下一条指令的冗余 Goto。
    if result.is_empty() {
        return (result, new_pc);
    }
    let m = result.len();
    let mut keep2 = vec![true; m];
    for i in 0..m.saturating_sub(1) {
        if let Instruction::Goto(t) = &result[i] {
            if *t == i + 1 {
                keep2[i] = false;
            }
        }
    }
    // 重建紧化映射。
    let mut new_pc2 = vec![0usize; m + 1];
    let mut count2 = 0;
    for i in 0..m {
        new_pc2[i] = count2;
        if keep2[i] {
            count2 += 1;
        }
    }
    new_pc2[m] = count2;
    let final_len = count2;

    let mut final_result = Vec::with_capacity(final_len);
    for i in 0..m {
        if !keep2[i] {
            continue;
        }
        let mut ins = result[i].clone();
        match &mut ins {
            Instruction::Goto(t)
            | Instruction::GotoIf(t)
            | Instruction::GotoIfNot(t)
            | Instruction::LoopCountdown(t) => {
                if *t < m {
                    *t = new_pc2[*t];
                } else {
                    *t = final_len;
                }
            }
            Instruction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                *catch_label = new_pc2[*catch_label];
                if *else_label != 0 {
                    *else_label = new_pc2[*else_label];
                }
                *end_label = new_pc2[*end_label];
            }
            _ => {}
        }
        final_result.push(ins);
    }

    // 7. 合并 old_pc → final_pc 映射（两遍紧化叠加）。
    let mut final_map = vec![0usize; n + 1];
    let mut idx = 0;
    for i in 0..n {
        final_map[i] = if keep[i] {
            let mid = new_pc[i];
            let v = if mid < m {
                new_pc2[mid]
            } else {
                final_len
            };
            idx = v;
            v
        } else {
            idx
        };
    }
    final_map[n] = final_len;

    (final_result, final_map)
}

/// 用 `old_pc → new_pc` 映射紧化并行数组（line_map / column_map）。
pub fn compact_parallel(map: &[usize], old_to_new: &[usize]) -> Vec<usize> {
    let n = map.len();
    if n == 0 || old_to_new.len() <= n {
        return map.to_vec();
    }
    let new_len = old_to_new[n];
    let mut result = Vec::with_capacity(new_len);
    let mut last_val = 0usize;
    for i in 0..n {
        let new_pc = old_to_new[i];
        while result.len() < new_pc {
            result.push(last_val);
        }
        if new_pc < new_len {
            last_val = map[i];
            result.push(map[i]);
        }
    }
    while result.len() < new_len {
        result.push(last_val);
    }
    result
}

/// 融合窥孔：检测 `LoadFast(s); PushSmall(n); Sub/Le` 三连模式并替换为
/// `LoadFastSubImm` / `LoadFastLeImm` 单指令。需在 `compact_bytecode` 之后运行
///（Label 已移除，三连指令必然相邻）。
///
/// 返回紧化后的代码及 `old_pc → new_pc` 映射（长度 n+1，含哨兵），
/// 供调用方同步紧化 `line_map` / `column_map`。
pub fn peephole_fuse(code: Vec<Instruction>) -> (Vec<Instruction>, Vec<usize>) {
    let n = code.len();
    if n < 3 {
        return (code, (0..=n).collect());
    }
    // 收集所有跳转目标 PC，防止融合跨越跳转入口。
    let mut jump_targets: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for ins in &code {
        match ins {
            Instruction::Goto(t) | Instruction::GotoIf(t) | Instruction::GotoIfNot(t)
            | Instruction::LoopCountdown(t) => {
                jump_targets.insert(*t);
            }
            Instruction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                jump_targets.insert(*catch_label);
                if *else_label != 0 {
                    jump_targets.insert(*else_label);
                }
                jump_targets.insert(*end_label);
            }
            _ => {}
        }
    }
    let mut new_code = Vec::with_capacity(n);
    let mut remap = vec![0usize; n + 1];
    let mut i = 0;
    while i < n {
        if i + 2 < n
            && !jump_targets.contains(&(i + 1))
            && !jump_targets.contains(&(i + 2))
        {
            match (&code[i], &code[i + 1], &code[i + 2]) {
                (
                    Instruction::LoadFast(slot),
                    Instruction::PushSmall(imm),
                    Instruction::Sub | Instruction::SubNumNum,
                ) if *imm >= i32::MIN as i64
                    && *imm <= i32::MAX as i64
                    && *slot < u32::MAX as usize =>
                {
                    remap[i] = new_code.len();
                    remap[i + 1] = new_code.len();
                    remap[i + 2] = new_code.len();
                    new_code.push(Instruction::LoadFastSubImm {
                        slot: *slot,
                        imm: *imm,
                    });
                    i += 3;
                    continue;
                }
                (
                    Instruction::LoadFast(slot),
                    Instruction::PushSmall(imm),
                    Instruction::Le | Instruction::LeNumNum,
                ) if *imm >= i32::MIN as i64
                    && *imm <= i32::MAX as i64
                    && *slot < u32::MAX as usize =>
                {
                    remap[i] = new_code.len();
                    remap[i + 1] = new_code.len();
                    remap[i + 2] = new_code.len();
                    new_code.push(Instruction::LoadFastLeImm {
                        slot: *slot,
                        imm: *imm,
                    });
                    i += 3;
                    continue;
                }
                _ => {}
            }
        }
        remap[i] = new_code.len();
        new_code.push(code[i].clone());
        i += 1;
    }
    remap[n] = new_code.len();
    // 用 remap 重映射跳转目标。
    let new_len = new_code.len();
    for ins in new_code.iter_mut() {
        match ins {
            Instruction::Goto(t) | Instruction::GotoIf(t) | Instruction::GotoIfNot(t)
            | Instruction::LoopCountdown(t) => {
                *t = if *t >= n { new_len } else { remap[*t] };
            }
            Instruction::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                *catch_label = if *catch_label >= n { new_len } else { remap[*catch_label] };
                if *else_label != 0 {
                    *else_label = if *else_label >= n { new_len } else { remap[*else_label] };
                }
                *end_label = if *end_label >= n { new_len } else { remap[*end_label] };
            }
            _ => {}
        }
    }
    (new_code, remap)
}

#[derive(Clone)]
pub struct MacroObject {
    pub name: String,
    pub params: Vec<crate::ast::MacroParam>,
    pub body: Arc<Vec<Instruction>>,
    pub entry_label: usize,
    pub fast_locals: usize,
    pub variadic_param_index: Option<usize>,
}

impl MacroObject {
    pub fn new(name: impl Into<String>, params: Vec<crate::ast::MacroParam>, body: Vec<Instruction>) -> Self {
        let variadic_param_index = params.iter().position(|p| p.is_variadic);
        Self {
            name: name.into(),
            params,
            body: Arc::new(body),
            entry_label: 0,
            fast_locals: 0,
            variadic_param_index,
        }
    }
}

/// 模块全局名表与绑定的快照；挂到该模块内编译的函数上，使导入后 `LoadGlobal`/`StoreGlobal` 仍可用。
/// `globals` 用 `SyncCell`（parking_lot）：导入后模块函数写入模块自己的绑定，且可跨线程共享。
/// `finalized`：模块加载结束、live 快照挂上后为 true。编译期占位 env 为 false，
/// 以便加载收尾可升级；已 finalized 的函数（含 `use` 引入的外模块函数）不得再被换绑。
#[derive(Clone)]
pub struct ModuleGlobalEnv {
    pub global_names: Vec<String>,
    pub globals: SyncCell<HashMap<String, Value>>,
    pub finalized: bool,
}

#[derive(Clone)]
pub struct GenericFunctionTemplate {
    pub name: String,
    pub type_params: Vec<(String, Option<crate::ast::Expr>)>,
    pub params: Vec<FuncParam>,
    pub body: crate::ast::Block,
    pub return_type: Option<crate::ast::Expr>,
    pub return_strong: bool,
    pub return_wrapper: Option<crate::ast::Expr>,
    pub is_generator: bool,
    /// 定义处源码（REPL 分段特化时供错误展示）。
    pub source: Option<Arc<str>>,
    pub source_file: String,
}

#[derive(Clone)]
pub struct FunctionObject {
    pub name: String,
    pub params: Vec<crate::ast::FuncParam>,
    pub body: Arc<Vec<Instruction>>,
    /// 与 `body` 等长的紧凑热操作码（`u8` + 操作数），供热循环使用。
    pub hot: crate::hot_code::HotCode,
    pub line_map: Arc<Vec<usize>>,
    pub column_map: Arc<Vec<usize>>,
    pub entry_label: usize,
    pub fast_locals: usize,
    pub is_builtin_body: bool,
    pub variadic_param_index: Option<usize>,
    pub kwvariadic_param_index: Option<usize>,
    /// 与 `params` 等长；有默认值的槽在定义时由 `__attach_defaults__` 填入。
    pub defaults: Vec<Option<Value>>,
    pub captured: Option<HashMap<String, Value>>,
    pub return_type: Option<crate::ast::Expr>,
    pub return_strong: bool,
    pub return_wrapper: Option<crate::ast::Expr>,
    /// 定义时求值后的参数类型（与 `params` 等长；无注解为 `None`）。
    pub param_types: Vec<Option<Value>>,
    /// 定义时求值后的返回类型。
    pub return_type_value: Option<Value>,
    /// 是否已在定义处完成注解求值与校验。
    pub types_resolved: bool,
    /// 局部帧 `Vec` 大小（`max(fast slot index) + 1`）。
    pub frame_slots: usize,
    /// 运行时是否需为本函数维护 name→slot 映射。
    pub uses_name_map: bool,
    /// 每次调用是否压入 `func_frames`（traceback）。
    pub track_frames: bool,
    /// 已预解析的 `entry_label` PC。
    pub entry_pc: usize,
    /// `CallSelf` 时用 `fast_ret_pcs` 而非完整 `UserCallFrame`。
    pub lightweight: bool,
    /// 体含 `yield` / `yield from`：调用返回 iterator，不立即执行体。
    pub is_generator: bool,
    /// 若设置，`LoadGlobal` 相对本模块环境解析，而非调用方的 `script_global_names` / `globals`。
    pub module_env: Option<Arc<ModuleGlobalEnv>>,
    /// 定义本函数的源码（供运行时错误展示上下文；REPL 分多段定义时必需）。
    pub source: Option<Arc<str>>,
    /// 定义本函数时的文件名。
    pub source_file: String,
}

impl FunctionObject {
    pub fn new(name: impl Into<String>, params: Vec<FuncParam>, body: Vec<Instruction>) -> Self {
        let hot = crate::hot_code::HotCode::encode(&body);
        Self {
            name: name.into(),
            params,
            body: Arc::new(body),
            hot,
            line_map: Arc::new(Vec::new()),
            column_map: Arc::new(Vec::new()),
            entry_label: 0,
            fast_locals: 0,
            is_builtin_body: false,
            variadic_param_index: None,
            kwvariadic_param_index: None,
            defaults: Vec::new(),
            captured: None,
            return_type: None,
            return_strong: false,
            return_wrapper: None,
            param_types: Vec::new(),
            return_type_value: None,
            types_resolved: false,
            frame_slots: 0,
            uses_name_map: true,
            track_frames: true,
            entry_pc: 0,
            lightweight: false,
            is_generator: false,
            module_env: None,
            source: None,
            source_file: "<script>".into(),
        }
    }
}

pub fn function_lightweight(
    body: &[Instruction],
    uses_name_map: bool,
    track_frames: bool,
    return_strong: bool,
) -> bool {
    if uses_name_map || track_frames || return_strong {
        return false;
    }
    body.iter().all(|ins| {
        !matches!(
            ins,
            Instruction::Call { .. }
                | Instruction::CallList
                | Instruction::CallEx
                | Instruction::EnterTry { .. }
                | Instruction::Throw
                | Instruction::PushExc
                | Instruction::Yield
                | Instruction::YieldFrom
        )
    })
}

pub fn function_uses_name_map(body: &[Instruction]) -> bool {
    body.iter().any(|ins| {
        matches!(
            ins,
            Instruction::Load(_)
                | Instruction::Store(_)
                | Instruction::NewVar { .. }
                | Instruction::NewVarOrLoad(_)
                | Instruction::BindFast { .. }
                | Instruction::DelName(_)
        )
    })
}

pub fn function_uses_try(body: &[Instruction]) -> bool {
    body.iter().any(|ins| {
        matches!(
            ins,
            Instruction::EnterTry { .. }
                | Instruction::Throw
                | Instruction::PushExc
                | Instruction::EndTry
                | Instruction::PopTry
        )
    })
}

pub struct CompiledProgram {
    pub code: Vec<Instruction>,
    /// 与 `code` 等长的热操作码。
    pub hot: crate::hot_code::HotCode,
    pub line_map: Vec<usize>,
    pub column_map: Vec<usize>,
    pub functions: HashMap<String, Arc<FunctionObject>>,
    pub macros: HashMap<String, Arc<MacroObject>>,
    pub struct_defs: HashMap<String, Arc<crate::value::StructDef>>,
    pub enum_defs: HashMap<String, Arc<crate::value::EnumDef>>,
    pub variant_defs: HashMap<String, Arc<crate::value::VariantDef>>,
    pub overload_tables: HashMap<String, Vec<Arc<FunctionObject>>>,
    pub protocols: HashMap<String, Arc<crate::protocol::ProtocolDef>>,
    pub generic_functions: HashMap<String, Arc<GenericFunctionTemplate>>,
    /// 本编译单元经 LoadGlobal/StoreGlobal 引用的名字（index → name）。
    pub global_names: Vec<String>,
}

impl Default for CompiledProgram {
    fn default() -> Self {
        Self::new()
    }
}

impl CompiledProgram {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            hot: crate::hot_code::HotCode::empty(),
            line_map: Vec::new(),
            column_map: Vec::new(),
            functions: HashMap::new(),
            macros: HashMap::new(),
            struct_defs: HashMap::new(),
            enum_defs: HashMap::new(),
            variant_defs: HashMap::new(),
            overload_tables: HashMap::new(),
            protocols: HashMap::new(),
            generic_functions: HashMap::new(),
            global_names: Vec::new(),
        }
    }
}

pub type Label = usize;

pub struct Codegen {
    pub code: Vec<Instruction>,
    pub line_map: Vec<usize>,
    pub column_map: Vec<usize>,
    pub label_counter: usize,
    pub label_positions: HashMap<usize, usize>,
    current_line: usize,
    current_column: usize,
}

impl Codegen {
    pub fn new() -> Self {
        Self {
            code: Vec::new(),
            line_map: Vec::new(),
            column_map: Vec::new(),
            label_counter: 0,
            label_positions: HashMap::new(),
            current_line: 0,
            current_column: 1,
        }
    }

    pub fn set_line(&mut self, line: usize) {
        self.current_line = line;
    }

    pub fn set_column(&mut self, column: usize) {
        self.current_column = if column == 0 { 1 } else { column };
    }

    pub fn set_loc(&mut self, line: usize, column: usize) {
        self.set_line(line);
        self.set_column(column);
    }

    pub fn take_line_map(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.line_map)
    }

    pub fn take_column_map(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.column_map)
    }

    pub fn fresh_label(&mut self) -> Label {
        let id = self.label_counter;
        self.label_counter += 1;
        id
    }

    pub fn fresh_temp(&mut self, prefix: &str) -> String {
        let id = self.label_counter;
        self.label_counter += 1;
        format!("{prefix}_{id}")
    }

    pub fn emit(&mut self, ins: Instruction) -> usize {
        let idx = self.code.len();
        self.code.push(ins);
        self.line_map.push(self.current_line);
        self.column_map.push(self.current_column);
        idx
    }

    pub fn mark_label(&mut self, label: Label) {
        self.label_positions.insert(label, self.code.len());
        self.emit(Instruction::Label(label));
    }

    pub fn patch_labels(&mut self) -> Result<(), String> {
        resolve_labels_in_place(&mut self.code)
    }
}

impl Default for Codegen {
    fn default() -> Self {
        Self::new()
    }
}
