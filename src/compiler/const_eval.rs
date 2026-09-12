//! Pure, fuel-limited compile-time evaluation for `const` and `const func`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{
    BinaryOp, Block, DestructElem, DestructPattern, Expr, ExprKind, ForItem, FuncParam, LValue,
    Program, SourceLoc, Stmt, UnaryOp,
};
use crate::compiler::const_effect;
use crate::compiler::module_interface;
use crate::error::RuntimeError;
use crate::value::{FrozenValue, Num, Value, ValueKey};
use crate::Result;

const MAX_CONST_STEPS: usize = 100_000;
const MAX_CONST_DEPTH: usize = 256;
const MAX_CONST_ALLOC: usize = 4 * 1024 * 1024;
const MAX_CONST_NODES: usize = 50_000;

#[derive(Clone, Debug, Default)]
pub struct CompileTimeModule {
    pub name: String,
    pub values: HashMap<String, Value>,
}

impl CompileTimeModule {
    #[must_use]
    pub fn new(name: impl Into<String>, values: HashMap<String, Value>) -> Self {
        Self {
            name: name.into(),
            values,
        }
    }

    #[must_use]
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.values.get(field)
    }
}

#[derive(Clone, Default)]
pub struct ConstImports {
    pub values: HashMap<String, Value>,
    pub modules: HashMap<String, CompileTimeModule>,
}

#[derive(Clone, Default)]
pub struct ConstContext {
    values: HashMap<String, Value>,
    modules: HashMap<String, CompileTimeModule>,
    functions: HashMap<String, ConstFunction>,
}

#[derive(Clone)]
struct ConstFunction {
    params: Vec<FuncParam>,
    body: Block,
    loc: SourceLoc,
}

#[derive(Default)]
struct EvalLocals {
    values: HashMap<String, Value>,
    mutable: HashSet<String>,
}

struct EvalState {
    steps: usize,
    depth: usize,
    alloc: usize,
    nodes: usize,
    stack: Vec<Frame>,
    counts: HashMap<String, usize>,
    memo: HashMap<MemoKey, Value>,
    active: HashSet<MemoKey>,
}

#[derive(Clone, Debug)]
struct Frame {
    name: String,
    loc: SourceLoc,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct MemoKey {
    name: String,
    args: String,
}

enum Flow {
    Normal,
    Return(Value),
    Break,
    LoopContinue,
}

enum ConstValue {
    Value(Value),
    Module(CompileTimeModule),
}

impl ConstContext {
    pub fn build(program: &Program) -> Result<Self> {
        Self::build_with_imports(program, ConstImports::default())
    }

    pub fn build_with_values(program: &Program, values: HashMap<String, Value>) -> Result<Self> {
        Self::build_with_imports(
            program,
            ConstImports {
                values,
                modules: HashMap::new(),
            },
        )
    }

    pub fn build_with_imports(program: &Program, imports: ConstImports) -> Result<Self> {
        let mut cx = Self {
            values: imports.values,
            modules: imports.modules,
            functions: HashMap::new(),
        };
        for located in &program.stmts {
            if let Stmt::FuncDecl {
                name,
                params,
                body,
                is_const: true,
                decorators,
                is_generator,
                type_params,
                ..
            } = &located.stmt
            {
                if *is_generator || !decorators.is_empty() || !type_params.is_empty() {
                    return Err(RuntimeError::msg(format!(
                        "const func `{name}` cannot be a generator, decorated, or generic"
                    )));
                }
                if params.iter().any(|param| {
                    param.default_expr.is_some()
                        || param.is_variadic
                        || param.is_kwvariadic
                        || param.implicit
                }) {
                    return Err(RuntimeError::msg(format!(
                        "const func `{name}` requires plain parameters without defaults"
                    )));
                }
                cx.functions.insert(
                    name.clone(),
                    ConstFunction {
                        params: params.clone(),
                        body: body.clone(),
                        loc: SourceLoc::new(located.line, located.column),
                    },
                );
            }
        }
        for (name, function) in &cx.functions {
            cx.validate_block(&function.body).map_err(|error| {
                RuntimeError::msg(format!(
                    "const func `{name}` has forbidden effect: {}",
                    error.message()
                ))
            })?;
        }
        let mut state = EvalState::new();
        for located in &program.stmts {
            if let Stmt::VarDecl {
                name,
                init: Some(init),
                is_const: true,
                ..
            } = &located.stmt
            {
                let value = cx
                    .eval_required(init, &EvalLocals::default(), &mut state)
                    .map_err(|error| {
                        RuntimeError::msg(format!(
                            "const `{name}` cannot be evaluated: {}",
                            error.message()
                        ))
                    })?;
                cx.values.insert(name.clone(), value);
            }
        }
        Ok(cx)
    }

    #[must_use]
    pub fn is_const_function(&self, name: &str) -> bool {
        self.functions.contains_key(name)
    }

    #[must_use]
    pub fn is_module(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    #[must_use]
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    #[must_use]
    pub fn module(&self, name: &str) -> Option<&CompileTimeModule> {
        self.modules.get(name)
    }

    pub fn try_eval(&self, expr: &Expr) -> Result<Option<Value>> {
        match self.eval(expr, &EvalLocals::default(), &mut EvalState::new())? {
            Some(ConstValue::Value(value)) => Ok(Some(value)),
            Some(ConstValue::Module(_)) | None => Ok(None),
        }
    }

    fn eval_required(
        &self,
        expr: &Expr,
        locals: &EvalLocals,
        state: &mut EvalState,
    ) -> Result<Value> {
        match self.eval(expr, locals, state)? {
            Some(ConstValue::Value(value)) => Ok(value),
            Some(ConstValue::Module(module)) => Err(state.error(
                expr.loc,
                format!(
                    "compile-time module `{}` is a namespace, not a value",
                    module.name
                ),
            )),
            None => Err(state.error(
                expr.loc,
                "expression depends on a runtime value or unsupported operation",
            )),
        }
    }

    fn eval(
        &self,
        expr: &Expr,
        locals: &EvalLocals,
        state: &mut EvalState,
    ) -> Result<Option<ConstValue>> {
        state.tick(expr.loc, "expression")?;
        let value = match &expr.kind {
            ExprKind::Number(text) => Value::Num(Num::from_literal(text)?),
            ExprKind::String(text) => {
                state.charge(text.len(), 1, expr.loc)?;
                Value::Text(text.clone())
            }
            ExprKind::Bool(value) => Value::Bool(*value),
            ExprKind::None => Value::None,
            ExprKind::Bytes(bytes) => {
                state.charge(bytes.len(), 1, expr.loc)?;
                Value::Bytes(Arc::new(bytes.clone()))
            }
            ExprKind::Var(name) => {
                if let Some(value) = locals.values.get(name).or_else(|| self.values.get(name)) {
                    return Ok(Some(ConstValue::Value(value.clone())));
                }
                if let Some(module) = self.modules.get(name) {
                    return Ok(Some(ConstValue::Module(module.clone())));
                }
                return Ok(None);
            }
            ExprKind::Member { object, field } => {
                return Ok(Some(ConstValue::Value(
                    self.eval_member(object, field, locals, state, expr.loc)?,
                )));
            }
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_required(item, locals, state)?);
                }
                state.charge_collection(values.len(), expr.loc)?;
                Value::Frozen(Arc::new(FrozenValue::List(values.into())))
            }
            ExprKind::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    let Some(ConstValue::Value(value)) = self.eval(item, locals, state)? else {
                        return Ok(None);
                    };
                    values.push(value);
                }
                state.charge_collection(values.len(), expr.loc)?;
                Value::Tuple(Arc::from(values.into_boxed_slice()))
            }
            ExprKind::Dict(entries) => {
                let mut values: Vec<(ValueKey, Value)> = Vec::with_capacity(entries.len());
                for (key, value) in entries {
                    let key = ValueKey::from_value(&self.eval_required(key, locals, state)?)?;
                    let value = self.eval_required(value, locals, state)?;
                    if let Some((_, current)) =
                        values.iter_mut().find(|(candidate, _)| candidate == &key)
                    {
                        *current = value;
                    } else {
                        values.push((key, value));
                    }
                }
                state.charge_collection(values.len(), expr.loc)?;
                Value::Frozen(Arc::new(FrozenValue::Dict(values.into())))
            }
            ExprKind::Set(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    let key = ValueKey::from_value(&self.eval_required(item, locals, state)?)?;
                    if !values.contains(&key) {
                        values.push(key);
                    }
                }
                state.charge_collection(values.len(), expr.loc)?;
                Value::Frozen(Arc::new(FrozenValue::Set(values.into())))
            }
            ExprKind::Index { object, index } => {
                let object = self.eval_required(object, locals, state)?;
                let index = self.eval_required(index, locals, state)?;
                index_value(&object, &index, expr.loc, state)?
            }
            ExprKind::Slice {
                object,
                start,
                end,
                step,
            } => {
                let object = self.eval_required(object, locals, state)?;
                let start = start
                    .as_deref()
                    .map(|expr| self.eval_required(expr, locals, state))
                    .transpose()?;
                let end = end
                    .as_deref()
                    .map(|expr| self.eval_required(expr, locals, state))
                    .transpose()?;
                let step = step
                    .as_deref()
                    .map(|expr| self.eval_required(expr, locals, state))
                    .transpose()?;
                slice_value(
                    &object,
                    start.as_ref(),
                    end.as_ref(),
                    step.as_ref(),
                    expr.loc,
                    state,
                )?
            }
            ExprKind::Unary { op, operand } => {
                let Some(ConstValue::Value(operand)) = self.eval(operand, locals, state)? else {
                    return Ok(None);
                };
                match op {
                    UnaryOp::Neg => operand.neg()?,
                    UnaryOp::Invert => operand.invert()?,
                    UnaryOp::Not => match operand {
                        Value::Bool(value) => Value::Bool(!value),
                        _ => {
                            return Err(state
                                .error(expr.loc, "compile-time `!` requires a boolean operand"));
                        }
                    },
                    UnaryOp::TruthyNot => Value::Bool(!truthy(&operand)),
                }
            }
            ExprKind::Binary { op, left, right } => {
                let Some(ConstValue::Value(left)) = self.eval(left, locals, state)? else {
                    return Ok(None);
                };
                if *op == BinaryOp::And && !truthy(&left) {
                    return Ok(Some(ConstValue::Value(left)));
                }
                if *op == BinaryOp::Or && truthy(&left) {
                    return Ok(Some(ConstValue::Value(left)));
                }
                let Some(ConstValue::Value(right)) = self.eval(right, locals, state)? else {
                    return Ok(None);
                };
                eval_binary(*op, &left, &right)?
            }
            ExprKind::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                let Some(ConstValue::Value(cond)) = self.eval(cond, locals, state)? else {
                    return Ok(None);
                };
                if truthy(&cond) {
                    let Some(ConstValue::Value(value)) = self.eval(then_expr, locals, state)?
                    else {
                        return Ok(None);
                    };
                    value
                } else {
                    let Some(ConstValue::Value(value)) = self.eval(else_expr, locals, state)?
                    else {
                        return Ok(None);
                    };
                    value
                }
            }
            ExprKind::Call { callee, args } => {
                return self.eval_call(callee, args, locals, state, expr.loc);
            }
            _ => return Ok(None),
        };
        Ok(Some(ConstValue::Value(value)))
    }

    fn eval_member(
        &self,
        object: &Expr,
        field: &str,
        locals: &EvalLocals,
        state: &mut EvalState,
        loc: SourceLoc,
    ) -> Result<Value> {
        match self.eval(object, locals, state)? {
            Some(ConstValue::Module(module)) => module.get(field).cloned().ok_or_else(|| {
                state.error(
                    loc,
                    format!(
                        "compile-time module `{}` has no public const `{field}`",
                        module.name
                    ),
                )
            }),
            Some(ConstValue::Value(_)) => Err(state.error(
                loc,
                "compile-time member access is only supported on imported module namespaces",
            )),
            None => Err(state.error(
                loc,
                "expression depends on a runtime value or unsupported operation",
            )),
        }
    }

    fn eval_call(
        &self,
        callee: &Expr,
        args: &[crate::ast::CallArg],
        locals: &EvalLocals,
        state: &mut EvalState,
        loc: SourceLoc,
    ) -> Result<Option<ConstValue>> {
        let ExprKind::Var(name) = &callee.kind else {
            return Ok(None);
        };
        if args
            .iter()
            .any(|arg| arg.name.is_some() || arg.is_splat || arg.is_kwsplat)
        {
            return Err(state.error(
                loc,
                format!("compile-time call `{name}` expects plain positional arguments"),
            ));
        }
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            let Some(ConstValue::Value(value)) = self.eval(&arg.value, locals, state)? else {
                return Ok(None);
            };
            values.push(value);
        }
        if const_effect::is_pure_builtin(name) {
            return Ok(Some(ConstValue::Value(
                const_effect::eval_pure_builtin(name, &values)
                    .map_err(|error| state.error(loc, error.message()))?,
            )));
        }
        let Some(function) = self.functions.get(name).cloned() else {
            return Ok(None);
        };
        if values.len() != function.params.len() {
            return Err(state.error(
                loc,
                format!(
                    "const func `{name}` expects {} positional arguments",
                    function.params.len()
                ),
            ));
        }
        let value = self.call_function(name, &function, values, loc, state)?;
        Ok(Some(ConstValue::Value(value)))
    }

    fn call_function(
        &self,
        name: &str,
        function: &ConstFunction,
        args: Vec<Value>,
        loc: SourceLoc,
        state: &mut EvalState,
    ) -> Result<Value> {
        let key = MemoKey {
            name: name.to_string(),
            args: fingerprint_args(&args)?,
        };
        if let Some(value) = state.memo.get(&key) {
            return Ok(value.clone());
        }
        if !state.active.insert(key.clone()) {
            return Err(state.error(
                loc,
                format!("compile-time recursion cycle involving `{name}`"),
            ));
        }
        state.enter(name, loc)?;
        let mut call_locals = EvalLocals::default();
        for (param, value) in function.params.iter().zip(args) {
            call_locals.values.insert(param.name.clone(), value);
        }
        let result = match self.exec_block(&function.body, &mut call_locals, state) {
            Ok(Flow::Return(value)) => Ok(value),
            Ok(Flow::Normal) => Ok(Value::None),
            Ok(Flow::Break | Flow::LoopContinue) => {
                Err(state.error(function.loc, "loop control escaped const func"))
            }
            Err(error) => Err(error),
        };
        state.leave();
        state.active.remove(&key);
        let value = result?;
        state.memo.insert(key, value.clone());
        Ok(value)
    }

    fn exec_block(
        &self,
        block: &Block,
        locals: &mut EvalLocals,
        state: &mut EvalState,
    ) -> Result<Flow> {
        for located in block {
            state.tick(SourceLoc::new(located.line, located.column), "statement")?;
            let loc = SourceLoc::new(located.line, located.column);
            match &located.stmt {
                Stmt::Return(value) => {
                    return Ok(Flow::Return(match value {
                        Some(expr) => self.eval_required(expr, locals, state)?,
                        None => Value::None,
                    }));
                }
                Stmt::VarDecl {
                    name,
                    init: Some(init),
                    is_var,
                    ..
                } => {
                    let value = self.eval_required(init, locals, state)?;
                    locals.values.insert(name.clone(), value);
                    if *is_var {
                        locals.mutable.insert(name.clone());
                    } else {
                        locals.mutable.remove(name);
                    }
                }
                Stmt::DestructDecl {
                    pattern,
                    init,
                    is_var,
                    ..
                } => {
                    let value = self.eval_required(init, locals, state)?;
                    bind_pattern(pattern, &value, locals, *is_var, true, loc, state)?;
                }
                Stmt::Assign {
                    target: LValue::Name(name),
                    value,
                } if locals.values.contains_key(name) => {
                    if !locals.mutable.contains(name) {
                        return Err(state.error(
                            loc,
                            format!("cannot assign to immutable binding `{name}` in const func"),
                        ));
                    }
                    let value = self.eval_required(value, locals, state)?;
                    locals.values.insert(name.clone(), value);
                }
                Stmt::DestructAssign { pattern, value } => {
                    let value = self.eval_required(value, locals, state)?;
                    bind_pattern(pattern, &value, locals, true, false, loc, state)?;
                }
                Stmt::If {
                    cond,
                    then_block,
                    elifs,
                    else_block,
                } => {
                    let selected = if truthy(&self.eval_required(cond, locals, state)?) {
                        Some(then_block)
                    } else {
                        let mut selected = None;
                        for (cond, block) in elifs {
                            if truthy(&self.eval_required(cond, locals, state)?) {
                                selected = Some(block);
                                break;
                            }
                        }
                        selected.or(else_block.as_ref())
                    };
                    if let Some(selected) = selected {
                        match self.exec_block(selected, locals, state)? {
                            Flow::Normal => {}
                            flow => return Ok(flow),
                        }
                    }
                }
                Stmt::While { cond, body } => {
                    while truthy(&self.eval_required(cond, locals, state)?) {
                        match self.exec_block(body, locals, state)? {
                            flow @ Flow::Return(_) => return Ok(flow),
                            Flow::Break => break,
                            Flow::Normal | Flow::LoopContinue => {}
                        }
                    }
                }
                Stmt::Loop { count, body } => {
                    let count = match count {
                        Some(expr) => match self.eval_required(expr, locals, state)? {
                            Value::Num(n) => n.to_i64().ok_or_else(|| {
                                state.error(loc, "const loop count must be an integer")
                            })?,
                            _ => return Err(state.error(loc, "const loop count must be a number")),
                        },
                        None => i64::MAX,
                    };
                    if count < 0 {
                        return Err(state.error(loc, "const loop count cannot be negative"));
                    }
                    for _ in 0..count {
                        match self.exec_block(body, locals, state)? {
                            flow @ Flow::Return(_) => return Ok(flow),
                            Flow::Break => break,
                            Flow::Normal | Flow::LoopContinue => {}
                        }
                    }
                }
                Stmt::For { items, body } => {
                    match self.exec_for(items, body, locals, state, loc)? {
                        Flow::Normal => {}
                        flow => return Ok(flow),
                    }
                }
                Stmt::Break => return Ok(Flow::Break),
                Stmt::Continue => return Ok(Flow::LoopContinue),
                Stmt::Expr(expr) => {
                    self.eval_required(expr, locals, state)?;
                }
                _ => {
                    return Err(state.error(loc, "statement is not allowed in a const func"));
                }
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_for(
        &self,
        items: &[ForItem],
        body: &Block,
        locals: &mut EvalLocals,
        state: &mut EvalState,
        loc: SourceLoc,
    ) -> Result<Flow> {
        if items.is_empty() {
            return Err(state.error(loc, "compile-time for requires an iterator"));
        }
        let mut streams = Vec::with_capacity(items.len());
        for item in items {
            streams.push(iterate(
                &self.eval_required(&item.iterable, locals, state)?,
                loc,
                state,
            )?);
        }
        let count = streams.iter().map(Vec::len).min().unwrap_or(0);
        for index in 0..count {
            state.tick(loc, "for iteration")?;
            for (item, stream) in items.iter().zip(&streams) {
                if item.name != "_" {
                    locals
                        .values
                        .insert(item.name.clone(), stream[index].clone());
                    locals.mutable.remove(&item.name);
                }
            }
            match self.exec_block(body, locals, state)? {
                flow @ Flow::Return(_) => return Ok(flow),
                Flow::Break => break,
                Flow::Normal | Flow::LoopContinue => {}
            }
        }
        Ok(Flow::Normal)
    }

    fn validate_block(&self, block: &Block) -> Result<()> {
        for located in block {
            match &located.stmt {
                Stmt::Return(expr) => {
                    if let Some(expr) = expr {
                        self.validate_expr(expr)?;
                    }
                }
                Stmt::VarDecl {
                    init: Some(expr), ..
                }
                | Stmt::Expr(expr)
                | Stmt::DestructDecl { init: expr, .. } => self.validate_expr(expr)?,
                Stmt::Assign {
                    target: LValue::Name(_),
                    value,
                }
                | Stmt::DestructAssign { value, .. } => self.validate_expr(value)?,
                Stmt::If {
                    cond,
                    then_block,
                    elifs,
                    else_block,
                } => {
                    self.validate_expr(cond)?;
                    self.validate_block(then_block)?;
                    for (cond, body) in elifs {
                        self.validate_expr(cond)?;
                        self.validate_block(body)?;
                    }
                    if let Some(body) = else_block {
                        self.validate_block(body)?;
                    }
                }
                Stmt::While { cond, body } => {
                    self.validate_expr(cond)?;
                    self.validate_block(body)?;
                }
                Stmt::Loop { count, body } => {
                    if let Some(count) = count {
                        self.validate_expr(count)?;
                    }
                    self.validate_block(body)?;
                }
                Stmt::For { items, body } => {
                    for item in items {
                        self.validate_expr(&item.iterable)?;
                    }
                    self.validate_block(body)?;
                }
                Stmt::Break | Stmt::Continue => {}
                _ => return Err(RuntimeError::msg("statement is not compile-time pure")),
            }
        }
        Ok(())
    }

    fn validate_expr(&self, expr: &Expr) -> Result<()> {
        match &expr.kind {
            ExprKind::Number(_)
            | ExprKind::String(_)
            | ExprKind::Bool(_)
            | ExprKind::None
            | ExprKind::Bytes(_)
            | ExprKind::Var(_) => Ok(()),
            ExprKind::Tuple(items) => {
                for item in items {
                    self.validate_expr(item)?;
                }
                Ok(())
            }
            ExprKind::List(items) | ExprKind::Set(items) => {
                for item in items {
                    self.validate_expr(item)?;
                }
                Ok(())
            }
            ExprKind::Dict(items) => {
                for (key, value) in items {
                    self.validate_expr(key)?;
                    self.validate_expr(value)?;
                }
                Ok(())
            }
            ExprKind::Unary { operand, .. } => self.validate_expr(operand),
            ExprKind::Binary { left, right, .. } => {
                self.validate_expr(left)?;
                self.validate_expr(right)
            }
            ExprKind::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                self.validate_expr(cond)?;
                self.validate_expr(then_expr)?;
                self.validate_expr(else_expr)
            }
            ExprKind::Index { object, index } => {
                self.validate_expr(object)?;
                self.validate_expr(index)
            }
            ExprKind::Slice {
                object,
                start,
                end,
                step,
            } => {
                self.validate_expr(object)?;
                if let Some(start) = start {
                    self.validate_expr(start)?;
                }
                if let Some(end) = end {
                    self.validate_expr(end)?;
                }
                if let Some(step) = step {
                    self.validate_expr(step)?;
                }
                Ok(())
            }
            ExprKind::Member { object, .. } => self.validate_expr(object),
            ExprKind::Call { callee, args } => {
                let ExprKind::Var(name) = &callee.kind else {
                    return Err(RuntimeError::msg("dynamic call"));
                };
                if !const_effect::is_pure_builtin(name) && !self.functions.contains_key(name) {
                    return Err(RuntimeError::msg(format!(
                        "call to runtime function `{name}`"
                    )));
                }
                for arg in args {
                    self.validate_expr(&arg.value)?;
                }
                Ok(())
            }
            _ => Err(RuntimeError::msg("expression is not compile-time pure")),
        }
    }
}

impl EvalState {
    fn new() -> Self {
        Self {
            steps: MAX_CONST_STEPS,
            depth: 0,
            alloc: 0,
            nodes: 0,
            stack: Vec::new(),
            counts: HashMap::new(),
            memo: HashMap::new(),
            active: HashSet::new(),
        }
    }

    fn tick(&mut self, loc: SourceLoc, what: &str) -> Result<()> {
        self.steps = self.steps.checked_sub(1).ok_or_else(|| {
            self.error(
                loc,
                format!("compile-time evaluation step limit exceeded during {what}"),
            )
        })?;
        if let Some(frame) = self.stack.last() {
            *self.counts.entry(frame.name.clone()).or_insert(0) += 1;
        }
        Ok(())
    }

    fn enter(&mut self, name: &str, loc: SourceLoc) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_CONST_DEPTH {
            return Err(self.error(loc, "compile-time recursion depth exceeded"));
        }
        self.stack.push(Frame {
            name: name.to_string(),
            loc,
        });
        Ok(())
    }

    fn leave(&mut self) {
        self.stack.pop();
        self.depth = self.depth.saturating_sub(1);
    }

    fn charge(&mut self, bytes: usize, nodes: usize, loc: SourceLoc) -> Result<()> {
        self.alloc = self.alloc.saturating_add(bytes);
        self.nodes = self.nodes.saturating_add(nodes);
        if self.alloc > MAX_CONST_ALLOC {
            return Err(self.error(loc, "compile-time allocation limit exceeded"));
        }
        if self.nodes > MAX_CONST_NODES {
            return Err(self.error(loc, "compile-time object limit exceeded"));
        }
        Ok(())
    }

    fn charge_collection(&mut self, len: usize, loc: SourceLoc) -> Result<()> {
        self.charge(len.saturating_mul(16), len.saturating_add(1), loc)
    }

    fn error(&self, loc: SourceLoc, message: impl Into<String>) -> RuntimeError {
        let mut parts = vec![message.into()];
        if loc.line > 0 {
            parts.push(format!("at {}:{}", loc.line, loc.column));
        }
        if !self.stack.is_empty() {
            let stack = self
                .stack
                .iter()
                .rev()
                .map(|frame| {
                    format!(
                        "{}:{} in `{}`",
                        frame.loc.line, frame.loc.column, frame.name
                    )
                })
                .collect::<Vec<_>>()
                .join(" <- ");
            parts.push(format!("call stack: {stack}"));
        }
        if let Some((name, count)) = self.counts.iter().max_by_key(|(_, count)| *count) {
            if *count > 1 {
                parts.push(format!("hottest call `{name}` ({count} steps)"));
            }
        }
        RuntimeError::msg(parts.join("; "))
    }
}

fn fingerprint_args(args: &[Value]) -> Result<String> {
    let encoded = args
        .iter()
        .map(module_interface::const_value_json)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(RuntimeError::msg)?;
    Ok(String::from_utf8(module_interface::canonical_json_bytes(
        &serde_json::Value::Array(encoded),
    ))
    .unwrap_or_default())
}

fn iterate(value: &Value, loc: SourceLoc, state: &EvalState) -> Result<Vec<Value>> {
    Ok(match value {
        Value::Tuple(values) => values.to_vec(),
        Value::Text(text) => text.chars().map(|ch| Value::Text(ch.to_string())).collect(),
        Value::Bytes(bytes) => bytes
            .iter()
            .map(|byte| Value::Num(Num::from_i64(i64::from(*byte))))
            .collect(),
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => values.to_vec(),
            FrozenValue::Set(values) => values
                .iter()
                .map(crate::value::value_key_to_value)
                .collect(),
            FrozenValue::Dict(values) => values
                .iter()
                .map(|(key, value)| {
                    Value::Tuple(Arc::from(
                        vec![crate::value::value_key_to_value(key), value.clone()]
                            .into_boxed_slice(),
                    ))
                })
                .collect(),
        },
        _ => {
            return Err(state.error(
                loc,
                "compile-time for can iterate tuple, text, bytes, or frozen collections",
            ))
        }
    })
}

fn bind_pattern(
    pattern: &DestructPattern,
    value: &Value,
    locals: &mut EvalLocals,
    mutable: bool,
    declaring: bool,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<()> {
    match pattern {
        DestructPattern::Name(name) => {
            bind_name(name, value.clone(), locals, mutable, declaring, loc, state)
        }
        DestructPattern::Discard => Ok(()),
        DestructPattern::Tuple(elems) | DestructPattern::List(elems) => {
            let items = sequence_items(value, loc, state)?;
            bind_elems(elems, &items, locals, mutable, declaring, loc, state)
        }
    }
}

fn bind_elems(
    elems: &[DestructElem],
    items: &[Value],
    locals: &mut EvalLocals,
    mutable: bool,
    declaring: bool,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<()> {
    let rest = elems
        .iter()
        .position(|elem| matches!(elem, DestructElem::Rest(_) | DestructElem::RestDiscard));
    if let Some(rest_at) = rest {
        let after = elems.len() - rest_at - 1;
        if items.len() < rest_at + after {
            return Err(state.error(loc, "compile-time destructuring length mismatch"));
        }
        for (elem, value) in elems[..rest_at].iter().zip(items) {
            bind_elem(elem, value, locals, mutable, declaring, loc, state)?;
        }
        let rest_values = items[rest_at..items.len() - after].to_vec();
        match &elems[rest_at] {
            DestructElem::Rest(name) => bind_name(
                name,
                Value::Frozen(Arc::new(FrozenValue::List(rest_values.into()))),
                locals,
                mutable,
                declaring,
                loc,
                state,
            )?,
            DestructElem::RestDiscard => {}
            DestructElem::Pat(_) => {}
        }
        for (elem, value) in elems[rest_at + 1..]
            .iter()
            .zip(&items[items.len() - after..])
        {
            bind_elem(elem, value, locals, mutable, declaring, loc, state)?;
        }
        return Ok(());
    }
    if elems.len() != items.len() {
        return Err(state.error(loc, "compile-time destructuring length mismatch"));
    }
    for (elem, value) in elems.iter().zip(items) {
        bind_elem(elem, value, locals, mutable, declaring, loc, state)?;
    }
    Ok(())
}

fn bind_elem(
    elem: &DestructElem,
    value: &Value,
    locals: &mut EvalLocals,
    mutable: bool,
    declaring: bool,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<()> {
    match elem {
        DestructElem::Pat(pattern) => {
            bind_pattern(pattern, value, locals, mutable, declaring, loc, state)
        }
        DestructElem::Rest(name) => {
            bind_name(name, value.clone(), locals, mutable, declaring, loc, state)
        }
        DestructElem::RestDiscard => Ok(()),
    }
}

fn bind_name(
    name: &str,
    value: Value,
    locals: &mut EvalLocals,
    mutable: bool,
    declaring: bool,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<()> {
    if !declaring {
        if !locals.values.contains_key(name) {
            return Err(state.error(
                loc,
                format!("cannot assign to unbound name `{name}` in const func"),
            ));
        }
        if !locals.mutable.contains(name) {
            return Err(state.error(
                loc,
                format!("cannot assign to immutable binding `{name}` in const func"),
            ));
        }
    }
    locals.values.insert(name.to_string(), value);
    if mutable {
        locals.mutable.insert(name.to_string());
    } else {
        locals.mutable.remove(name);
    }
    Ok(())
}

fn sequence_items(value: &Value, loc: SourceLoc, state: &EvalState) -> Result<Vec<Value>> {
    match value {
        Value::Tuple(values) => Ok(values.to_vec()),
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => Ok(values.to_vec()),
            _ => Err(state.error(
                loc,
                "compile-time destructuring requires a tuple or frozen list",
            )),
        },
        _ => Err(state.error(
            loc,
            "compile-time destructuring requires a tuple or frozen list",
        )),
    }
}

fn index_value(object: &Value, index: &Value, loc: SourceLoc, state: &EvalState) -> Result<Value> {
    match object {
        Value::Tuple(values) => at_index(values, index, loc, state).cloned(),
        Value::Text(text) => {
            let chars: Vec<char> = text.chars().collect();
            Ok(Value::Text(
                at_index(&chars, index, loc, state)?.to_string(),
            ))
        }
        Value::Bytes(bytes) => Ok(Value::Num(Num::from_i64(i64::from(*at_index(
            bytes.as_slice(),
            index,
            loc,
            state,
        )?)))),
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => at_index(values, index, loc, state).cloned(),
            FrozenValue::Dict(values) => {
                let key = ValueKey::from_value(index)?;
                values
                    .iter()
                    .find(|(candidate, _)| candidate == &key)
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| state.error(loc, "compile-time key not found"))
            }
            FrozenValue::Set(_) => Err(state.error(loc, "compile-time set is not indexable")),
        },
        _ => Err(state.error(loc, "value is not compile-time indexable")),
    }
}

fn at_index<'a, T>(
    items: &'a [T],
    index: &Value,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<&'a T> {
    let Value::Num(index) = index else {
        return Err(state.error(loc, "compile-time index must be an integer"));
    };
    let index = index
        .to_i64()
        .ok_or_else(|| state.error(loc, "compile-time index must fit in i64"))?;
    let len = items.len() as i64;
    let at = if index < 0 { len + index } else { index };
    items
        .get(usize::try_from(at).unwrap_or(usize::MAX))
        .ok_or_else(|| state.error(loc, "compile-time index out of range"))
}

fn slice_value(
    object: &Value,
    start: Option<&Value>,
    end: Option<&Value>,
    step: Option<&Value>,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<Value> {
    let step = optional_index(step, loc, state)?.unwrap_or(1);
    if step == 0 {
        return Err(state.error(loc, "compile-time slice step cannot be 0"));
    }
    match object {
        Value::Tuple(values) => {
            let sliced = slice_items(values, start, end, step, loc, state)?;
            Ok(Value::Tuple(Arc::from(sliced.into_boxed_slice())))
        }
        Value::Text(text) => {
            let chars: Vec<char> = text.chars().collect();
            let sliced = slice_items(&chars, start, end, step, loc, state)?;
            Ok(Value::Text(sliced.into_iter().collect()))
        }
        Value::Frozen(frozen) => match frozen.as_ref() {
            FrozenValue::List(values) => {
                let sliced = slice_items(values, start, end, step, loc, state)?;
                Ok(Value::Frozen(Arc::new(FrozenValue::List(sliced.into()))))
            }
            _ => Err(state.error(loc, "compile-time slice requires a list or tuple")),
        },
        _ => Err(state.error(loc, "value is not compile-time sliceable")),
    }
}

fn slice_items<T: Clone>(
    items: &[T],
    start: Option<&Value>,
    end: Option<&Value>,
    step: i64,
    loc: SourceLoc,
    state: &EvalState,
) -> Result<Vec<T>> {
    let len = items.len() as i64;
    let start = optional_index(start, loc, state)?.unwrap_or(if step > 0 { 0 } else { len - 1 });
    let end = optional_index(end, loc, state)?.unwrap_or(if step > 0 { len } else { -len - 1 });
    let start = normalize_bound(start, len);
    let end = normalize_bound(end, len);
    let mut out = Vec::new();
    let mut index = start;
    while if step > 0 { index < end } else { index > end } {
        if let Ok(at) = usize::try_from(index) {
            if let Some(item) = items.get(at) {
                out.push(item.clone());
            }
        }
        index += step;
    }
    Ok(out)
}

fn optional_index(value: Option<&Value>, loc: SourceLoc, state: &EvalState) -> Result<Option<i64>> {
    match value {
        None | Some(Value::None) => Ok(None),
        Some(Value::Num(num)) => num
            .to_i64()
            .map(Some)
            .ok_or_else(|| state.error(loc, "compile-time slice bound must fit in i64")),
        Some(_) => Err(state.error(loc, "compile-time slice bound must be an integer")),
    }
}

fn normalize_bound(index: i64, len: i64) -> i64 {
    if index < 0 {
        (len + index).max(0)
    } else {
        index.min(len)
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(value) => *value,
        Value::Num(value) => !value.is_zero(),
        Value::Text(value) => !value.is_empty(),
        Value::List(value) => !value.borrow().is_empty(),
        Value::Tuple(value) => !value.is_empty(),
        Value::Bytes(value) => !value.is_empty(),
        Value::Frozen(value) => match value.as_ref() {
            FrozenValue::List(value) => !value.is_empty(),
            FrozenValue::Dict(value) => !value.is_empty(),
            FrozenValue::Set(value) => !value.is_empty(),
        },
        _ => true,
    }
}

fn eval_binary(op: BinaryOp, left: &Value, right: &Value) -> Result<Value> {
    Ok(match op {
        BinaryOp::Add => left.add(right)?,
        BinaryOp::Sub => left.sub(right)?,
        BinaryOp::Mul => left.mul(right)?,
        BinaryOp::Div => left.div(right)?,
        BinaryOp::Mod => left.rem(right)?,
        BinaryOp::Pow => left.pow(right)?,
        BinaryOp::BitAnd => left.bitand(right)?,
        BinaryOp::BitOr => left.bitor(right)?,
        BinaryOp::BitXor => left.bitxor(right)?,
        BinaryOp::LShift => left.lshift(right)?,
        BinaryOp::RShift => left.rshift(right)?,
        BinaryOp::Eq => Value::Bool(left.eq(right)?),
        BinaryOp::Ne => Value::Bool(!left.eq(right)?),
        BinaryOp::Is => Value::Bool(left.identical(right)),
        BinaryOp::IsNot => Value::Bool(!left.identical(right)),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let (Value::Num(a), Value::Num(b)) = (left, right) else {
                return Err(RuntimeError::msg(
                    "compile-time ordering currently requires numbers",
                ));
            };
            let ord = a.to_rational().cmp(&b.to_rational());
            Value::Bool(match op {
                BinaryOp::Lt => ord.is_lt(),
                BinaryOp::Le => ord.is_le(),
                BinaryOp::Gt => ord.is_gt(),
                BinaryOp::Ge => ord.is_ge(),
                _ => unreachable!(),
            })
        }
        BinaryOp::And | BinaryOp::Or => right.clone(),
        BinaryOp::In => Value::Bool(const_effect::value_in(left, right)?),
    })
}
