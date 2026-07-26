use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{FuncParam, TypeExpr};
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
    Pow,
    /// 已证两侧为 `Num` 的幂运算。
    PowNumNum,
    Neg,
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
    TypeCheck(TypeExpr),
    FindMod(String),
    RegisterExport(String),
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
            | Instruction::GotoIfNot(target) => {
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

#[derive(Clone)]
pub struct MacroObject {
    pub name: String,
    pub params: Vec<crate::ast::MacroParam>,
    pub body: Rc<Vec<Instruction>>,
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
            body: Rc::new(body),
            entry_label: 0,
            fast_locals: 0,
            variadic_param_index,
        }
    }
}

/// 模块全局名表与绑定的快照；挂到该模块内编译的函数上，使导入后 `LoadGlobal` 仍可用。
#[derive(Clone)]
pub struct ModuleGlobalEnv {
    pub global_names: Vec<String>,
    pub globals: HashMap<String, Value>,
}

#[derive(Clone)]
pub struct GenericFunctionTemplate {
    pub name: String,
    pub type_params: Vec<(String, Option<crate::ast::TypeExpr>)>,
    pub params: Vec<FuncParam>,
    pub body: crate::ast::Block,
    pub return_type: Option<crate::ast::TypeExpr>,
    pub return_strong: bool,
    pub return_wrapper: Option<crate::ast::Expr>,
    /// 定义处源码（REPL 分段特化时供错误展示）。
    pub source: Option<Rc<str>>,
    pub source_file: String,
}

#[derive(Clone)]
pub struct FunctionObject {
    pub name: String,
    pub params: Vec<crate::ast::FuncParam>,
    pub body: Rc<Vec<Instruction>>,
    /// 与 `body` 等长的紧凑热操作码（`u8` + 操作数），供热循环使用。
    pub hot: crate::hot_code::HotCode,
    pub line_map: Rc<Vec<usize>>,
    pub column_map: Rc<Vec<usize>>,
    pub entry_label: usize,
    pub fast_locals: usize,
    pub is_builtin_body: bool,
    pub variadic_param_index: Option<usize>,
    pub kwvariadic_param_index: Option<usize>,
    /// 与 `params` 等长；有默认值的槽在定义时由 `__attach_defaults__` 填入。
    pub defaults: Vec<Option<Value>>,
    pub captured: Option<HashMap<String, Value>>,
    pub return_type: Option<TypeExpr>,
    pub return_strong: bool,
    pub return_wrapper: Option<crate::ast::Expr>,
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
    /// 若设置，`LoadGlobal` 相对本模块环境解析，而非调用方的 `script_global_names` / `globals`。
    pub module_env: Option<Rc<ModuleGlobalEnv>>,
    /// 定义本函数的源码（供运行时错误展示上下文；REPL 分多段定义时必需）。
    pub source: Option<Rc<str>>,
    /// 定义本函数时的文件名。
    pub source_file: String,
}

impl FunctionObject {
    pub fn new(name: impl Into<String>, params: Vec<FuncParam>, body: Vec<Instruction>) -> Self {
        let hot = crate::hot_code::HotCode::encode(&body);
        Self {
            name: name.into(),
            params,
            body: Rc::new(body),
            hot,
            line_map: Rc::new(Vec::new()),
            column_map: Rc::new(Vec::new()),
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
            frame_slots: 0,
            uses_name_map: true,
            track_frames: true,
            entry_pc: 0,
            lightweight: false,
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
    pub functions: HashMap<String, Rc<FunctionObject>>,
    pub macros: HashMap<String, Rc<MacroObject>>,
    pub struct_defs: HashMap<String, Rc<crate::value::StructDef>>,
    pub enum_defs: HashMap<String, Rc<crate::value::EnumDef>>,
    pub variant_defs: HashMap<String, Rc<crate::value::VariantDef>>,
    pub overload_tables: HashMap<String, Vec<Rc<FunctionObject>>>,
    pub protocols: HashMap<String, Rc<crate::protocol::ProtocolDef>>,
    pub generic_functions: HashMap<String, Rc<GenericFunctionTemplate>>,
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
