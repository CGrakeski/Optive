use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// 无 `export` / `intern` 修饰符。
    Default,
    Exported,
    Internal,
}

#[derive(Debug, Clone)]
pub struct FuncParam {
    pub name: String,
    /// `*args` 可变位置参数。
    pub is_variadic: bool,
    /// `**kwargs` 可变关键字参数。
    pub is_kwvariadic: bool,
    /// `implicit a: T`：调用时隐式转换到 `T`。
    pub implicit: bool,
    /// 类型注解表达式（`: T` / `:: T`）；硬检查时求值，结果须 `is_a(_, type)`。
    pub type_expr: Option<Expr>,
    pub type_strong: bool,
    pub default_expr: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub name: Option<String>,
    /// `*iterable` 位置展开。
    pub is_splat: bool,
    /// `**dict` 关键字展开。
    pub is_kwsplat: bool,
    pub value: Expr,
}

/// 宏调用参数：解析期冻结的 AST（`m{arg}`），不是运行时 `Expr`。
#[derive(Debug, Clone)]
pub struct MacroCallArg {
    pub is_splat: bool,
    pub node: std::sync::Arc<crate::runtime_ast::RuntimeAstNode>,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: String,
    pub is_variadic: bool,
    pub type_expr: Option<Expr>,
    pub type_strong: bool,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    /// 字段 / payload 绑定名（如 `Point(a, b)` 中的 `a`）。
    Bind(String),
    Value(Box<Expr>),
    List(Vec<PatternElem>),
    /// `(a, b)` / `("done", x)` 元组模式（匹配 tuple 或 list）。
    Tuple(Vec<PatternElem>),
    Struct { type_name: String, fields: Vec<String> },
    Or(Vec<Self>),
    Call { type_name: String, args: Vec<Self> },
}

#[derive(Debug, Clone)]
pub enum PatternElem {
    Bind(String),
    Nested(Pattern),
    Value(Box<Expr>),
}

/// 解构绑定目标：`let (x, [y, *rest]) = ...` / `(a, b) = ...`。
#[derive(Debug, Clone)]
pub enum DestructPattern {
    Name(String),
    /// `_` 丢弃
    Discard,
    /// `(...)` 元组形解构（接受 list 或 tuple）
    Tuple(Vec<DestructElem>),
    /// `[...]` 列表形解构（接受 list 或 tuple）
    List(Vec<DestructElem>),
}

#[derive(Debug, Clone)]
pub enum DestructElem {
    Pat(DestructPattern),
    /// `*name` 收集剩余元素为 list
    Rest(String),
    /// `*_` 丢弃剩余
    RestDiscard,
}

#[derive(Debug, Clone)]
pub struct CatchClause {
    pub pattern: CatchPattern,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub enum CatchPattern {
    Wildcard,
    Bind { name: String, type_name: Option<String> },
}

#[derive(Debug, Clone)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct SelectCase {
    pub event: Expr,
    pub bind: Option<String>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct ForItem {
    pub name: String,
    pub iterable: Expr,
}

#[derive(Debug, Clone)]
pub struct UseItem {
    pub name: String,
    pub alias: Option<String>,
}

/// `use` 语句中的模块引用：`std.math` 或 `"path.tive".export`。
#[derive(Debug, Clone)]
pub enum ModuleRef {
    Qualified(Vec<String>),
    FilePath { path: String, attrs: Vec<String> },
}

#[derive(Debug, Clone)]
pub enum FStringPart {
    Text(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    VarDecl {
        visibility: Visibility,
        is_const: bool,
        is_var: bool,
        name: String,
        type_expr: Option<Expr>,
        type_strong: bool,
        init: Option<Expr>,
    },
    /// `let (x, y) = ...` / `var [a, *rest] = ...`
    DestructDecl {
        visibility: Visibility,
        is_const: bool,
        is_var: bool,
        pattern: DestructPattern,
        init: Expr,
    },
    Assign { target: LValue, value: Expr },
    /// `(x, y) = ...` / `[a, [b, c]] = ...`
    DestructAssign {
        pattern: DestructPattern,
        value: Expr,
    },
    FuncDecl {
        visibility: Visibility,
        decorators: Vec<Expr>,
        name: String,
        type_params: Vec<(String, Option<Expr>)>,
        params: Vec<FuncParam>,
        return_type: Option<Expr>,
        return_strong: bool,
        return_wrapper: Option<Expr>,
        body: Block,
        is_generator: bool,
    },
    ProtocolDecl {
        visibility: Visibility,
        name: String,
        members: Vec<ProtocolMember>,
    },
    Return(Option<Expr>),
    /// `yield` / `yield expr` —— 生成器产出（仅出现在含 yield 的 func/do 中）。
    Yield(Option<Expr>),
    /// `yield from expr` —— 委托迭代。
    YieldFrom(Expr),
    Throw(Expr),
    If {
        cond: Expr,
        then_block: Block,
        elifs: Vec<(Expr, Block)>,
        else_block: Option<Block>,
    },
    While { cond: Expr, body: Block },
    Loop { count: Option<Expr>, body: Block },
    For { items: Vec<ForItem>, body: Block },
    Break,
    Continue,
    Try {
        body: Block,
        catches: Vec<CatchClause>,
        else_block: Option<Block>,
    },
    Match {
        subject: Expr,
        cases: Vec<MatchCase>,
        else_block: Option<Block>,
    },
    Del(DelTarget),
    With {
        context: Expr,
        alias: Option<String>,
        body: Block,
    },
    Import {
        path: String,
        path_is_string: bool,
        alias: Option<String>,
    },
    Use {
        module: ModuleRef,
        items: Vec<UseItem>,
    },
    StructDecl {
        visibility: Visibility,
        typed: bool,
        name: String,
        type_params: Vec<(String, Option<Expr>)>,
        base: Option<String>,
        fields: Vec<StructField>,
        methods: Vec<StructMethod>,
        /// 尾部注解，如 `} : C.layout`（经属性链解析到一等 Layout）。
        layout: Option<Expr>,
    },
    MacroDecl {
        visibility: Visibility,
        name: String,
        params: Vec<MacroParam>,
        body: Block,
    },
    FriendFuncDecl {
        visibility: Visibility,
        name: String,
        params: Option<Vec<FuncParam>>,
        return_type: Option<Expr>,
        return_strong: bool,
        return_wrapper: Option<Expr>,
        body: Option<Block>,
    },
    EnumDecl {
        visibility: Visibility,
        name: String,
        members: Vec<EnumMemberDecl>,
        methods: Vec<EnumMethodDecl>,
    },
    VariantDecl {
        visibility: Visibility,
        name: String,
        type_params: Vec<(String, Option<Expr>)>,
        cases: Vec<VariantCaseDecl>,
    },
    Expr(Expr),
    Block(Block),
    /// 语句级注释（codegen 忽略；供 `fmt` 保留）。
    Comment {
        /// `true` = `/* ... */`，`false` = `// ...`
        is_block: bool,
        text: String,
    },
}

#[derive(Debug, Clone)]
pub enum ProtocolMember {
    Method {
        name: String,
        params: Vec<FuncParam>,
    },
    Field {
        name: String,
        mutable: bool,
    },
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub mutable: bool,
    pub name: String,
    pub type_expr: Option<Expr>,
    pub type_strong: bool,
    pub default_expr: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct StructMethod {
    pub name: String,
    pub params: Vec<FuncParam>,
    pub outside: bool,
    pub overload: bool,
    pub return_type: Option<Expr>,
    pub return_strong: bool,
    pub return_wrapper: Option<Expr>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct EnumMemberDecl {
    pub name: String,
    pub value: Option<Expr>,
}

#[derive(Debug, Clone)]
pub struct EnumMethodDecl {
    pub name: String,
    pub params: Vec<FuncParam>,
    pub body: Block,
}

#[derive(Debug, Clone)]
pub struct VariantCaseDecl {
    pub name: String,
    pub fields: Vec<StructField>,
}

#[derive(Debug, Clone)]
pub enum LValue {
    Name(String),
    Member { object: Box<Expr>, field: String },
    Index { object: Box<Expr>, index: Box<Expr> },
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone)]
pub enum DelTarget {
    Name(String),
    Member { object: Box<Expr>, field: String },
    Index { object: Box<Expr>, index: Box<Expr> },
}

pub type Block = Vec<LocatedStmt>;

/// 1-based 源码位置（与词法器 token 坐标一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceLoc {
    pub line: usize,
    pub column: usize,
}

impl SourceLoc {
    #[must_use]
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// 带源码位置的表达式（供诊断 / 插入符定位）。
#[derive(Debug, Clone)]
pub struct Expr {
    pub loc: SourceLoc,
    pub kind: ExprKind,
}

impl Expr {
    #[must_use]
    pub const fn new(loc: SourceLoc, kind: ExprKind) -> Self {
        Self { loc, kind }
    }

    #[must_use]
    pub const fn at(line: usize, column: usize, kind: ExprKind) -> Self {
        Self::new(SourceLoc::new(line, column), kind)
    }
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Number(String),
    String(String),
    FString(Vec<FStringPart>),
    Bool(bool),
    None,
    Var(String),
    Placeholder,
    Unary { op: UnaryOp, operand: Box<Expr> },
    Binary { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<CallArg> },
    MacroCall {
        callee: Box<Expr>,
        args: Vec<MacroCallArg>,
    },
    Member { object: Box<Expr>, field: String },
    Index { object: Box<Expr>, index: Box<Expr> },
    Slice {
        object: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
    TypeConvert { type_expr: Box<Expr>, value: Box<Expr> },
    List(Vec<Expr>),
    ListComp {
        elem: Box<Expr>,
        items: Vec<ForItem>,
        guards: Vec<Expr>,
    },
    Dict(Vec<(Expr, Expr)>),
    /// `{k: v for (x in xs) if (cond)}` 字典推导式。
    DictComp {
        key: Box<Expr>,
        value: Box<Expr>,
        items: Vec<ForItem>,
        guards: Vec<Expr>,
    },
    /// 集合字面量 `{a, b, c}`（无冒号）。空集为 `{,}`，不是 `{}`。
    Set(Vec<Expr>),
    /// `{x for (x in xs) if (cond)}` 集合推导式。
    SetComp {
        elem: Box<Expr>,
        items: Vec<ForItem>,
        guards: Vec<Expr>,
    },
    /// `(x for (x in xs) if (cond))` — 惰性生成器表达式。
    GeneratorExp {
        elem: Box<Expr>,
        items: Vec<ForItem>,
        guards: Vec<Expr>,
    },
    /// 元组字面量 `(a, b)` 或 `(a,)`。空元组为 `()`。
    Tuple(Vec<Expr>),
    /// 字节字面量 `b"..."`。
    Bytes(Vec<u8>),
    IfThenElse {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Handle { operand: Box<Expr> },
    /// `go expr` — 启动任务，立即返回 Task。
    Go { operand: Box<Expr> },
    /// `par for (x in xs) { ... }` — 并行 map，脱糖为 `std.async.par_map` / `gather`。
    ParFor {
        items: Vec<ForItem>,
        body: Block,
    },
    /// `par { e1; e2; ... }` — 并行求值表达式，返回结果元组。
    ParBlock { exprs: Vec<Expr> },
    /// `snap expr` — 若为 `none` 则抛异常，否则返回 `expr`。
    Snap { operand: Box<Expr> },
    /// `await expr` — 启动并等待。
    Await { operand: Box<Expr> },
    /// `suspend` — 协作式让出 CPU（调度器预算用尽时也会隐式挂起）。
    Suspend,
    /// `select { case ev as x { } } else { }`
    Select {
        cases: Vec<SelectCase>,
        else_block: Option<Block>,
    },
    NamedAssign { name: String, value: Box<Expr> },
    DoFunc {
        params: Vec<FuncParam>,
        /// 须 `Box`：直接 `Option<Expr>` 会与 `ExprKind` 形成无限大小。
        return_type: Option<Box<Expr>>,
        return_strong: bool,
        return_wrapper: Option<Box<Expr>>,
        body: Block,
    },
    /// `left |> right`；解析期已将 `_` 替换为对 `pipe_name` 的引用。
    Pipeline {
        left: Box<Expr>,
        right: Box<Expr>,
        pipe_name: String,
    },
    Quote {
        hygienic_names: Vec<String>,
        bindings: Vec<Expr>,
        body: Block,
    },
    Match {
        subject: Box<Expr>,
        cases: Vec<MatchCase>,
        else_block: Option<Block>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    TruthyNot,
    Invert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
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
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    Is,
    IsNot,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub struct LocatedStmt {
    pub line: usize,
    pub column: usize,
    pub stmt: Stmt,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub stmts: Block,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// 返回包装器中 `_` 解析后绑定的局部名。
pub const RET_WRAPPER_VAL: &str = "__ret_wrapper_val";

/// 将表达式中的 `Placeholder` 替换为 `repl`。
/// 不进入 `DoFunc` / `Quote` 体与宏实参（那些作用域各自处理）。
#[must_use]
pub fn fill_placeholders(expr: &Expr, repl: &Expr) -> Expr {
    match &expr.kind {
        ExprKind::Placeholder => repl.clone(),
        ExprKind::DoFunc { .. } => expr.clone(),
        ExprKind::Quote {
            hygienic_names,
            bindings,
            body,
        } => Expr::new(
            expr.loc,
            ExprKind::Quote {
                hygienic_names: hygienic_names.clone(),
                bindings: bindings.iter().map(|b| fill_placeholders(b, repl)).collect(),
                body: body.clone(),
            },
        ),
        ExprKind::MacroCall { callee, args } => Expr::new(
            expr.loc,
            ExprKind::MacroCall {
                callee: Box::new(fill_placeholders(callee, repl)),
                args: args.clone(),
            },
        ),
        ExprKind::FString(parts) => Expr::new(
            expr.loc,
            ExprKind::FString(
                parts
                    .iter()
                    .map(|p| match p {
                        FStringPart::Text(t) => FStringPart::Text(t.clone()),
                        FStringPart::Expr(e) => {
                            FStringPart::Expr(Box::new(fill_placeholders(e, repl)))
                        }
                    })
                    .collect(),
            ),
        ),
        ExprKind::Unary { op, operand } => Expr::new(
            expr.loc,
            ExprKind::Unary {
                op: *op,
                operand: Box::new(fill_placeholders(operand, repl)),
            },
        ),
        ExprKind::Binary { op, left, right } => Expr::new(
            expr.loc,
            ExprKind::Binary {
                op: *op,
                left: Box::new(fill_placeholders(left, repl)),
                right: Box::new(fill_placeholders(right, repl)),
            },
        ),
        ExprKind::Call { callee, args } => Expr::new(
            expr.loc,
            ExprKind::Call {
                callee: Box::new(fill_placeholders(callee, repl)),
                args: args
                    .iter()
                    .map(|a| CallArg {
                        name: a.name.clone(),
                        is_splat: a.is_splat,
                        is_kwsplat: a.is_kwsplat,
                        value: fill_placeholders(&a.value, repl),
                    })
                    .collect(),
            },
        ),
        ExprKind::Member { object, field } => Expr::new(
            expr.loc,
            ExprKind::Member {
                object: Box::new(fill_placeholders(object, repl)),
                field: field.clone(),
            },
        ),
        ExprKind::Index { object, index } => Expr::new(
            expr.loc,
            ExprKind::Index {
                object: Box::new(fill_placeholders(object, repl)),
                index: Box::new(fill_placeholders(index, repl)),
            },
        ),
        ExprKind::Slice {
            object,
            start,
            end,
            step,
        } => Expr::new(
            expr.loc,
            ExprKind::Slice {
                object: Box::new(fill_placeholders(object, repl)),
                start: start.as_ref().map(|e| Box::new(fill_placeholders(e, repl))),
                end: end.as_ref().map(|e| Box::new(fill_placeholders(e, repl))),
                step: step.as_ref().map(|e| Box::new(fill_placeholders(e, repl))),
            },
        ),
        ExprKind::TypeConvert { type_expr, value } => Expr::new(
            expr.loc,
            ExprKind::TypeConvert {
                type_expr: Box::new(fill_placeholders(type_expr, repl)),
                value: Box::new(fill_placeholders(value, repl)),
            },
        ),
        ExprKind::List(elems) => Expr::new(
            expr.loc,
            ExprKind::List(elems.iter().map(|e| fill_placeholders(e, repl)).collect()),
        ),
        ExprKind::Set(elems) => Expr::new(
            expr.loc,
            ExprKind::Set(elems.iter().map(|e| fill_placeholders(e, repl)).collect()),
        ),
        ExprKind::Tuple(elems) => Expr::new(
            expr.loc,
            ExprKind::Tuple(elems.iter().map(|e| fill_placeholders(e, repl)).collect()),
        ),
        ExprKind::Dict(entries) => Expr::new(
            expr.loc,
            ExprKind::Dict(
                entries
                    .iter()
                    .map(|(k, v)| (fill_placeholders(k, repl), fill_placeholders(v, repl)))
                    .collect(),
            ),
        ),
        ExprKind::ListComp { elem, items, guards } => Expr::new(
            expr.loc,
            ExprKind::ListComp {
                elem: Box::new(fill_placeholders(elem, repl)),
                items: fill_placeholder_for_items(items, repl),
                guards: guards.iter().map(|g| fill_placeholders(g, repl)).collect(),
            },
        ),
        ExprKind::SetComp { elem, items, guards } => Expr::new(
            expr.loc,
            ExprKind::SetComp {
                elem: Box::new(fill_placeholders(elem, repl)),
                items: fill_placeholder_for_items(items, repl),
                guards: guards.iter().map(|g| fill_placeholders(g, repl)).collect(),
            },
        ),
        ExprKind::GeneratorExp { elem, items, guards } => Expr::new(
            expr.loc,
            ExprKind::GeneratorExp {
                elem: Box::new(fill_placeholders(elem, repl)),
                items: fill_placeholder_for_items(items, repl),
                guards: guards.iter().map(|g| fill_placeholders(g, repl)).collect(),
            },
        ),
        ExprKind::DictComp {
            key,
            value,
            items,
            guards,
        } => Expr::new(
            expr.loc,
            ExprKind::DictComp {
                key: Box::new(fill_placeholders(key, repl)),
                value: Box::new(fill_placeholders(value, repl)),
                items: fill_placeholder_for_items(items, repl),
                guards: guards.iter().map(|g| fill_placeholders(g, repl)).collect(),
            },
        ),
        ExprKind::IfThenElse {
            cond,
            then_expr,
            else_expr,
        } => Expr::new(
            expr.loc,
            ExprKind::IfThenElse {
                cond: Box::new(fill_placeholders(cond, repl)),
                then_expr: Box::new(fill_placeholders(then_expr, repl)),
                else_expr: Box::new(fill_placeholders(else_expr, repl)),
            },
        ),
        ExprKind::Handle { operand } => Expr::new(
            expr.loc,
            ExprKind::Handle {
                operand: Box::new(fill_placeholders(operand, repl)),
            },
        ),
        ExprKind::Go { operand } => Expr::new(
            expr.loc,
            ExprKind::Go {
                operand: Box::new(fill_placeholders(operand, repl)),
            },
        ),
        ExprKind::ParFor { items, body } => Expr::new(
            expr.loc,
            ExprKind::ParFor {
                items: fill_placeholder_for_items(items, repl),
                body: body.clone(),
            },
        ),
        ExprKind::ParBlock { exprs } => Expr::new(
            expr.loc,
            ExprKind::ParBlock {
                exprs: exprs.iter().map(|e| fill_placeholders(e, repl)).collect(),
            },
        ),
        ExprKind::Snap { operand } => Expr::new(
            expr.loc,
            ExprKind::Snap {
                operand: Box::new(fill_placeholders(operand, repl)),
            },
        ),
        ExprKind::Await { operand } => Expr::new(
            expr.loc,
            ExprKind::Await {
                operand: Box::new(fill_placeholders(operand, repl)),
            },
        ),
        ExprKind::Select { cases, else_block } => Expr::new(
            expr.loc,
            ExprKind::Select {
                cases: cases
                    .iter()
                    .map(|c| SelectCase {
                        event: fill_placeholders(&c.event, repl),
                        bind: c.bind.clone(),
                        body: c.body.clone(),
                    })
                    .collect(),
                else_block: else_block.clone(),
            },
        ),
        ExprKind::NamedAssign { name, value } => Expr::new(
            expr.loc,
            ExprKind::NamedAssign {
                name: name.clone(),
                value: Box::new(fill_placeholders(value, repl)),
            },
        ),
        ExprKind::Pipeline {
            left,
            right,
            pipe_name,
        } => Expr::new(
            expr.loc,
            ExprKind::Pipeline {
                left: Box::new(fill_placeholders(left, repl)),
                right: Box::new(fill_placeholders(right, repl)),
                pipe_name: pipe_name.clone(),
            },
        ),
        ExprKind::Match {
            subject,
            cases,
            else_block,
        } => Expr::new(
            expr.loc,
            ExprKind::Match {
                subject: Box::new(fill_placeholders(subject, repl)),
                cases: cases.clone(),
                else_block: else_block.clone(),
            },
        ),
        ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Bool(_)
        | ExprKind::None
        | ExprKind::Var(_)
        | ExprKind::Bytes(_)
        | ExprKind::Suspend => expr.clone(),
    }
}

fn fill_placeholder_for_items(items: &[ForItem], repl: &Expr) -> Vec<ForItem> {
    items
        .iter()
        .map(|it| ForItem {
            name: it.name.clone(),
            iterable: fill_placeholders(&it.iterable, repl),
        })
        .collect()
}
