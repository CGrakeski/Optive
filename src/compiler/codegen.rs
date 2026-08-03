use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::ast::*;
use crate::error::RuntimeError;
use crate::free_vars;
use crate::opcode::{Codegen, CompiledProgram, FunctionObject, GenericFunctionTemplate, Instruction, MacroObject};
use crate::protocol::{self, TypeCheckContext};
use crate::monomorph;
use crate::runtime_ast;
use crate::value::{FieldTypeInfo, Num, StructDef, Value};
use crate::Result;

use crate::shared::Shared;

fn is_c_layout_annotation(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Attr { object, field } if field == "layout" => {
            matches!(object.as_ref(), TypeExpr::Name(n) if n == "C")
        }
        TypeExpr::Name(n) if n == "C.layout" => true,
        _ => false,
    }
}

enum CompKind {
    List,
    Set,
    Dict,
}

/// 编译期打开的 try/with，供 return/break/continue 补发清理。
#[derive(Clone)]
enum OpenHandler {
    Try,
    With { ctx: String },
}

pub struct Generator {
    cg: Codegen,
    program: CompiledProgram,
    loop_break_labels: Vec<usize>,
    loop_continue_labels: Vec<usize>,
    /// 进入循环时 `handler_stack.len()`，break/continue 只清到该深度。
    loop_handler_depths: Vec<usize>,
    /// 与 break/continue 标签对齐：计数 `loop` 在栈上压了倒计时器。
    loop_owns_stack_counter: Vec<bool>,
    /// 当前词法位置仍打开的 try/with（innermost last）。
    handler_stack: Vec<OpenHandler>,
    type_env: Vec<HashMap<String, (TypeExpr, bool)>>,
    macro_depth: usize,
    match_expr_ends: Vec<usize>,
    local_slots: Option<HashMap<String, usize>>,
    next_local_slot: usize,
    /// 顶层已 `NewVar` 过的临时名，避免每次 `emit_store_temp` 重复建名。
    declared_temps: HashSet<String>,
    global_index: FxHashMap<String, usize>,
    next_global_sym: usize,
    current_func: Option<String>,
    block_depth: usize,
    captured_names: HashSet<String>,
    current_return_wrapper: Option<Expr>,
    /// 正在编译生成器函数体（`yield` / `return expr` 语义不同）。
    compiling_generator: bool,
}

struct CompileFnExtras<'a> {
    return_type: Option<&'a TypeExpr>,
    return_strong: bool,
    return_wrapper: Option<Expr>,
    captured_names: HashSet<String>,
    is_generator: bool,
}

impl Generator {
    pub fn new() -> Self {
        Self {
            cg: Codegen::new(),
            program: CompiledProgram::new(),
            loop_break_labels: Vec::new(),
            loop_continue_labels: Vec::new(),
            loop_handler_depths: Vec::new(),
            loop_owns_stack_counter: Vec::new(),
            handler_stack: Vec::new(),
            type_env: vec![HashMap::new()],
            macro_depth: 0,
            match_expr_ends: Vec::new(),
            local_slots: None,
            next_local_slot: 0,
            declared_temps: HashSet::new(),
            global_index: FxHashMap::default(),
            next_global_sym: 0,
            current_func: None,
            block_depth: 0,
            captured_names: HashSet::new(),
            current_return_wrapper: None,
            compiling_generator: false,
        }
    }

    /// 块内是否含本层 `yield`/`yield from`（不进入嵌套 `func`/`do` 体）。
    fn block_has_yield(body: &Block) -> bool {
        fn stmt_has(stmt: &Stmt) -> bool {
            match stmt {
                Stmt::Yield(_) | Stmt::YieldFrom(_) => true,
                Stmt::If {
                    then_block,
                    elifs,
                    else_block,
                    ..
                } => {
                    Generator::block_has_yield(then_block)
                        || elifs.iter().any(|(_, b)| Generator::block_has_yield(b))
                        || else_block
                            .as_ref()
                            .is_some_and(Generator::block_has_yield)
                }
                Stmt::While { body, .. }
                | Stmt::Loop { body, .. }
                | Stmt::For { body, .. }
                | Stmt::With { body, .. }
                | Stmt::Block(body) => Generator::block_has_yield(body),
                Stmt::Try {
                    body,
                    catches,
                    else_block,
                } => {
                    Generator::block_has_yield(body)
                        || catches.iter().any(|c| Generator::block_has_yield(&c.body))
                        || else_block
                            .as_ref()
                            .is_some_and(Generator::block_has_yield)
                }
                Stmt::Match {
                    cases,
                    else_block,
                    ..
                } => {
                    cases.iter().any(|c| Generator::block_has_yield(&c.body))
                        || else_block
                            .as_ref()
                            .is_some_and(Generator::block_has_yield)
                }
                // 嵌套 FuncDecl 的 yield 只属于内层；do 在表达式里单独编译。
                _ => false,
            }
        }
        body.iter().any(|s| stmt_has(&s.stmt))
    }

    fn global_slot(&mut self, name: &str) -> usize {
        if let Some(&slot) = self.global_index.get(name) {
            self.ensure_global_name_at(slot, name);
            return slot;
        }
        let slot = self.next_global_sym;
        self.next_global_sym += 1;
        self.global_index.insert(name.to_string(), slot);
        self.ensure_global_name_at(slot, name);
        slot
    }

    /// 保证 `program.global_names[slot] == name`。
    /// 子编译器会克隆父级 `global_index`，但 `fresh_subprogram` 的 `global_names` 为空；
    /// 若不回填，函数 `module_env` 会缺名，`LoadGlobal` 下标就会错位。
    fn ensure_global_name_at(&mut self, slot: usize, name: &str) {
        if slot >= self.program.global_names.len() {
            self.program.global_names.resize(slot + 1, String::new());
        }
        let existing = &self.program.global_names[slot];
        if existing.is_empty() {
            self.program.global_names[slot] = name.to_string();
        } else if existing != name {
            // 不变量：同一槽只能有一个名字。静默保留旧名会导致 Store/Load 绑错导出。
            panic!(
                "internal: global slot {slot} claimed by both `{existing}` and `{name}`"
            );
        }
    }

    fn is_self_call(&self, callee: &Expr) -> bool {
        match (self.current_func.as_deref(), &callee.kind) {
            (Some(func), ExprKind::Var(name)) => func == name.as_str(),
            _ => false,
        }
    }

    fn current_func_has_flexible_params(&self) -> bool {
        let Some(name) = self.current_func.as_deref() else {
            return false;
        };
        self.program
            .functions
            .get(name)
            .map(|f| {
                f.variadic_param_index.is_some()
                    || f.kwvariadic_param_index.is_some()
                    || f.defaults.iter().any(|d| d.is_some())
                    || f.params.iter().any(|p| p.default_expr.is_some())
            })
            .unwrap_or(false)
    }

    fn emit_function_value_with_defaults(
        &mut self,
        params: &[FuncParam],
        func: FunctionObject,
    ) -> Result<()> {
        let default_exprs: Vec<&Expr> = params
            .iter()
            .filter_map(|p| p.default_expr.as_ref())
            .collect();
        if default_exprs.is_empty() {
            self.cg
                .emit(Instruction::Push(Value::Function(Arc::new(func))));
            return Ok(());
        }
        for expr in &default_exprs {
            self.gen_expr(expr)?;
        }
        self.cg.emit(Instruction::VecNew(default_exprs.len()));
        self.cg
            .emit(Instruction::Push(Value::Function(Arc::new(func))));
        self.cg
            .emit(Instruction::Load("__attach_defaults__".into()));
        self.cg.emit(Instruction::Call { argc: 2 });
        Ok(())
    }

    /// 暂存栈上的 `args`/`kwargs`，对其一做原地修改后再压回栈。
    /// `mutate_args == true` 时修改 args 列表，否则修改 kwargs 字典。
    fn with_call_arg_temps(
        &mut self,
        mutate_args: bool,
        f: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let args_tmp = self.cg.fresh_temp("__call_args");
        let kw_tmp = self.cg.fresh_temp("__call_kwargs");
        self.emit_store_temp(&kw_tmp);
        self.emit_store_temp(&args_tmp);
        if mutate_args {
            self.emit_load_temp(&args_tmp);
        } else {
            self.emit_load_temp(&kw_tmp);
        }
        f(self)?;
        if mutate_args {
            self.emit_store_temp(&args_tmp);
        } else {
            self.emit_store_temp(&kw_tmp);
        }
        self.emit_load_temp(&args_tmp);
        self.emit_load_temp(&kw_tmp);
        Ok(())
    }

    /// 生成位置参数列表与关键字参数字典（栈：args_list, kwargs_dict）。
    fn gen_call_args_and_kwargs(&mut self, args: &[CallArg]) -> Result<()> {
        self.cg.emit(Instruction::VecNew(0));
        self.cg.emit(Instruction::DictNew(0));
        for a in args {
            if a.is_kwsplat {
                // kwargs = update(kwargs, dict) — 合并关键字参数字典
                self.gen_expr(&a.value)?;
                self.cg
                    .emit(Instruction::Load("__merge_kwargs__".into()));
                self.cg.emit(Instruction::Call { argc: 2 });
            } else if let Some(name) = &a.name {
                // kwargs[name] = value；保留 kwargs 在栈上
                self.with_call_arg_temps(false, |this| {
                    this.cg
                        .emit(Instruction::Push(Value::Text(name.clone())));
                    this.gen_expr(&a.value)?;
                    this.cg.emit(Instruction::DictSet);
                    Ok(())
                })?;
            } else if a.is_splat {
                self.with_call_arg_temps(true, |this| {
                    this.gen_expr(&a.value)?;
                    this.cg.emit(Instruction::ListExtend);
                    Ok(())
                })?;
            } else {
                self.with_call_arg_temps(true, |this| {
                    this.gen_expr(&a.value)?;
                    this.cg.emit(Instruction::ListAppend);
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn ensure_local_slot(&mut self, name: &str) -> usize {
        let slots = self.local_slots.get_or_insert_with(HashMap::new);
        if let Some(&slot) = slots.get(name) {
            return slot;
        }
        let slot = self.next_local_slot;
        slots.insert(name.to_string(), slot);
        self.next_local_slot += 1;
        slot
    }

    fn emit_bind_name(&mut self, name: &str) {
        self.emit_bind_name_flags(name, false);
    }

    fn emit_bind_name_flags(&mut self, name: &str, is_const: bool) {
        if self.local_slots.is_some() {
            let slot = self.ensure_local_slot(name);
            self.cg.emit(Instruction::BindFast {
                slot,
                name: name.to_string(),
                is_const,
            });
        } else {
            self.cg.emit(Instruction::NewVar {
                name: name.to_string(),
                is_const,
            });
            self.cg.emit(Instruction::Store(name.to_string()));
        }
    }

    fn emit_store_temp(&mut self, name: &str) {
        if self.local_slots.is_some() {
            let slot = self.ensure_local_slot(name);
            self.cg.emit(Instruction::StoreFast(slot));
        } else {
            if self.declared_temps.insert(name.to_string()) {
                self.cg.emit(Instruction::NewVar {
                    name: name.to_string(),
                    is_const: false,
                });
            }
            self.cg.emit(Instruction::Store(name.to_string()));
        }
    }

    fn emit_load_temp(&mut self, name: &str) {
        if let Some(slots) = &self.local_slots {
            if let Some(&slot) = slots.get(name) {
                self.cg.emit(Instruction::LoadFast(slot));
                return;
            }
        }
        self.cg.emit(Instruction::Load(name.to_string()));
    }

    fn emit_load_name(&mut self, name: &str) {
        let local_binding = self
            .local_slots
            .as_ref()
            .and_then(|slots| slots.get(name).copied());
        if local_binding.is_none()
            && (self.program.struct_defs.contains_key(name)
                || crate::type_registry::is_registered_primitive(name)
                || name.starts_with("C.types."))
        {
            self.cg
                .emit(Instruction::Push(Value::type_ref(name.to_string())));
            return;
        }
        if let Some(slot) = local_binding {
            self.cg.emit(Instruction::LoadFast(slot));
            return;
        }
        if self.local_slots.is_some() {
            if self.captured_names.contains(name) {
                self.cg.emit(Instruction::Load(name.to_string()));
                return;
            }
            let slot = self.global_slot(name);
            self.cg.emit(Instruction::LoadGlobal(slot));
            return;
        }
        if self.macro_depth > 0 || self.block_depth > 0 {
            self.cg.emit(Instruction::Load(name.to_string()));
            return;
        }
        let slot = self.global_slot(name);
        self.cg.emit(Instruction::LoadGlobal(slot));
    }

    fn emit_store_name(&mut self, name: &str) {
        if let Some(slots) = &self.local_slots {
            if let Some(&slot) = slots.get(name) {
                self.cg.emit(Instruction::StoreFast(slot));
                return;
            }
            // 与 emit_load_name 对称：捕获变量走名字 Store（Cell），勿写成 StoreGlobal。
            if self.captured_names.contains(name) {
                self.cg.emit(Instruction::Store(name.to_string()));
                return;
            }
            let slot = self.global_slot(name);
            self.cg.emit(Instruction::StoreGlobal(slot));
            return;
        }
        if self.macro_depth > 0 || self.block_depth > 0 {
            self.cg.emit(Instruction::Store(name.to_string()));
            return;
        }
        let slot = self.global_slot(name);
        self.cg.emit(Instruction::StoreGlobal(slot));
    }

    fn expr_is_generic_type_formable(&self, expr: &Expr) -> bool {
        matches!(
            &expr.kind, ExprKind::Var(name) if self
                .program
                .struct_defs
                .get(name.as_str())
                .is_some_and(|def| !def.type_params.is_empty())
        )
    }

    fn gen_type_index_operand(&mut self, expr: &Expr) -> Result<()> {
        match &expr.kind {
            ExprKind::Var(name) => {
                self.cg
                    .emit(Instruction::Push(Value::type_ref(name.clone())));
            }
            ExprKind::Index { object, index } => {
                self.gen_type_index_operand(object)?;
                self.gen_type_index_operand(index)?;
                self.cg.emit(Instruction::Index);
            }
            ExprKind::List(elems) => {
                for e in elems {
                    self.gen_type_index_operand(e)?;
                }
                self.cg.emit(Instruction::VecNew(elems.len()));
            }
            _ => self.gen_expr(expr)?,
        }
        Ok(())
    }

    pub fn compile(mut self, program: &Program) -> Result<CompiledProgram> {
        for stmt in &program.stmts {
            self.gen_stmt(stmt, true)?;
        }
        self.cg.emit(Instruction::Ret);
        self.cg
            .patch_labels()
            .map_err(RuntimeError::msg)?;
        crate::specialize::specialize_instructions(&mut self.cg.code);
        let (compacted, remap) = crate::opcode::compact_bytecode(std::mem::take(&mut self.cg.code));
        let (fused, remap2) = crate::opcode::peephole_fuse(compacted);
        self.cg.code = fused;
        self.program.code = std::mem::take(&mut self.cg.code);
        self.program.hot = crate::hot_code::HotCode::encode(&self.program.code);
        let lm = crate::opcode::compact_parallel(&self.cg.line_map, &remap);
        self.program.line_map = crate::opcode::compact_parallel(&lm, &remap2);
        let cm = crate::opcode::compact_parallel(&self.cg.column_map, &remap);
        self.program.column_map = crate::opcode::compact_parallel(&cm, &remap2);
        self.attach_compile_global_envs();
        Ok(self.program)
    }

    /// 将本编译单元的全局名表绑定到各函数，使 `LoadGlobal` 下标
    /// 在 REPL 多次 `load_program` 替换后仍保持有效。
    fn attach_compile_global_envs(&mut self) {
        let env = Arc::new(crate::opcode::ModuleGlobalEnv {
            global_names: self.program.global_names.clone(),
            globals: crate::shared::SyncCell::new(HashMap::new()),
            finalized: false,
        });
        let updated: HashMap<String, Arc<FunctionObject>> = self
            .program
            .functions
            .iter()
            .map(|(k, f)| {
                let func = if f.module_env.is_none() {
                    let mut func = (**f).clone();
                    func.module_env = Some(env.clone());
                    Arc::new(func)
                } else {
                    f.clone()
                };
                (k.clone(), func)
            })
            .collect();
        self.program.functions = updated;
        Self::patch_function_pushes(&mut self.program.code, &self.program.functions, &env);
        // 函数体内嵌套的 `Push(Function)`。
        let names: Vec<String> = self.program.functions.keys().cloned().collect();
        for name in names {
            let Some(f) = self.program.functions.get(&name).cloned() else {
                continue;
            };
            let mut body = (*f.body).clone();
            if !Self::patch_function_pushes(&mut body, &self.program.functions, &env) {
                continue;
            }
            let mut func = (*f).clone();
            func.body = Arc::new(body);
            func.hot = crate::hot_code::HotCode::encode(&func.body);
            self.program.functions.insert(name, Arc::new(func));
        }
    }

    fn patch_function_pushes(
        code: &mut [Instruction],
        functions: &HashMap<String, Arc<FunctionObject>>,
        env: &Arc<crate::opcode::ModuleGlobalEnv>,
    ) -> bool {
        let mut changed = false;
        for ins in code.iter_mut() {
            if let Instruction::Push(Value::Function(f)) = ins {
                if f.module_env.is_some() {
                    continue;
                }
                let replacement = if let Some(u) = functions.get(&f.name) {
                    if u.module_env.is_some() {
                        u.clone()
                    } else {
                        let mut func = (**u).clone();
                        func.module_env = Some(env.clone());
                        Arc::new(func)
                    }
                } else {
                    let mut func = (**f).clone();
                    func.module_env = Some(env.clone());
                    Arc::new(func)
                };
                *ins = Instruction::Push(Value::Function(replacement));
                changed = true;
            }
        }
        changed
    }

    /// 编译运行时展开片段（如 quote 体）。`lexical_bindings` 非空时名字经 `Load`/`Store`
    /// 解析，使运行时提供的 quote `with` 绑定可见。
    pub fn compile_snippet(program: &Program, lexical_bindings: &[String]) -> Result<CompiledProgram> {
        let mut gen = Self::new();
        if !lexical_bindings.is_empty() {
            gen.block_depth = 1;
        }
        gen.compile(program)
    }

    fn push_type_scope(&mut self) {
        self.type_env.push(HashMap::new());
    }

    fn pop_type_scope(&mut self) {
        if self.type_env.len() > 1 {
            self.type_env.pop();
        }
    }

    fn bind_type(&mut self, name: &str, ty: TypeExpr, strict: bool) {
        if let Some(scope) = self.type_env.last_mut() {
            scope.insert(name.to_string(), (ty, strict));
        }
    }

    fn lookup_strict_type(&self, name: &str) -> Option<&TypeExpr> {
        for scope in self.type_env.iter().rev() {
            if let Some((ty, strict)) = scope.get(name) {
                if *strict {
                    return Some(ty);
                }
                return None;
            }
        }
        None
    }

    fn emit_type_check(&mut self, ty: &TypeExpr) {
        self.cg.emit(Instruction::TypeCheck(ty.clone()));
    }

    fn field_strict(typed: bool, type_strong: bool, type_expr: &Option<TypeExpr>) -> bool {
        type_expr.is_some() && (type_strong || typed)
    }

    fn maybe_register_export(&mut self, visibility: Visibility, name: &str, top_level: bool) {
        // 历史语义：无修饰符与 `export` 均注册为导出；仅 `intern` 不导出。
        if top_level && visibility != Visibility::Internal {
            self.cg
                .emit(Instruction::RegisterExport(name.to_string()));
        }
    }

    fn gen_stmt(&mut self, located: &LocatedStmt, top_level: bool) -> Result<()> {
        self.cg.set_loc(located.line, located.column);
        let stmt = &located.stmt;
        match stmt {
            Stmt::VarDecl {
                name,
                is_const,
                init,
                type_expr,
                type_strong,
                visibility,
                ..
            } => {
                if self.local_slots.is_some() {
                    let slot = self.ensure_local_slot(name);
                    if let Some(init) = init {
                        self.gen_expr(init)?;
                        if *type_strong {
                            if let Some(ty) = type_expr {
                                self.emit_type_check(ty);
                            }
                        }
                    } else {
                        self.cg.emit(Instruction::Push(Value::None));
                    }
                    self.cg.emit(Instruction::BindFast {
                        slot,
                        name: name.clone(),
                        is_const: *is_const,
                    });
                } else {
                    self.cg.emit(Instruction::NewVar {
                        name: name.clone(),
                        is_const: *is_const,
                    });
                    if let Some(init) = init {
                        self.gen_expr(init)?;
                        if *type_strong {
                            if let Some(ty) = type_expr {
                                self.emit_type_check(ty);
                            }
                        }
                        self.emit_store_name(name);
                    }
                }
                if let Some(ty) = type_expr {
                    if *type_strong {
                        self.bind_type(name, ty.clone(), true);
                    }
                }
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::DestructDecl {
                pattern,
                init,
                is_const,
                visibility,
                ..
            } => {
                self.gen_expr(init)?;
                self.gen_destruct_bind(pattern, *is_const, true)?;
                if top_level {
                    for name in destruct_bound_names(pattern) {
                        self.maybe_register_export(*visibility, &name, true);
                    }
                }
            }
            Stmt::DestructAssign { pattern, value } => {
                self.gen_expr(value)?;
                self.gen_destruct_bind(pattern, false, false)?;
            }
            Stmt::Assign { target, value } => match target {
                LValue::Member { object, field } => {
                    self.gen_expr(object)?;
                    self.gen_expr(value)?;
                    self.cg.emit(Instruction::SetField(field.clone()));
                }
                LValue::Index { object, index } => {
                    self.gen_expr(object)?;
                    self.gen_expr(index)?;
                    self.gen_expr(value)?;
                    self.cg.emit(Instruction::IndexSet);
                }
                LValue::Slice {
                    object,
                    start,
                    end,
                    step,
                } => {
                    self.gen_expr(object)?;
                    self.gen_slice_bound(start.as_deref())?;
                    self.gen_slice_bound(end.as_deref())?;
                    self.gen_slice_bound(step.as_deref())?;
                    self.gen_expr(value)?;
                    self.cg.emit(Instruction::SliceSet);
                }
                LValue::Name(name) => {
                    self.gen_expr(value)?;
                    if let Some(ty) = self.lookup_strict_type(name).cloned() {
                        self.emit_type_check(&ty);
                    }
                    self.emit_store_name(name);
                }
            },
            Stmt::FuncDecl {
                name,
                type_params,
                params,
                body,
                return_type,
                return_strong,
                return_wrapper,
                visibility,
                decorators,
                is_generator,
            } => {
                let is_gen = *is_generator;
                if type_params.is_empty() {
                    let param_names: HashSet<String> =
                        params.iter().map(|p| p.name.clone()).collect();
                    let free = if self.local_slots.is_some() {
                        free_vars::free_vars_in_block(body, &param_names)
                    } else {
                        Vec::new()
                    };
                    let captured: HashSet<String> = free.iter().cloned().collect();
                    let func = self.compile_function(
                        name,
                        params,
                        body,
                        CompileFnExtras {
                            return_type: return_type.as_ref(),
                            return_strong: *return_strong,
                            return_wrapper: return_wrapper.clone(),
                            captured_names: captured,
                            is_generator: is_gen,
                        },
                    )?;
                    self.program.functions.insert(name.clone(), Arc::new(func.clone()));
                    self.emit_function_value_with_defaults(params, func)?;
                    if !free.is_empty() {
                        for fname in &free {
                            self.cg
                                .emit(Instruction::Push(Value::Text(fname.clone())));
                            self.cg.emit(Instruction::Load(fname.clone()));
                        }
                        self.cg.emit(Instruction::DictNew(free.len()));
                        self.cg
                            .emit(Instruction::Load("__make_closure__".into()));
                        self.cg.emit(Instruction::Call { argc: 2 });
                    }
                } else {
                    let template = Arc::new(GenericFunctionTemplate {
                        name: name.clone(),
                        type_params: type_params.clone(),
                        params: params.clone(),
                        body: body.clone(),
                        return_type: return_type.clone(),
                        return_strong: *return_strong,
                        return_wrapper: return_wrapper.clone(),
                        is_generator: is_gen,
                        source: None,
                        source_file: "<script>".into(),
                    });
                    self.program
                        .generic_functions
                        .insert(name.clone(), template.clone());
                    self.cg
                        .emit(Instruction::Push(Value::GenericFunction(template)));
                }
                self.gen_apply_decorators(decorators)?;
                if self.local_slots.is_some() {
                    let slot = self.ensure_local_slot(name);
                    self.cg.emit(Instruction::BindFast {
                        slot,
                        name: name.clone(),
                        is_const: false,
                    });
                } else {
                    self.cg.emit(Instruction::NewVar {
                        name: name.clone(),
                        is_const: false,
                    });
                    self.emit_store_name(name);
                }
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::ProtocolDecl {
                name,
                members,
                visibility,
            } => {
                let def = Arc::new(protocol::protocol_from_members(name.clone(), members.clone()));
                self.program.protocols.insert(name.clone(), def);
                self.cg
                    .emit(Instruction::Push(Value::type_ref(name.clone())));
                self.cg.emit(Instruction::NewVar {
                    name: name.clone(),
                    is_const: true,
                });
                self.emit_store_name(name);
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::MacroDecl {
                name,
                params,
                body,
                visibility,
            } => {
                let mac = self.compile_macro(name, params, body)?;
                self.program.macros.insert(name.clone(), Arc::new(mac.clone()));
                self.cg.emit(Instruction::Push(Value::Macro(Arc::new(mac))));
                self.cg.emit(Instruction::NewVar {
                    name: name.clone(),
                    is_const: false,
                });
                self.cg.emit(Instruction::Store(name.clone()));
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::FriendFuncDecl {
                name,
                params,
                body,
                return_type,
                return_strong,
                return_wrapper,
                visibility,
            } => {
                if let (Some(params), Some(body)) = (params, body) {
                    let handler = self.compile_function(
                        &format!("{name}.__dispatch__"),
                        params,
                        body,
                        CompileFnExtras {
                            return_type: return_type.as_ref(),
                            return_strong: *return_strong,
                            return_wrapper: return_wrapper.clone(),
                            captured_names: HashSet::new(),
                            is_generator: false,
                        },
                    )?;
                    self.cg
                        .emit(Instruction::Push(Value::Text(name.clone())));
                    self.cg
                        .emit(Instruction::Push(Value::Function(Arc::new(handler))));
                    self.cg
                        .emit(Instruction::Load("__register_dispatch_handler__".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                } else {
                    self.cg.emit(Instruction::Push(Value::Text(name.clone())));
                    self.cg
                        .emit(Instruction::Load("__ensure_dispatch__".into()));
                    self.cg.emit(Instruction::Call { argc: 1 });
                }
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::Return(expr) => {
                if self.compiling_generator {
                    // 8B：`return expr` ≡ 再 yield 一次后结束；裸 return 直接结束。
                    if let Some(e) = expr {
                        self.gen_expr(e)?;
                        self.cg.emit(Instruction::Yield);
                    }
                    self.cg.emit(Instruction::Push(Value::None));
                    self.gen_return_from_tos()?;
                } else {
                    self.gen_return_expr(expr.as_ref())?;
                }
            }
            Stmt::Yield(expr) => {
                if !self.compiling_generator {
                    return Err(RuntimeError::msg(
                        "`yield` is only valid inside a generator function or do",
                    ));
                }
                if let Some(e) = expr {
                    self.gen_expr(e)?;
                } else {
                    self.cg.emit(Instruction::Push(Value::None));
                }
                self.cg.emit(Instruction::Yield);
            }
            Stmt::YieldFrom(expr) => {
                if !self.compiling_generator {
                    return Err(RuntimeError::msg(
                        "`yield from` is only valid inside a generator function or do",
                    ));
                }
                self.gen_expr(expr)?;
                self.cg.emit(Instruction::YieldFrom);
            }
            Stmt::Expr(e) => {
                self.gen_expr(e)?;
                if top_level {
                    // 顶层保留栈顶值供 REPL / 最后结果
                } else {
                    self.cg.emit(Instruction::Pop);
                }
            }
            Stmt::If {
                cond,
                then_block,
                elifs,
                else_block,
            } => {
                self.gen_if(cond, then_block, elifs, else_block, top_level)?;
            }
            Stmt::While { cond, body } => {
                let start = self.cg.fresh_label();
                let end = self.cg.fresh_label();
                self.cg.mark_label(start);
                self.gen_expr(cond)?;
                let jmp = self.cg.emit(Instruction::GotoIfNot(end));
                self.loop_break_labels.push(end);
                self.loop_continue_labels.push(start);
                self.loop_handler_depths.push(self.handler_stack.len());
                self.loop_owns_stack_counter.push(false);
                for s in body {
                    self.gen_stmt(s, false)?;
                }
                self.loop_break_labels.pop();
                self.loop_continue_labels.pop();
                self.loop_handler_depths.pop();
                self.loop_owns_stack_counter.pop();
                self.cg.emit(Instruction::Goto(start));
                self.cg.mark_label(end);
                let _ = jmp;
            }
            Stmt::Loop { count, body } => {
                let start = self.cg.fresh_label();
                let end = self.cg.fresh_label();
                let owns_counter = count.is_some();
                if let Some(c) = count {
                    self.gen_expr(c)?;
                }
                self.cg.mark_label(start);
                if owns_counter {
                    let done = self.cg.fresh_label();
                    self.cg.emit(Instruction::LoopCountdown(done));
                    self.loop_break_labels.push(end);
                    self.loop_continue_labels.push(start);
                    self.loop_handler_depths.push(self.handler_stack.len());
                    self.loop_owns_stack_counter.push(true);
                    for s in body {
                        self.gen_stmt(s, false)?;
                    }
                    self.loop_break_labels.pop();
                    self.loop_continue_labels.pop();
                    self.loop_handler_depths.pop();
                    self.loop_owns_stack_counter.pop();
                    self.cg.emit(Instruction::Goto(start));
                    self.cg.mark_label(done);
                    // break 跳向 `end`；与 `done` 同 PC，否则 patch_labels 报 undefined label。
                    self.cg.mark_label(end);
                } else {
                    self.loop_break_labels.push(end);
                    self.loop_continue_labels.push(start);
                    self.loop_handler_depths.push(self.handler_stack.len());
                    self.loop_owns_stack_counter.push(false);
                    for s in body {
                        self.gen_stmt(s, false)?;
                    }
                    self.loop_break_labels.pop();
                    self.loop_continue_labels.pop();
                    self.loop_handler_depths.pop();
                    self.loop_owns_stack_counter.pop();
                    self.cg.emit(Instruction::Goto(start));
                    self.cg.mark_label(end);
                }
            }
            Stmt::For { items, body } => {
                self.gen_for(items, body)?;
            }
            Stmt::Break => {
                let lbl = *self
                    .loop_break_labels
                    .last()
                    .ok_or_else(|| RuntimeError::msg("break outside loop"))?;
                let depth = *self
                    .loop_handler_depths
                    .last()
                    .ok_or_else(|| RuntimeError::msg("break outside loop"))?;
                let owns_counter = *self
                    .loop_owns_stack_counter
                    .last()
                    .ok_or_else(|| RuntimeError::msg("break outside loop"))?;
                self.emit_handler_exit_cleanups(depth);
                if owns_counter {
                    self.cg.emit(Instruction::Pop);
                }
                self.cg.emit(Instruction::Goto(lbl));
            }
            Stmt::Continue => {
                let lbl = *self
                    .loop_continue_labels
                    .last()
                    .ok_or_else(|| RuntimeError::msg("continue outside loop"))?;
                let depth = *self
                    .loop_handler_depths
                    .last()
                    .ok_or_else(|| RuntimeError::msg("continue outside loop"))?;
                self.emit_handler_exit_cleanups(depth);
                self.cg.emit(Instruction::Goto(lbl));
            }
            Stmt::Block(stmts) => {
                self.cg.emit(Instruction::EnterScope);
                self.block_depth += 1;
                self.gen_block(stmts, top_level)?;
                self.block_depth -= 1;
                self.cg.emit(Instruction::LeaveScope);
            }
            Stmt::StructDecl {
                visibility,
                typed,
                name,
                type_params,
                base,
                fields,
                methods,
                layout,
                ..
            } => {
                let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                let mutable: Vec<bool> = fields.iter().map(|f| f.mutable).collect();
                let field_types: Vec<FieldTypeInfo> = fields
                    .iter()
                    .map(|f| FieldTypeInfo {
                        type_expr: f.type_expr.clone(),
                        strict: Self::field_strict(*typed, f.type_strong, &f.type_expr),
                    })
                    .collect();
                let c_layout = if let Some(layout_ty) = layout {
                    if !is_c_layout_annotation(layout_ty) {
                        return Err(RuntimeError::type_err(format!(
                            "struct '{name}': unsupported layout annotation (expected C.layout)"
                        )));
                    }
                    if !*typed {
                        return Err(RuntimeError::type_err(format!(
                            "struct '{name}': C.layout requires `typed struct`"
                        )));
                    }
                    let tmp = StructDef {
                        name: name.clone(),
                        base: base.clone(),
                        fields: field_names.clone(),
                        mutable_fields: mutable.clone(),
                        typed: *typed,
                        field_types: field_types.clone(),
                        type_params: type_params.clone(),
                        c_layout: None,
                    };
                    Some(Arc::new(crate::ffi_extra::layout_from_struct_def(&tmp)?))
                } else {
                    None
                };
                let def = Arc::new(StructDef {
                    name: name.clone(),
                    base: base.clone(),
                    fields: field_names,
                    mutable_fields: mutable,
                    typed: *typed,
                    field_types,
                    type_params: type_params.clone(),
                    c_layout,
                });
                self.program.struct_defs.insert(name.clone(), def.clone());
                for m in methods {
                    let full_name = if m.outside {
                        m.name.clone()
                    } else {
                        format!("{name}.{}", m.name)
                    };
                    let mut params = m.params.clone();
                    if !m.outside && !params.first().map(|p| p.name == "self").unwrap_or(false) {
                        params.insert(
                            0,
                            FuncParam {
                                name: "self".into(),
                                is_variadic: false,
                                is_kwvariadic: false,
                                implicit: false,
                                type_expr: None,
                                type_strong: false,
                                default_expr: None,
                            },
                        );
                    }
                    let return_strong = m.return_strong;
                    let func = self.compile_function(
                        &full_name,
                        &params,
                        &m.body,
                        CompileFnExtras {
                            return_type: m.return_type.as_ref(),
                            return_strong,
                            return_wrapper: m.return_wrapper.clone(),
                            captured_names: HashSet::new(),
                            is_generator: false,
                        },
                    )?;
                    let func_rc = Arc::new(func);
                    if m.outside {
                        self.program
                            .functions
                            .insert(full_name.clone(), func_rc.clone());
                        self.program.functions.insert(
                            format!("{name}.{}", m.name),
                            func_rc,
                        );
                    } else if m.overload {
                        self.program.functions.insert(
                            format!("{full_name}#{}", self.program.functions.len()),
                            func_rc.clone(),
                        );
                        self.program
                            .overload_tables
                            .entry(full_name.clone())
                            .or_default()
                            .push(func_rc);
                    } else {
                        self.program.functions.insert(full_name, func_rc);
                    }
                }
                self.cg.emit(Instruction::Push(Value::type_ref(name.clone())));
                self.cg.emit(Instruction::NewVar {
                    name: name.clone(),
                    is_const: false,
                });
                self.emit_store_name(name);
                self.maybe_register_export(*visibility, name, top_level);
            }
            Stmt::EnumDecl {
                name,
                members,
                methods,
                visibility,
            } => {
                self.gen_enum_decl(name, members, methods, *visibility, top_level)?;
            }
            Stmt::VariantDecl {
                name,
                type_params,
                cases,
                visibility,
            } => {
                self.gen_variant_decl(name, type_params, cases, *visibility, top_level)?;
            }
            Stmt::Import {
                path,
                path_is_string,
                alias,
            } => {
                let bind = if *path_is_string {
                    alias.clone().unwrap_or_else(|| {
                        std::path::Path::new(path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("module")
                            .to_string()
                    })
                } else {
                    alias
                        .clone()
                        .unwrap_or_else(|| path.rsplit('.').next().unwrap_or(path).to_string())
                };
                let find = if *path_is_string {
                    format!("@str:{path}")
                } else {
                    path.clone()
                };
                self.cg.emit(Instruction::FindMod(find));
                self.emit_bind_name(&bind);
            }
            Stmt::Use { module, items } => {
                let temp = self.cg.fresh_temp("__use_mod");
                self.emit_load_module_ref(module)?;
                self.emit_store_temp(&temp);
                for item in items {
                    self.emit_load_temp(&temp);
                    self.cg
                        .emit(Instruction::GetAttr(item.name.clone()));
                    let bind = item.alias.clone().unwrap_or_else(|| item.name.clone());
                    self.emit_bind_name(&bind);
                }
            }
            Stmt::Throw(e) => {
                self.gen_expr(e)?;
                self.cg.emit(Instruction::Throw);
            }
            Stmt::Try {
                body,
                catches,
                else_block,
            } => {
                self.gen_try(body, catches, else_block.as_ref(), top_level)?;
            }
            Stmt::Match {
                subject,
                cases,
                else_block,
            } => {
                self.gen_match(subject, cases, else_block.as_ref(), top_level)?;
            }
            Stmt::Del(target) => match target {
                DelTarget::Name(name) => {
                    self.cg.emit(Instruction::DelName(name.clone()));
                }
                DelTarget::Index { object, index } => {
                    self.gen_expr(object)?;
                    self.gen_expr(index)?;
                    self.cg.emit(Instruction::DelIndex);
                }
                DelTarget::Member { object, field } => {
                    self.gen_expr(object)?;
                    self.cg.emit(Instruction::DelAttr(field.clone()));
                }
            },
            Stmt::With {
                context,
                alias,
                body,
            } => {
                self.gen_with(context, alias.as_deref(), body, top_level)?;
            }
            Stmt::Comment { .. } => {
                // 注释不生成任何指令。
            }
        }
        Ok(())
    }

    fn gen_apply_decorators(&mut self, decorators: &[Expr]) -> Result<()> {
        for deco in decorators.iter().rev() {
            self.gen_expr(deco)?;
            self.cg.emit(Instruction::Call { argc: 1 });
        }
        Ok(())
    }

    fn gen_with(
        &mut self,
        context: &Expr,
        alias: Option<&str>,
        body: &Block,
        as_value: bool,
    ) -> Result<()> {
        let ctx = self.cg.fresh_temp("__with_ctx");
        let exc = self.cg.fresh_temp("__with_exc");
        self.gen_expr(context)?;
        self.emit_store_temp(&ctx);
        self.emit_load_temp(&ctx);
        self.cg.emit(Instruction::GetAttr("__enter__".into()));
        self.cg.emit(Instruction::Call { argc: 0 });
        if let Some(name) = alias {
            self.emit_bind_name(name);
        } else {
            self.cg.emit(Instruction::Pop);
        }

        let catch_dispatch = self.cg.fresh_label();
        let success_cleanup = self.cg.fresh_label();
        let try_end = self.cg.fresh_label();
        // else_label → 成功清理；EndTry 会 PopTry 后再跳过来。
        self.cg.emit(Instruction::EnterTry {
            catch_label: catch_dispatch,
            else_label: success_cleanup,
            end_label: try_end,
        });
        self.handler_stack.push(OpenHandler::With {
            ctx: ctx.clone(),
        });
        self.gen_block(body, as_value)?;
        self.cg.emit(Instruction::EndTry);
        self.handler_stack.pop();

        self.cg.mark_label(success_cleanup);
        self.emit_load_temp(&ctx);
        self.cg.emit(Instruction::Push(Value::None));
        self.cg
            .emit(Instruction::Load("__with_exit__".into()));
        self.cg.emit(Instruction::Call { argc: 2 });
        self.cg.emit(Instruction::Pop);
        self.cg.emit(Instruction::Goto(try_end));

        self.cg.mark_label(catch_dispatch);
        self.cg.emit(Instruction::PushExc);
        self.emit_store_temp(&exc);
        // 先弹出本层 try，再调 __exit__ / 重抛，避免重抛再次落入同一 catch 死循环。
        self.cg.emit(Instruction::PopTry);
        self.emit_load_temp(&ctx);
        self.emit_load_temp(&exc);
        self.cg
            .emit(Instruction::Load("__with_exit__".into()));
        self.cg.emit(Instruction::Call { argc: 2 });
        self.cg.emit(Instruction::Pop);
        self.cg.emit(Instruction::Load(exc));
        self.cg.emit(Instruction::Throw);

        self.cg.mark_label(try_end);
        Ok(())
    }

    fn gen_if(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        elifs: &[(Expr, Block)],
        else_block: &Option<Block>,
        as_value: bool,
    ) -> Result<()> {
        let end = self.cg.fresh_label();
        let mut next = self.cg.fresh_label();
        self.gen_expr(cond)?;
        self.cg.emit(Instruction::GotoIfNot(next));
        self.gen_block(then_block, as_value)?;
        self.cg.emit(Instruction::Goto(end));
        for (elif_cond, elif_body) in elifs {
            self.cg.mark_label(next);
            next = self.cg.fresh_label();
            self.gen_expr(elif_cond)?;
            self.cg.emit(Instruction::GotoIfNot(next));
            self.gen_block(elif_body, as_value)?;
            self.cg.emit(Instruction::Goto(end));
        }
        self.cg.mark_label(next);
        if let Some(else_b) = else_block {
            self.gen_block(else_b, as_value)?;
        } else if as_value {
            self.cg.emit(Instruction::Push(Value::None));
        }
        self.cg.mark_label(end);
        Ok(())
    }

    fn gen_try(
        &mut self,
        body: &Block,
        catches: &[CatchClause],
        else_block: Option<&Block>,
        as_value: bool,
    ) -> Result<()> {
        let catch_dispatch = self.cg.fresh_label();
        let else_label = if else_block.is_some() {
            self.cg.fresh_label()
        } else {
            0
        };
        let try_end = self.cg.fresh_label();

        self.cg.emit(Instruction::EnterTry {
            catch_label: catch_dispatch,
            else_label,
            end_label: try_end,
        });
        self.handler_stack.push(OpenHandler::Try);
        self.gen_block(body, false)?;
        self.cg.emit(Instruction::EndTry);
        // EndTry 已在运行时弹帧；catch 会在 body 前 PopTry。编译期栈在此收起。
        self.handler_stack.pop();

        self.cg.mark_label(catch_dispatch);

        let mut body_labels = Vec::new();
        let mut first_wildcard: Option<usize> = None;

        for clause in catches {
            let body_lbl = self.cg.fresh_label();
            body_labels.push(body_lbl);
            match &clause.pattern {
                CatchPattern::Wildcard => {
                    if first_wildcard.is_none() {
                        first_wildcard = Some(body_lbl);
                    }
                }
                CatchPattern::Bind { type_name: Some(t), .. } => {
                    self.cg.emit(Instruction::ExcMatch(t.clone()));
                    self.cg.emit(Instruction::GotoIf(body_lbl));
                }
                CatchPattern::Bind { type_name: None, .. } => {
                    if first_wildcard.is_none() {
                        first_wildcard = Some(body_lbl);
                    }
                }
            }
        }

        if let Some(wc) = first_wildcard {
            self.cg.emit(Instruction::Goto(wc));
        } else {
            self.cg.emit(Instruction::Rethrow);
        }

        for (clause, body_lbl) in catches.iter().zip(body_labels.iter()) {
            self.cg.mark_label(*body_lbl);
            if let CatchPattern::Bind { name, .. } = &clause.pattern {
                self.cg.emit(Instruction::PushExc);
                self.emit_bind_name(name);
            }
            // 先弹出本层 try，再跑 catch 体，避免 catch 内再抛落入同一 handler 死循环。
            self.cg.emit(Instruction::PopTry);
            self.gen_block(&clause.body, as_value)?;
            self.cg.emit(Instruction::Goto(try_end));
        }

        if else_label != 0 {
            self.cg.mark_label(else_label);
            // EndTry 已 PopTry，else 不再被本层 handler 覆盖。
            if let Some(else_b) = else_block {
                self.gen_block(else_b, as_value)?;
            }
        }

        self.cg.mark_label(try_end);
        Ok(())
    }

    fn gen_match(
        &mut self,
        subject: &Expr,
        cases: &[MatchCase],
        else_block: Option<&Block>,
        as_value: bool,
    ) -> Result<()> {
        let temp = self.cg.fresh_temp("__match");
        self.gen_expr(subject)?;
        self.emit_store_temp(&temp);

        let end = self.cg.fresh_label();
        let mut next_case = self.cg.fresh_label();

        for case in cases {
            self.cg.mark_label(next_case);
            next_case = self.cg.fresh_label();
            self.gen_match_case(&temp, &case.pattern, &case.body, next_case, end, as_value)?;
        }
        self.cg.mark_label(next_case);

        if let Some(else_b) = else_block {
            self.gen_block(else_b, as_value)?;
        } else if as_value {
            self.cg.emit(Instruction::Push(Value::None));
        }
        self.cg.mark_label(end);
        Ok(())
    }

    fn gen_match_case(
        &mut self,
        temp: &str,
        pattern: &Pattern,
        body: &Block,
        fail_label: usize,
        end_label: usize,
        as_value: bool,
    ) -> Result<()> {
        if let Pattern::Or(alts) = pattern {
            let mut body_labels = Vec::new();
            for alt in alts {
                let body_lbl = self.cg.fresh_label();
                body_labels.push(body_lbl);
                let alt_fail = self.cg.fresh_label();
                self.gen_match_pattern_test(temp, alt, &[], alt_fail)?;
                self.cg.emit(Instruction::Goto(body_lbl));
                self.cg.mark_label(alt_fail);
            }
            self.cg.emit(Instruction::Goto(fail_label));
            for (alt, body_lbl) in alts.iter().zip(body_labels.iter()) {
                self.cg.mark_label(*body_lbl);
                self.gen_match_pattern_bindings(temp, alt, &[])?;
                self.gen_block(body, as_value)?;
                self.cg.emit(Instruction::Goto(end_label));
            }
            return Ok(());
        }

        let body_lbl = self.cg.fresh_label();
        self.gen_match_pattern_test(temp, pattern, &[], fail_label)?;
        self.cg.emit(Instruction::Goto(body_lbl));
        self.cg.mark_label(body_lbl);
        self.gen_match_pattern_bindings(temp, pattern, &[])?;
        self.gen_block(body, as_value)?;
        self.cg.emit(Instruction::Goto(end_label));
        Ok(())
    }

    fn gen_match_pattern_test(
        &mut self,
        temp: &str,
        pattern: &Pattern,
        path: &[usize],
        fail_label: usize,
    ) -> Result<()> {
        match pattern {
            Pattern::Value(expr) => {
                self.emit_load_match_at(temp, path)?;
                self.gen_expr(expr)?;
                self.cg.emit(Instruction::MatchEq);
                self.cg.emit(Instruction::GotoIfNot(fail_label));
            }
            Pattern::Bind(_) => {}
            Pattern::List(elems) => {
                self.emit_load_match_at(temp, path)?;
                self.cg.emit(Instruction::IsList);
                self.cg.emit(Instruction::GotoIfNot(fail_label));
                self.emit_load_match_at(temp, path)?;
                self.cg.emit(Instruction::ListLen);
                self.cg
                    .emit(Instruction::PushSmall(elems.len() as i64));
                self.cg.emit(Instruction::Eq);
                self.cg.emit(Instruction::GotoIfNot(fail_label));
                for (i, elem) in elems.iter().enumerate() {
                    let mut child_path = path.to_vec();
                    child_path.push(i);
                    self.gen_match_pattern_elem_test(temp, elem, &child_path, fail_label)?;
                }
            }
            Pattern::Struct { type_name, .. } => {
                self.emit_load_match_at(temp, path)?;
                self.cg.emit(Instruction::IsInstance(type_name.clone()));
                self.cg.emit(Instruction::GotoIfNot(fail_label));
            }
            Pattern::Or(_) => {
                return Err(RuntimeError::msg("or pattern in test"));
            }
            Pattern::Call { type_name, args } => {
                self.gen_match_call_pattern_test(temp, path, type_name, args, fail_label)?;
            }
        }
        Ok(())
    }

    fn gen_match_call_pattern_test(
        &mut self,
        temp: &str,
        path: &[usize],
        type_name: &str,
        args: &[Pattern],
        fail_label: usize,
    ) -> Result<()> {
        if self.program.variant_defs.contains_key(type_name) {
            self.emit_load_match_at(temp, path)?;
            self.cg
                .emit(Instruction::Push(Value::type_ref(type_name.to_string())));
            self.cg.emit(Instruction::Load("__variant_is__".into()));
            self.cg.emit(Instruction::Call { argc: 2 });
            self.cg.emit(Instruction::GotoIfNot(fail_label));
            if let Some(inner) = args.first() {
                let payload_temp = self.cg.fresh_temp("__variant_payload");
                self.emit_load_match_at(temp, path)?;
                self.cg.emit(Instruction::Load("__variant_payload__".into()));
                self.cg.emit(Instruction::Call { argc: 1 });
                self.emit_store_temp(&payload_temp);
                self.gen_match_pattern_test(&payload_temp, inner, &[], fail_label)?;
            }
            return Ok(());
        }
        self.emit_load_match_at(temp, path)?;
        self.cg
            .emit(Instruction::IsInstance(type_name.to_string()));
        self.cg.emit(Instruction::GotoIfNot(fail_label));
        if let Some(sdef) = self.program.struct_defs.get(type_name).cloned() {
            let field_names = sdef.fields.clone();
            if args.len() != field_names.len() {
                return Err(RuntimeError::value_err("case pattern arity mismatch"));
            }
            for (i, arg) in args.iter().enumerate() {
                match arg {
                    Pattern::Bind(_) => {}
                    other => {
                        let fname = &field_names[i];
                        let field_temp = self.cg.fresh_temp("__match_field");
                        self.emit_load_match_at(temp, path)?;
                        self.cg.emit(Instruction::GetAttr(fname.clone()));
                        self.emit_store_temp(&field_temp);
                        self.gen_match_pattern_test(&field_temp, other, &[], fail_label)?;
                    }
                }
            }
        } else if let Some(inner) = args.first() {
            self.gen_match_pattern_test(temp, inner, path, fail_label)?;
        }
        Ok(())
    }

    fn gen_match_pattern_elem_test(
        &mut self,
        temp: &str,
        elem: &PatternElem,
        path: &[usize],
        fail_label: usize,
    ) -> Result<()> {
        match elem {
            PatternElem::Bind(_) => Ok(()),
            PatternElem::Nested(pat) => self.gen_match_pattern_test(temp, pat, path, fail_label),
            PatternElem::Value(expr) => {
                self.emit_load_match_at(temp, path)?;
                self.gen_expr(expr)?;
                self.cg.emit(Instruction::MatchEq);
                self.cg.emit(Instruction::GotoIfNot(fail_label));
                Ok(())
            }
        }
    }

    fn gen_match_pattern_bindings(
        &mut self,
        temp: &str,
        pattern: &Pattern,
        path: &[usize],
    ) -> Result<()> {
        match pattern {
            Pattern::Bind(name) => {
                self.emit_load_match_at(temp, path)?;
                self.emit_bind_name(name);
            }
            Pattern::List(elems) => {
                for (i, elem) in elems.iter().enumerate() {
                    let mut child_path = path.to_vec();
                    child_path.push(i);
                    self.gen_match_pattern_elem_bindings(temp, elem, &child_path)?;
                }
            }
            Pattern::Struct { fields, .. } => {
                for field in fields {
                    self.emit_load_match_at(temp, path)?;
                    self.cg.emit(Instruction::GetAttr(field.clone()));
                    self.emit_bind_name(field);
                }
            }
            Pattern::Or(_alts) => {
                return Err(RuntimeError::msg("or pattern in bindings"));
            }
            Pattern::Call { type_name, args } => {
                if self.program.variant_defs.contains_key(type_name) {
                    if let Some(inner) = args.first() {
                        let payload_temp = self.cg.fresh_temp("__variant_payload");
                        self.emit_load_match_at(temp, path)?;
                        self.cg.emit(Instruction::Load("__variant_payload__".into()));
                        self.cg.emit(Instruction::Call { argc: 1 });
                        self.emit_store_temp(&payload_temp);
                        self.gen_match_pattern_bindings(&payload_temp, inner, &[])?;
                    }
                } else if let Some(sdef) = self.program.struct_defs.get(type_name).cloned() {
                    let field_names = sdef.fields.clone();
                    if args.len() != field_names.len() {
                        return Err(RuntimeError::value_err("case pattern arity mismatch"));
                    }
                    for (i, arg) in args.iter().enumerate() {
                        let fname = &field_names[i];
                        match arg {
                            Pattern::Bind(bind_name) => {
                                self.emit_load_match_at(temp, path)?;
                                self.cg.emit(Instruction::GetAttr(fname.clone()));
                                self.emit_bind_name(bind_name);
                            }
                            other => {
                                let field_temp = self.cg.fresh_temp("__match_field");
                                self.emit_load_match_at(temp, path)?;
                                self.cg.emit(Instruction::GetAttr(fname.clone()));
                                self.emit_store_temp(&field_temp);
                                self.gen_match_pattern_bindings(&field_temp, other, &[])?;
                            }
                        }
                    }
                } else if let Some(inner) = args.first() {
                    self.gen_match_pattern_bindings(temp, inner, path)?;
                }
            }
            Pattern::Value(_) => {}
        }
        Ok(())
    }

    fn gen_enum_decl(
        &mut self,
        name: &str,
        members: &[EnumMemberDecl],
        methods: &[EnumMethodDecl],
        visibility: Visibility,
        top_level: bool,
    ) -> Result<()> {
        let mut generate_func = None;

        for method in methods {
            crate::enum_variant::validate_enum_method(method)?;
            let full_name = format!("{name}.{}", method.name);
            let func = self.compile_function(
                &full_name,
                &method.params,
                &method.body,
                CompileFnExtras {
                    return_type: None,
                    return_strong: false,
                    return_wrapper: None,
                    captured_names: HashSet::new(),
                    is_generator: false,
                },
            )?;
            if method.name == "__generate__" {
                generate_func = Some(Arc::new(func));
            } else {
                self.program
                    .functions
                    .insert(full_name, Arc::new(func));
            }
        }

        if let Some(gen_func) = generate_func {
            self.emit_enum_all_dict(members)?;
            self.cg
                .emit(Instruction::Push(Value::Function(gen_func)));
            self.cg.emit(Instruction::Call { argc: 1 });
            let values_temp = self.cg.fresh_temp("__enum_values");
            self.emit_store_temp(&values_temp);
            self.cg
                .emit(Instruction::Push(Value::type_ref(name.to_string())));
            for m in members {
                self.cg
                    .emit(Instruction::Push(Value::Text(m.name.clone())));
            }
            self.cg.emit(Instruction::VecNew(members.len()));
            self.emit_load_temp(&values_temp);
            self.cg.emit(Instruction::Load("__finalize_enum__".into()));
            self.cg.emit(Instruction::Call { argc: 3 });
        } else {
            let member_infos = crate::enum_variant::default_enum_values(members)?;
            let def = crate::enum_variant::build_enum_def(name, member_infos);
            self.program.enum_defs.insert(name.to_string(), def.clone());
            for (name, func) in crate::enum_variant::builtin_enum_method_entries(name, &def) {
                self.program.functions.insert(name, func);
            }
        }

        self.cg.emit(Instruction::Push(Value::type_ref(name.to_string())));
        self.cg.emit(Instruction::NewVar {
            name: name.to_string(),
            is_const: false,
        });
        self.emit_store_name(name);
        self.maybe_register_export(visibility, name, top_level);
        Ok(())
    }

    fn emit_enum_all_dict(&mut self, members: &[EnumMemberDecl]) -> Result<()> {
        for m in members.iter().rev() {
            self.cg
                .emit(Instruction::Push(Value::Text(m.name.clone())));
            if let Some(expr) = &m.value {
                let num = crate::enum_variant::eval_const_num(expr)?;
                self.cg.emit(Instruction::Push(Value::Num(num)));
            } else {
                self.cg.emit(Instruction::Push(Value::None));
            }
        }
        self.cg.emit(Instruction::DictNew(members.len()));
        Ok(())
    }

    fn emit_load_module_ref(&mut self, module: &ModuleRef) -> Result<()> {
        match module {
            ModuleRef::Qualified(parts) => {
                self.cg.emit(Instruction::FindMod(parts.join(".")));
            }
            ModuleRef::FilePath { path, attrs } => {
                self.cg
                    .emit(Instruction::FindMod(format!("@str:{path}")));
                for attr in attrs {
                    self.cg.emit(Instruction::GetAttr(attr.clone()));
                }
            }
        }
        Ok(())
    }

    fn gen_fstring(&mut self, parts: &[FStringPart]) -> Result<()> {
        if parts.is_empty() {
            self.cg.emit(Instruction::Push(Value::Text(String::new())));
            return Ok(());
        }
        let mut first = true;
        for part in parts {
            match part {
                FStringPart::Text(s) => {
                    self.cg.emit(Instruction::Push(Value::Text(s.clone())));
                }
                FStringPart::Expr(expr) => {
                    self.gen_expr(expr)?;
                    self.cg.emit(Instruction::Load("str".into()));
                    self.cg.emit(Instruction::Call { argc: 1 });
                }
            }
            if first {
                first = false;
            } else {
                self.cg.emit(Instruction::Add);
            }
        }
        Ok(())
    }

    fn gen_variant_decl(
        &mut self,
        name: &str,
        type_params: &[(String, Option<TypeExpr>)],
        cases: &[VariantCaseDecl],
        visibility: Visibility,
        top_level: bool,
    ) -> Result<()> {
        let (vdef, struct_defs) =
            crate::enum_variant::build_variant_def(name, type_params.to_vec(), cases);
        self.program.variant_defs.insert(name.to_string(), vdef);
        for (sname, sdef) in struct_defs {
            self.program.struct_defs.insert(sname, sdef);
        }
        self.cg.emit(Instruction::Push(Value::type_ref(name.to_string())));
        self.cg.emit(Instruction::NewVar {
            name: name.to_string(),
            is_const: false,
        });
        self.emit_store_name(name);
        self.maybe_register_export(visibility, name, top_level);
        Ok(())
    }

    fn gen_match_pattern_elem_bindings(
        &mut self,
        temp: &str,
        elem: &PatternElem,
        path: &[usize],
    ) -> Result<()> {
        match elem {
            PatternElem::Bind(name) => {
                self.emit_load_match_at(temp, path)?;
                self.emit_bind_name(name);
                Ok(())
            }
            PatternElem::Nested(pat) => self.gen_match_pattern_bindings(temp, pat, path),
            PatternElem::Value(_) => Ok(()),
        }
    }

    fn emit_load_match_at(&mut self, temp: &str, path: &[usize]) -> Result<()> {
        self.emit_load_temp(temp);
        for &idx in path {
            self.cg
                .emit(Instruction::PushSmall(idx as i64));
            self.cg.emit(Instruction::Index);
        }
        Ok(())
    }

    fn gen_block(&mut self, block: &Block, keep_last_value: bool) -> Result<()> {
        self.push_type_scope();
        let last_value_idx = block.iter().rposition(|s| !matches!(s.stmt, Stmt::Comment { .. }));
        if keep_last_value
            && (last_value_idx.is_none()
                || !Self::stmt_yields_value(&block[last_value_idx.expect("checked")].stmt))
        {
            for located in block {
                self.gen_stmt(located, false)?;
            }
            self.cg.emit(Instruction::Push(Value::None));
        } else {
            for (i, located) in block.iter().enumerate() {
                let keep = keep_last_value && Some(i) == last_value_idx;
                self.gen_stmt(located, keep)?;
            }
        }
        self.pop_type_scope();
        Ok(())
    }

    /// 作为「块的值」时，这些语句能把结果留在操作数栈顶。
    fn stmt_yields_value(stmt: &Stmt) -> bool {
        matches!(
            stmt,
            Stmt::Expr(_)
                | Stmt::If { .. }
                | Stmt::Match { .. }
                | Stmt::Try { .. }
                | Stmt::Block(_)
                | Stmt::With { .. }
        )
    }

    fn fresh_subprogram(&self) -> CompiledProgram {
        let mut program = CompiledProgram::new();
        program.struct_defs = self.program.struct_defs.clone();
        program.enum_defs = self.program.enum_defs.clone();
        program.variant_defs = self.program.variant_defs.clone();
        program.protocols = self.program.protocols.clone();
        program.generic_functions = self.program.generic_functions.clone();
        program
    }

    fn specialization_key(name: &str, type_args: &[TypeExpr]) -> String {
        format!(
            "{name}${}",
            type_args
                .iter()
                .map(crate::types::type_expr_display)
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    fn instantiate_generic(
        &mut self,
        name: &str,
        type_args: Vec<TypeExpr>,
    ) -> Result<Arc<FunctionObject>> {
        let template = self
            .program
            .generic_functions
            .get(name)
            .cloned()
            .ok_or_else(|| RuntimeError::msg(format!("unknown generic function `{name}`")))?;
        let ctx = TypeCheckContext::from_program(&self.program);
        let func = Self::specialize_generic_template(&template, type_args, &ctx, &mut self.program.functions)?;
        Ok(func)
    }

    /// 运行时 / REPL：由模板与类型实参生成单态函数（写入 `cache`）。
    pub fn specialize_generic_template(
        template: &GenericFunctionTemplate,
        type_args: Vec<TypeExpr>,
        ctx: &TypeCheckContext,
        cache: &mut HashMap<String, Arc<FunctionObject>>,
    ) -> Result<Arc<FunctionObject>> {
        if type_args.len() != template.type_params.len() {
            return Err(RuntimeError::type_err(format!(
                "generic function `{}` expects {} type argument(s), got {}",
                template.name,
                template.type_params.len(),
                type_args.len()
            )));
        }
        let key = Self::specialization_key(&template.name, &type_args);
        if let Some(existing) = cache.get(&key) {
            return Ok(existing.clone());
        }
        for (i, (_tp_name, bound)) in template.type_params.iter().enumerate() {
            if let Some(b) = bound {
                protocol::check_type_bound_ctx(ctx, &type_args[i], b)?;
            }
        }
        let subs = monomorph::type_substitution_map(&template.type_params, &type_args);
        let type_names = monomorph::type_name_map(&template.type_params, &type_args);
        let params: Vec<FuncParam> = template
            .params
            .iter()
            .map(|p| monomorph::substitute_func_param(p, &subs, &type_names))
            .collect();
        let body = monomorph::substitute_block(&template.body, &type_names);
        let return_type = template
            .return_type
            .as_ref()
            .map(|t| monomorph::substitute_type_expr(t, &subs));
        let return_wrapper = template
            .return_wrapper
            .as_ref()
            .map(|e| monomorph::substitute_expr(e, &type_names));
        let mut gen = Generator::new();
        gen.program.struct_defs = ctx.struct_defs.clone();
        gen.program.protocols = ctx.protocols.clone();
        gen.program.functions = cache.clone();
        let func = gen.compile_function(
            &key,
            &params,
            &body,
            CompileFnExtras {
                return_type: return_type.as_ref(),
                return_strong: template.return_strong,
                return_wrapper,
                captured_names: HashSet::new(),
                is_generator: template.is_generator,
            },
        )?;
        let mut func = func;
        if func.source.is_none() {
            func.source = template.source.clone();
            func.source_file = template.source_file.clone();
        }
        let func = Arc::new(func);
        cache.insert(key, func.clone());
        Ok(func)
    }

    fn try_emit_generic_call(&mut self, callee: &Expr, args: &[CallArg]) -> Result<bool> {
        let resolved = match &callee.kind {
            ExprKind::Index { object, index } => {
                let ExprKind::Var(name) = &object.kind else {
                    return Ok(false);
                };
                if !self.program.generic_functions.contains_key(name) {
                    return Ok(false);
                }
                let type_args = monomorph::type_args_from_index_expr(index)
                    .map_err(RuntimeError::msg)?;
                (name.clone(), type_args)
            }
            ExprKind::Var(name) => {
                if !self.program.generic_functions.contains_key(name) {
                    return Ok(false);
                }
                let template = self.program.generic_functions[name].clone();
                let type_args = self.infer_generic_type_args(&template, args)?;
                (name.clone(), type_args)
            }
            _ => return Ok(false),
        };
        let func = self.instantiate_generic(&resolved.0, resolved.1)?;
        for a in args {
            self.gen_expr(&a.value)?;
        }
        self.cg.emit(Instruction::Push(Value::Function(func)));
        self.cg.emit(Instruction::Call { argc: args.len() });
        Ok(true)
    }

    fn infer_generic_type_args(
        &self,
        template: &GenericFunctionTemplate,
        args: &[CallArg],
    ) -> Result<Vec<TypeExpr>> {
        if template.type_params.len() != 1 {
            return Err(RuntimeError::msg(format!(
                "cannot infer {} type parameter(s) for `{}`; use {}[...](...)",
                template.type_params.len(),
                template.name,
                template.name
            )));
        }
        let arg_expr = &args
            .first()
            .ok_or_else(|| {
                RuntimeError::type_err(format!(
                    "{} expects at least one argument for type inference",
                    template.name
                ))
            })?
            .value;
        let inferred = self
            .infer_type_from_arg(arg_expr)
            .ok_or_else(|| {
                RuntimeError::msg(format!(
                    "cannot infer type parameter for `{}` from argument; try `{}[type](...)`",
                    template.name, template.name
                ))
            })?;
        Ok(vec![inferred])
    }

    fn infer_type_from_arg(&self, expr: &Expr) -> Option<TypeExpr> {
        if let Some(ty) = monomorph::infer_type_from_expr(expr, "x") {
            return Some(ty);
        }
        match &expr.kind {
            ExprKind::Call { callee, .. } => {
                if let ExprKind::Var(name) = &callee.kind {
                    if self.program.struct_defs.contains_key(name) {
                        return Some(TypeExpr::Name(name.clone()));
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn compile_function(
        &mut self,
        name: &str,
        params: &[FuncParam],
        body: &Block,
        extras: CompileFnExtras<'_>,
    ) -> Result<FunctionObject> {
        let CompileFnExtras {
            return_type,
            return_strong,
            return_wrapper,
            captured_names,
            is_generator,
        } = extras;
        let mut local_slots = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            local_slots.insert(p.name.clone(), i);
        }
        // 为闭包捕获预留局部槽，避免 `let`/`BindFast` 覆盖捕获的 Cell。
        let mut captured_ordered: Vec<String> = captured_names.iter().cloned().collect();
        captured_ordered.sort();
        let mut next_local_slot = params.len();
        for name in &captured_ordered {
            if !local_slots.contains_key(name) {
                local_slots.insert(name.clone(), next_local_slot);
                next_local_slot += 1;
            }
        }
        let wrapper_for_func = return_wrapper.clone();
        let mut sub = Generator {
            cg: Codegen::new(),
            program: self.fresh_subprogram(),
            loop_break_labels: Vec::new(),
            loop_continue_labels: Vec::new(),
            loop_handler_depths: Vec::new(),
            loop_owns_stack_counter: Vec::new(),
            handler_stack: Vec::new(),
            type_env: vec![HashMap::new()],
            macro_depth: self.macro_depth,
            match_expr_ends: Vec::new(),
            local_slots: Some(local_slots),
            next_local_slot,
            declared_temps: HashSet::new(),
            global_index: self.global_index.clone(),
            next_global_sym: self.next_global_sym,
            current_func: Some(name.to_string()),
            block_depth: 0,
            captured_names,
            current_return_wrapper: return_wrapper,
            compiling_generator: is_generator,
        };
        for p in params {
            if let (Some(ty), true) = (&p.type_expr, p.type_strong) {
                sub.bind_type(&p.name, ty.clone(), true);
            }
        }
        // 隐式返回：块末尾产生值的语句（表达式 / if / match / …）留在栈上并 Ret；
        // 显式 `return` / `return expr` 仍走 Return；空 `return` → none。
        if body.is_empty() {
            sub.gen_return_expr(None)?;
        } else {
            let last = body.len() - 1;
            for (i, s) in body.iter().enumerate() {
                if i != last {
                    sub.gen_stmt(s, false)?;
                    continue;
                }
                match &s.stmt {
                    Stmt::Return(_) => {
                        sub.gen_stmt(s, false)?;
                    }
                    Stmt::Yield(_) | Stmt::YieldFrom(_) => {
                        sub.gen_stmt(s, false)?;
                        sub.gen_return_expr(None)?;
                    }
                    other if Self::stmt_yields_value(other) => {
                        sub.gen_stmt(s, true)?;
                        sub.gen_return_from_tos()?;
                    }
                    _ => {
                        sub.gen_stmt(s, false)?;
                        sub.gen_return_expr(None)?;
                    }
                }
            }
        }
        sub.cg
            .patch_labels()
            .map_err(RuntimeError::msg)?;
        let mut body = std::mem::take(&mut sub.cg.code);
        let entry_env: Vec<Option<crate::specialize::Tag>> = params
            .iter()
            .map(|p| match (&p.type_expr, p.type_strong) {
                (Some(ty), true) => crate::specialize::tag_from_strong_type(ty),
                _ => None,
            })
            .collect();
        crate::specialize::specialize_with_entry(&mut body, &entry_env);
        let (compacted_body, body_remap) = crate::opcode::compact_bytecode(body);
        let (fused_body, body_remap2) = crate::opcode::peephole_fuse(compacted_body);
        body = fused_body;
        let lm = crate::opcode::compact_parallel(&sub.cg.line_map, &body_remap);
        let func_line_map = crate::opcode::compact_parallel(&lm, &body_remap2);
        let cm = crate::opcode::compact_parallel(&sub.cg.column_map, &body_remap);
        let func_column_map = crate::opcode::compact_parallel(&cm, &body_remap2);
        // 子编译器的 `global_names` 可能含空洞：struct/原始类型经 `Push(type_ref)`
        // 不写入名字表，而 `len` 等仍占后续 `LoadGlobal` 下标 → `["", "len", …]`。
        // 不可对空串调用 `global_slot`（会偷占槽位），也不可把 `next_global_sym`
        // 回退到子编译器值（会与已合并槽冲突），否则随后的函数名
        // `StoreGlobal` 写到错误槽，`RegisterExport` 读到仍为 `none` 的绑定。
        let func_global_names = sub.program.global_names.clone();
        for name in &func_global_names {
            if name.is_empty() {
                continue;
            }
            self.global_slot(name);
        }
        self.next_global_sym = self.next_global_sym.max(sub.next_global_sym);
        let uses_name_map = crate::opcode::function_uses_name_map(&body);
        let track_frames = return_strong || crate::opcode::function_uses_try(&body);
        let flexible_params = params.iter().any(|p| {
            p.default_expr.is_some() || p.is_variadic || p.is_kwvariadic
        });
        let lightweight = !flexible_params
            && crate::opcode::function_lightweight(
                &body,
                uses_name_map,
                track_frames,
                return_strong,
            );
        let hot = crate::hot_code::HotCode::encode(&body);
        let variadic_param_index = params.iter().position(|p| p.is_variadic);
        let kwvariadic_param_index = params.iter().position(|p| p.is_kwvariadic);
        let defaults: Vec<Option<Value>> = params
            .iter()
            .map(|p| {
                p.default_expr
                    .as_ref()
                    .and_then(const_default_value)
            })
            .collect();
        // LoadGlobal 下标相对本函数编译时的名字表，必须与 module_env 一致；
        // 不可改用整脚本 global_names（REPL / 单态化时下标会错位）。
        let module_env = if func_global_names.is_empty() {
            None
        } else {
            Some(Arc::new(crate::opcode::ModuleGlobalEnv {
                global_names: func_global_names,
                globals: crate::shared::SyncCell::new(HashMap::new()),
                finalized: false,
            }))
        };
        Ok(FunctionObject {
            name: name.to_string(),
            params: params.to_vec(),
            body: Arc::new(body),
            hot,
            line_map: Arc::new(func_line_map),
            column_map: Arc::new(func_column_map),
            entry_label: 0,
            fast_locals: sub.next_local_slot,
            is_builtin_body: false,
            variadic_param_index,
            kwvariadic_param_index,
            defaults,
            captured: None,
            return_type: return_type.cloned(),
            return_strong,
            return_wrapper: wrapper_for_func,
            frame_slots: sub.next_local_slot,
            uses_name_map,
            track_frames,
            entry_pc: 0,
            lightweight: lightweight && !is_generator,
            is_generator,
            module_env,
            source: None,
            source_file: "<script>".into(),
        })
    }

    /// 非局部退出前补发打开的 try/with 清理（`down_to..` 段，自内向外）。
    fn emit_handler_exit_cleanups(&mut self, down_to: usize) {
        let handlers: Vec<OpenHandler> = self.handler_stack[down_to..].to_vec();
        for h in handlers.into_iter().rev() {
            match h {
                OpenHandler::Try => {
                    self.cg.emit(Instruction::PopTry);
                }
                OpenHandler::With { ctx } => {
                    self.cg.emit(Instruction::PopTry);
                    self.emit_load_temp(&ctx);
                    self.cg.emit(Instruction::Push(Value::None));
                    self.cg
                        .emit(Instruction::Load("__with_exit__".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                    self.cg.emit(Instruction::Pop);
                }
            }
        }
    }

    /// `return` 前丢掉仍压在栈上的计数循环计数器。
    /// `keep_tos`：栈顶已是返回值，需先挪开再弹计数器。
    fn emit_discard_loop_stack_counters(&mut self, keep_tos: bool) {
        let n = self
            .loop_owns_stack_counter
            .iter()
            .filter(|&&owns| owns)
            .count();
        if n == 0 {
            return;
        }
        if keep_tos {
            let tmp = self.cg.fresh_temp("__ret_keep");
            self.emit_store_temp(&tmp);
            for _ in 0..n {
                self.cg.emit(Instruction::Pop);
            }
            self.emit_load_temp(&tmp);
        } else {
            for _ in 0..n {
                self.cg.emit(Instruction::Pop);
            }
        }
    }

    fn gen_return_expr(&mut self, expr: Option<&Expr>) -> Result<()> {
        if let (Some(e), Some(slots)) = (expr, self.local_slots.as_ref()) {
            if let ExprKind::Var(name) = &e.kind {
            if self.current_return_wrapper.is_none() {
                if let Some(&slot) = slots.get(name.as_str()) {
                    if let Some(end) = self.match_expr_ends.last().copied() {
                        self.emit_handler_exit_cleanups(0);
                        self.emit_discard_loop_stack_counters(false);
                        self.cg.emit(Instruction::Goto(end));
                    } else {
                        self.emit_handler_exit_cleanups(0);
                        self.emit_discard_loop_stack_counters(false);
                        self.cg.emit(Instruction::RetFast(slot));
                    }
                    return Ok(());
                }
            }
            }
        }
        if let Some(e) = expr {
            self.gen_expr(e)?;
        } else {
            self.cg.emit(Instruction::Push(Value::None));
        }
        self.gen_return_from_tos()
    }

    /// 操作数栈顶已是返回值（隐式末尾表达式 / 已求值的 return 表达式）。
    fn gen_return_from_tos(&mut self) -> Result<()> {
        if let Some(wrapper) = self.current_return_wrapper.clone() {
            // 解析期已将包装器中的 `_` 替换为 `__ret_wrapper_val`。
            let slot = self.ensure_local_slot(RET_WRAPPER_VAL);
            self.cg.emit(Instruction::StoreFast(slot));
            self.gen_expr(&wrapper)?;
        }
        if let Some(end) = self.match_expr_ends.last().copied() {
            self.emit_handler_exit_cleanups(0);
            self.emit_discard_loop_stack_counters(true);
            self.cg.emit(Instruction::Goto(end));
        } else {
            self.emit_handler_exit_cleanups(0);
            self.emit_discard_loop_stack_counters(true);
            self.cg.emit(Instruction::Ret);
        }
        Ok(())
    }

    fn compile_macro(
        &mut self,
        name: &str,
        params: &[MacroParam],
        body: &Block,
    ) -> Result<MacroObject> {
        let mut sub = Generator {
            cg: Codegen::new(),
            program: self.fresh_subprogram(),
            loop_break_labels: Vec::new(),
            loop_continue_labels: Vec::new(),
            loop_handler_depths: Vec::new(),
            loop_owns_stack_counter: Vec::new(),
            handler_stack: Vec::new(),
            type_env: vec![HashMap::new()],
            macro_depth: self.macro_depth + 1,
            match_expr_ends: Vec::new(),
            local_slots: None,
            next_local_slot: 0,
            declared_temps: HashSet::new(),
            global_index: self.global_index.clone(),
            next_global_sym: self.next_global_sym,
            current_func: None,
            block_depth: 0,
            captured_names: HashSet::new(),
            current_return_wrapper: None,
            compiling_generator: false,
        };
        for s in body {
            sub.gen_stmt(s, false)?;
        }
        sub.cg.emit(Instruction::Push(Value::None));
        sub.cg.emit(Instruction::Ret);
        sub.cg
            .patch_labels()
            .map_err(RuntimeError::msg)?;

        Ok(MacroObject::new(name, params.to_vec(), sub.cg.code))
    }

    fn gen_macro_callee(&mut self, callee: &Expr) -> Result<()> {
        match &callee.kind {
            ExprKind::Var(name) => {
                self.cg.emit(Instruction::LoadMacro(name.clone()));
            }
            _ => self.gen_expr(callee)?,
        }
        Ok(())
    }

    /// 宏体内将解析期冻结的 AST 实参重新物化：参数名克隆存活绑定，
    /// 嵌套 `{...}` 经 `__ast_macro_call__` 组合，字面量保持冻结。
    fn gen_materialize_frozen_ast_arg(&mut self, node: &runtime_ast::RuntimeAstNode) -> Result<()> {
        use crate::runtime_ast::AstNodeKind;
        match node.kind {
            AstNodeKind::VarRef => {
                self.cg.emit(Instruction::Load(node.text.clone()));
                self.cg
                    .emit(Instruction::Load("__ast_clone__".into()));
                self.cg.emit(Instruction::Call { argc: 1 });
            }
            AstNodeKind::MacroCall => {
                self.cg.emit(Instruction::VecNew(0));
                for call_arg in &node.call_args {
                    self.gen_materialize_frozen_ast_arg(&call_arg.value)?;
                    self.cg.emit(Instruction::ListAppend);
                }
                let callee = node.slot_a.as_ref().ok_or_else(|| {
                    RuntimeError::msg("macro call AST missing callee")
                })?;
                self.cg
                    .emit(Instruction::Push(Value::RuntimeAst(Arc::new((**callee).clone()))));
                self.cg
                    .emit(Instruction::Load("__ast_macro_call__".into()));
                self.cg.emit(Instruction::Call { argc: 2 });
            }
            _ => {
                self.cg.emit(Instruction::Push(Value::RuntimeAst(Arc::new(
                    node.clone(),
                ))));
            }
        }
        Ok(())
    }

    /// 压入一个宏调用实参。调用点为解析期冻结 AST；宏体内物化以使宏参数与嵌套 `{...}` 正确解析。
    fn gen_push_macro_call_arg(&mut self, arg: &MacroCallArg) -> Result<()> {
        if arg.is_splat {
            if arg.node.kind != crate::runtime_ast::AstNodeKind::VarRef {
                return Err(RuntimeError::type_err(
                    "macro splat argument must be a simple identifier",
                ));
            }
            self.cg.emit(Instruction::Load(arg.node.text.clone()));
            self.cg
                .emit(Instruction::Load("__ast_clone__".into()));
            self.cg.emit(Instruction::Call { argc: 1 });
        } else if self.macro_depth > 0 {
            self.gen_materialize_frozen_ast_arg(&arg.node)?;
        } else {
            self.cg
                .emit(Instruction::Push(Value::RuntimeAst(arg.node.clone())));
        }
        Ok(())
    }

    /// 压入表达式的编译期冻结 AST（宏被调名、quote 体等）。
    fn gen_frozen_ast_expr(&mut self, expr: &Expr) -> Result<()> {
        let ast = runtime_ast::ast_from_expr(expr);
        self.cg
            .emit(Instruction::Push(Value::RuntimeAst(Arc::new(ast))));
        Ok(())
    }

    /// 宏体内，宏调用实参中的裸标识符在运行时指向宏参数。
    fn gen_materialized_ast_expr(&mut self, expr: &Expr) -> Result<()> {
        if self.macro_depth > 0 {
            if let ExprKind::Var(name) = &expr.kind {
                self.cg.emit(Instruction::Load(name.clone()));
                self.cg.emit(Instruction::Load("__ast_clone__".into()));
                self.cg.emit(Instruction::Call { argc: 1 });
                return Ok(());
            }
        }
        self.gen_frozen_ast_expr(expr)
    }

    fn gen_expr(&mut self, expr: &Expr) -> Result<()> {
        self.cg.set_loc(expr.loc.line, expr.loc.column);
        match &expr.kind {
            ExprKind::Number(n) => {
                if let Ok(sized) = crate::sized::SizedNum::from_literal(n) {
                    self.cg
                        .emit(Instruction::Push(Value::Sized(sized)));
                } else {
                    let num = Num::from_literal(n)?;
                    if let Num::Small(v) = num {
                        self.cg.emit(Instruction::PushSmall(v));
                    } else {
                        self.cg.emit(Instruction::Push(Value::Num(num)));
                    }
                }
            }
            ExprKind::String(s) => {
                self.cg.emit(Instruction::Push(Value::Text(s.clone())));
            }
            ExprKind::FString(parts) => {
                self.gen_fstring(parts)?;
            }
            ExprKind::Bool(b) => {
                self.cg.emit(Instruction::Push(Value::Bool(*b)));
            }
            ExprKind::None => {
                self.cg.emit(Instruction::Push(Value::None));
            }
            ExprKind::Var(name) => {
                self.emit_load_name(name);
            }
            ExprKind::Placeholder => {
                return Err(RuntimeError::msg(
                    "internal error: unresolved '_' (expected fill at parse time)",
                ));
            }
            ExprKind::Unary { op, operand } => {
                self.gen_expr(operand)?;
                match op {
                    UnaryOp::Neg => {
                        self.cg.emit(Instruction::Neg);
                    }
                    UnaryOp::Not => {
                        self.cg.emit(Instruction::Not);
                    }
                    UnaryOp::TruthyNot => {
                        self.cg.emit(Instruction::TruthyNot);
                    }
                    UnaryOp::Invert => {
                        self.cg.emit(Instruction::Invert);
                    }
                }
            }
            ExprKind::Binary { op, left, right } => {
                match op {
                    BinaryOp::And => {
                        let end = self.cg.fresh_label();
                        let rest = self.cg.fresh_label();
                        let temp = self.cg.fresh_temp("__sc_and");
                        self.gen_expr(left)?;
                        self.emit_store_temp(&temp);
                        self.emit_load_temp(&temp);
                        self.cg.emit(Instruction::GotoIfNot(end));
                        self.gen_expr(right)?;
                        self.cg.emit(Instruction::Goto(rest));
                        self.cg.mark_label(end);
                        self.emit_load_temp(&temp);
                        self.cg.mark_label(rest);
                    }
                    BinaryOp::Or => {
                        let use_left = self.cg.fresh_label();
                        let end = self.cg.fresh_label();
                        let temp = self.cg.fresh_temp("__sc_or");
                        self.gen_expr(left)?;
                        self.emit_store_temp(&temp);
                        self.emit_load_temp(&temp);
                        self.cg.emit(Instruction::GotoIf(use_left));
                        self.gen_expr(right)?;
                        self.cg.emit(Instruction::Goto(end));
                        self.cg.mark_label(use_left);
                        self.emit_load_temp(&temp);
                        self.cg.mark_label(end);
                    }
                    _ => {
                        self.gen_expr(left)?;
                        self.gen_expr(right)?;
                        let instr = match op {
                            BinaryOp::Add => Instruction::Add,
                            BinaryOp::Sub => Instruction::Sub,
                            BinaryOp::Mul => Instruction::Mul,
                            BinaryOp::Div => Instruction::Div,
                            BinaryOp::Mod => Instruction::Mod,
                            BinaryOp::Pow => Instruction::Pow,
                            BinaryOp::BitAnd => Instruction::BitAnd,
                            BinaryOp::BitOr => Instruction::BitOr,
                            BinaryOp::BitXor => Instruction::BitXor,
                            BinaryOp::LShift => Instruction::LShift,
                            BinaryOp::RShift => Instruction::RShift,
                            BinaryOp::Eq => Instruction::Eq,
                            BinaryOp::Ne => Instruction::Ne,
                            BinaryOp::Lt => Instruction::Lt,
                            BinaryOp::Le => Instruction::Le,
                            BinaryOp::Gt => Instruction::Gt,
                            BinaryOp::Ge => Instruction::Ge,
                            BinaryOp::In => Instruction::In,
                            BinaryOp::Is => Instruction::Is,
                            BinaryOp::IsNot => Instruction::IsNot,
                            // And/Or 应在短路 lowering 阶段被消除；到达此处属内部错误。
                            BinaryOp::And | BinaryOp::Or => {
                                return Err(RuntimeError::msg(
                                    "internal: And/Or must be lowered to short-circuit before codegen (theoretically unreachable)",
                                ));
                            }
                        };
                        self.cg.emit(instr);
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                let has_named = args.iter().any(|a| a.name.is_some());
                let has_kwsplat = args.iter().any(|a| a.is_kwsplat);
                let has_splat = args.iter().any(|a| a.is_splat);
                if has_named || has_kwsplat {
                    self.gen_call_args_and_kwargs(args)?;
                    self.gen_expr(callee)?;
                    self.cg.emit(Instruction::CallEx);
                } else if has_splat {
                    self.cg.emit(Instruction::VecNew(0));
                    for a in args {
                        if a.is_splat {
                            self.gen_expr(&a.value)?;
                            self.cg.emit(Instruction::ListExtend);
                        } else {
                            self.gen_expr(&a.value)?;
                            self.cg.emit(Instruction::ListAppend);
                        }
                    }
                    self.gen_expr(callee)?;
                    self.cg.emit(Instruction::CallList);
                } else if self.try_emit_generic_call(callee, args)? {
                } else if self.is_self_call(callee) && !self.current_func_has_flexible_params() {
                    for a in args {
                        self.gen_expr(&a.value)?;
                    }
                    self.cg
                        .emit(Instruction::CallSelf { argc: args.len() });
                } else {
                    for a in args {
                        self.gen_expr(&a.value)?;
                    }
                    self.gen_expr(callee)?;
                    self.cg.emit(Instruction::Call { argc: args.len() });
                }
            }
            ExprKind::Member { object, field } => {
                self.gen_expr(object)?;
                self.cg.emit(Instruction::GetAttr(field.clone()));
            }
            ExprKind::Index { object, index } => {
                if let ExprKind::Var(name) = &object.kind {
                    if self.program.generic_functions.contains_key(name) {
                        let type_args = monomorph::type_args_from_index_expr(index)
                            .map_err(RuntimeError::msg)?;
                        let func = self.instantiate_generic(name, type_args)?;
                        self.cg.emit(Instruction::Push(Value::Function(func)));
                        return Ok(());
                    }
                }
                self.gen_expr(object)?;
                if self.expr_is_generic_type_formable(object) {
                    self.gen_type_index_operand(index)?;
                } else {
                    self.gen_expr(index)?;
                }
                self.cg.emit(Instruction::Index);
            }
            ExprKind::List(elems) => {
                for e in elems {
                    self.gen_expr(e)?;
                }
                self.cg.emit(Instruction::VecNew(elems.len()));
            }
            ExprKind::ListComp { elem, items, guards } => {
                self.gen_list_comp(elem, items, guards)?;
            }
            ExprKind::SetComp { elem, items, guards } => {
                self.gen_set_comp(elem, items, guards)?;
            }
            ExprKind::DictComp {
                key,
                value,
                items,
                guards,
            } => {
                self.gen_dict_comp(key, value, items, guards)?;
            }
            ExprKind::GeneratorExp { elem, items, guards } => {
                self.gen_generator_exp(elem, items, guards)?;
            }
            ExprKind::Dict(entries) => {
                for (k, v) in entries {
                    self.gen_expr(k)?;
                    self.gen_expr(v)?;
                }
                self.cg.emit(Instruction::DictNew(entries.len()));
            }
            ExprKind::Set(elems) => {
                for e in elems {
                    self.gen_expr(e)?;
                }
                self.cg.emit(Instruction::SetNew(elems.len()));
            }
            ExprKind::Tuple(elems) => {
                for e in elems {
                    self.gen_expr(e)?;
                }
                self.cg.emit(Instruction::TupleNew(elems.len()));
            }
            ExprKind::Bytes(bytes) => {
                self.cg
                    .emit(Instruction::Push(Value::Bytes(Arc::new(bytes.clone()))));
            }
            ExprKind::DoFunc {
                params,
                body,
                return_type,
                return_strong,
                return_wrapper,
            } => {
                let param_names: HashSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let free = free_vars::free_vars_in_block(body, &param_names);
                let captured: HashSet<String> = free.iter().cloned().collect();
                let is_generator = Self::block_has_yield(body);
                let func = self.compile_function(
                    "<do>",
                    params,
                    body,
                    CompileFnExtras {
                        return_type: return_type.as_ref(),
                        return_strong: *return_strong,
                        return_wrapper: return_wrapper.as_ref().map(|b| *b.clone()),
                        captured_names: captured,
                        is_generator,
                    },
                )?;
                self.emit_function_value_with_defaults(params, func)?;
                if !free.is_empty() {
                    for name in &free {
                        self.cg
                            .emit(Instruction::Push(Value::Text(name.clone())));
                        self.cg.emit(Instruction::Load(name.clone()));
                    }
                    self.cg.emit(Instruction::DictNew(free.len()));
                    self.cg
                        .emit(Instruction::Load("__make_closure__".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                }
            }
            ExprKind::Pipeline {
                left,
                right,
                pipe_name,
            } => {
                self.gen_expr(left)?;
                self.emit_store_temp(pipe_name);
                self.gen_expr(right)?;
            }
            ExprKind::Slice {
                object,
                start,
                end,
                step,
            } => {
                self.gen_expr(object)?;
                self.gen_slice_bound(start.as_deref())?;
                self.gen_slice_bound(end.as_deref())?;
                self.gen_slice_bound(step.as_deref())?;
                self.cg.emit(Instruction::SliceGet);
            }
            ExprKind::TypeConvert { type_expr, value } => {
                if self.macro_depth > 0 {
                    self.gen_materialized_ast_expr(value)?;
                    self.gen_materialized_ast_expr(type_expr)?;
                    self.cg.emit(Instruction::Load("__ast_type_convert__".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                } else {
                    self.gen_type_convert_type_expr(type_expr)?;
                    self.gen_expr(value)?;
                    self.cg.emit(Instruction::Load("convert".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                }
            }
            ExprKind::MacroCall { callee, args } => {
                if self.macro_depth > 0 {
                    // 宏体内嵌套宏调用只组合 AST，此处不展开。
                    self.cg.emit(Instruction::VecNew(0));
                    for arg in args {
                        self.gen_push_macro_call_arg(arg)?;
                        self.cg.emit(Instruction::ListAppend);
                    }
                    self.gen_frozen_ast_expr(callee)?;
                    self.cg
                        .emit(Instruction::Load("__ast_macro_call__".into()));
                    self.cg.emit(Instruction::Call { argc: 2 });
                } else {
                    for arg in args {
                        self.gen_push_macro_call_arg(arg)?;
                    }
                    self.gen_macro_callee(callee)?;
                    self.cg.emit(Instruction::MacroCall { argc: args.len() });
                }
            }
            ExprKind::Quote {
                hygienic_names,
                bindings,
                body,
            } => {
                let mut hyg_vals = Vec::new();
                for name in hygienic_names {
                    hyg_vals.push(Value::Text(name.clone()));
                }
                self.cg.emit(Instruction::Push(Value::List(Shared::new(
                    hyg_vals,
                ))));
                let mut bind_vals = Vec::new();
                for binding in bindings {
                    let ast = runtime_ast::ast_from_expr(binding);
                    bind_vals.push(Value::RuntimeAst(Arc::new(ast)));
                }
                self.cg.emit(Instruction::Push(Value::List(Shared::new(
                    bind_vals,
                ))));
                let body_ast = runtime_ast::ast_from_block(body);
                self.cg
                    .emit(Instruction::Push(Value::RuntimeAst(Arc::new(body_ast))));
                self.cg.emit(Instruction::Load("quote".into()));
                self.cg.emit(Instruction::Call { argc: 3 });
            }
            ExprKind::IfThenElse {
                cond,
                then_expr,
                else_expr,
            } => {
                let else_label = self.cg.fresh_label();
                let end = self.cg.fresh_label();
                self.gen_expr(cond)?;
                self.cg.emit(Instruction::GotoIfNot(else_label));
                self.gen_expr(then_expr)?;
                self.cg.emit(Instruction::Goto(end));
                self.cg.mark_label(else_label);
                self.gen_expr(else_expr)?;
                self.cg.mark_label(end);
            }
            ExprKind::Handle { operand } => {
                let catch_label = self.cg.fresh_label();
                let end_label = self.cg.fresh_label();
                self.cg.emit(Instruction::EnterTry {
                    catch_label,
                    else_label: 0,
                    end_label,
                });
                // 与 try 一样入栈，使 loop 内 break/continue 能补发 PopTry。
                self.handler_stack.push(OpenHandler::Try);
                self.gen_expr(operand)?;
                self.cg.emit(Instruction::Goto(end_label));
                self.cg.mark_label(catch_label);
                self.cg.emit(Instruction::Push(Value::None));
                self.cg.mark_label(end_label);
                self.handler_stack.pop();
                self.cg.emit(Instruction::PopTry);
            }
            ExprKind::Go { operand } => {
                self.gen_go_operand(operand)?;
            }
            ExprKind::Await { operand } => {
                if let ExprKind::Call { callee, args } = &operand.kind {
                    let has_named = args.iter().any(|a| a.name.is_some());
                    let has_kwsplat = args.iter().any(|a| a.is_kwsplat);
                    let has_splat = args.iter().any(|a| a.is_splat);
                    if !has_named && !has_kwsplat && !has_splat {
                        for a in args {
                            self.gen_expr(&a.value)?;
                        }
                        self.gen_expr(callee)?;
                        self.cg.emit(Instruction::GoCall(args.len()));
                        self.cg.emit(Instruction::Await);
                    } else {
                        self.gen_expr(operand)?;
                        self.cg.emit(Instruction::Await);
                    }
                } else {
                    self.gen_expr(operand)?;
                    self.cg.emit(Instruction::Await);
                }
            }
            ExprKind::Suspend => {
                self.cg.emit(Instruction::Suspend);
            }
            ExprKind::Select { cases, else_block } => {
                self.gen_select(cases, else_block.as_ref())?;
            }
            ExprKind::NamedAssign { name, value } => {
                self.gen_expr(value)?;
                self.emit_bind_name(name);
                self.emit_load_name(name);
            }
            ExprKind::Match {
                subject,
                cases,
                else_block,
            } => {
                let end = self.cg.fresh_label();
                self.match_expr_ends.push(end);
                self.gen_match(subject, cases, else_block.as_ref(), true)?;
                self.match_expr_ends.pop();
                self.cg.mark_label(end);
            }
        }
        Ok(())
    }

    fn gen_go_operand(&mut self, operand: &Expr) -> Result<()> {
        if let ExprKind::Call { callee, args } = &operand.kind {
            let has_named = args.iter().any(|a| a.name.is_some());
            let has_kwsplat = args.iter().any(|a| a.is_kwsplat);
            let has_splat = args.iter().any(|a| a.is_splat);
            if !has_named && !has_kwsplat && !has_splat {
                for a in args {
                    self.gen_expr(&a.value)?;
                }
                self.gen_expr(callee)?;
                self.cg.emit(Instruction::GoCall(args.len()));
                return Ok(());
            }
        }
        // 非调用：求值后包装为已完成 Task。
        self.gen_expr(operand)?;
        self.cg.emit(Instruction::GoValue);
        Ok(())
    }

    fn gen_select(&mut self, cases: &[SelectCase], else_block: Option<&Block>) -> Result<()> {
        let end = self.cg.fresh_label();
        let start = self.cg.fresh_label();

        // 预处理 sleep 截止时间。
        let mut sleep_temps: Vec<Option<String>> = Vec::with_capacity(cases.len());
        for case in cases {
            if let Some(secs) = select_sleep_seconds_expr(&case.event) {
                let tmp = self.cg.fresh_temp("__sel_deadline");
                self.gen_expr(secs)?;
                self.cg.emit(Instruction::MakeDeadline);
                self.emit_store_temp(&tmp);
                sleep_temps.push(Some(tmp));
            } else {
                sleep_temps.push(None);
            }
        }

        self.cg.mark_label(start);
        for (i, case) in cases.iter().enumerate() {
            let next = self.cg.fresh_label();
            self.gen_select_poll(&case.event, sleep_temps[i].as_deref())?;
            self.cg.emit(Instruction::GotoIfNot(next));
            // ready：栈顶为事件结果值
            if let Some(name) = &case.bind {
                if name != "_" {
                    self.emit_bind_name(name);
                } else {
                    self.cg.emit(Instruction::Pop);
                }
            } else {
                self.cg.emit(Instruction::Pop);
            }
            self.gen_block(&case.body, true)?;
            self.cg.emit(Instruction::Goto(end));
            self.cg.mark_label(next);
        }

        if let Some(else_b) = else_block {
            // 粗略：若没有任何 case 就绪，先让步再重试；多次空转后走 else。
            // 用一次 Yield 后若仍全不就绪则 else（简化：直接检查通道全关由 SelectTryRecv 的 closed 处理）。
            // 此处：跑一次调度，再若仍无进展则 else。
            self.cg.emit(Instruction::Suspend);
            // 再 poll 一轮；若仍无则 else
            let else_lbl = self.cg.fresh_label();
            for (i, case) in cases.iter().enumerate() {
                let next = self.cg.fresh_label();
                self.gen_select_poll(&case.event, sleep_temps[i].as_deref())?;
                self.cg.emit(Instruction::GotoIfNot(next));
                if let Some(name) = &case.bind {
                    if name != "_" {
                        self.emit_bind_name(name);
                    } else {
                        self.cg.emit(Instruction::Pop);
                    }
                } else {
                    self.cg.emit(Instruction::Pop);
                }
                self.gen_block(&case.body, true)?;
                self.cg.emit(Instruction::Goto(end));
                self.cg.mark_label(next);
            }
            self.cg.mark_label(else_lbl);
            self.gen_block(else_b, true)?;
            self.cg.emit(Instruction::Goto(end));
        } else {
            self.cg.emit(Instruction::Suspend);
            self.cg.emit(Instruction::Goto(start));
        }
        self.cg.mark_label(end);
        Ok(())
    }

    fn gen_select_poll(&mut self, event: &Expr, sleep_tmp: Option<&str>) -> Result<()> {
        if let Some(tmp) = sleep_tmp {
            self.emit_load_temp(tmp);
            self.cg.emit(Instruction::SelectPollDeadline);
            // ready 时补一个 none 作为绑定值
            let not_ready = self.cg.fresh_label();
            let done = self.cg.fresh_label();
            self.cg.emit(Instruction::GotoIfNot(not_ready));
            self.cg.emit(Instruction::Push(Value::None));
            self.cg.emit(Instruction::Push(Value::Bool(true)));
            self.cg.emit(Instruction::Goto(done));
            self.cg.mark_label(not_ready);
            self.cg.emit(Instruction::Push(Value::Bool(false)));
            self.cg.mark_label(done);
            return Ok(());
        }
        match &event.kind {
            ExprKind::Await { operand } => {
                self.gen_expr(operand)?;
                self.cg.emit(Instruction::SelectPollTask);
                Ok(())
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Member { object, field } = &callee.kind {
                    if field == "recv" && args.is_empty() {
                        self.gen_expr(object)?;
                        self.cg.emit(Instruction::SelectTryRecv);
                        return Ok(());
                    }
                    if field == "send" && args.len() == 1 {
                        self.gen_expr(object)?;
                        self.gen_expr(&args[0].value)?;
                        self.cg.emit(Instruction::SelectTrySend);
                        // send ready → 绑定值用 none
                        let not_ready = self.cg.fresh_label();
                        let done = self.cg.fresh_label();
                        self.cg.emit(Instruction::GotoIfNot(not_ready));
                        self.cg.emit(Instruction::Push(Value::None));
                        self.cg.emit(Instruction::Push(Value::Bool(true)));
                        self.cg.emit(Instruction::Goto(done));
                        self.cg.mark_label(not_ready);
                        self.cg.emit(Instruction::Push(Value::Bool(false)));
                        self.cg.mark_label(done);
                        return Ok(());
                    }
                }
                Err(RuntimeError::msg(
                    "select case event must be ch.recv(), ch.send(v), await t, or sleep(...)",
                ))
            }
            _ => Err(RuntimeError::msg(
                "select case event must be ch.recv(), ch.send(v), await t, or sleep(...)",
            )),
        }
    }

    fn gen_list_comp(
        &mut self,
        elem: &Expr,
        items: &[ForItem],
        guards: &[Expr],
    ) -> Result<()> {
        self.gen_collection_comp(CompKind::List, Some(elem), None, None, items, guards)
    }

    fn gen_set_comp(
        &mut self,
        elem: &Expr,
        items: &[ForItem],
        guards: &[Expr],
    ) -> Result<()> {
        self.gen_collection_comp(CompKind::Set, Some(elem), None, None, items, guards)
    }

    fn gen_dict_comp(
        &mut self,
        key: &Expr,
        value: &Expr,
        items: &[ForItem],
        guards: &[Expr],
    ) -> Result<()> {
        self.gen_collection_comp(CompKind::Dict, None, Some(key), Some(value), items, guards)
    }

    fn gen_collection_comp(
        &mut self,
        kind: CompKind,
        elem: Option<&Expr>,
        key: Option<&Expr>,
        value: Option<&Expr>,
        items: &[ForItem],
        guards: &[Expr],
    ) -> Result<()> {
        if items.is_empty() {
            return Err(RuntimeError::type_err("comprehension requires for clause"));
        }
        let result_name = self.cg.fresh_temp(match kind {
            CompKind::List => "__list_comp_result",
            CompKind::Set => "__set_comp_result",
            CompKind::Dict => "__dict_comp_result",
        });

        match kind {
            CompKind::List => self.cg.emit(Instruction::VecNew(0)),
            CompKind::Set => self.cg.emit(Instruction::SetNew(0)),
            CompKind::Dict => self.cg.emit(Instruction::DictNew(0)),
        };
        self.emit_store_temp(&result_name);

        self.gen_for_iter_setup(items)?;
        let start = self.cg.fresh_label();
        let end = self.cg.fresh_label();
        self.cg.mark_label(start);
        self.cg.emit(Instruction::IterNext);
        self.cg.emit(Instruction::GotoIfNot(end));
        self.gen_for_iter_bind(items)?;

        for guard in guards {
            self.gen_expr(guard)?;
            self.cg.emit(Instruction::GotoIfNot(start));
        }

        self.emit_load_temp(&result_name);
        match kind {
            CompKind::List => {
                let e = elem.ok_or_else(|| {
                    RuntimeError::msg("internal: list comprehension missing element")
                })?;
                self.gen_expr(e)?;
                self.cg.emit(Instruction::ListAppend);
            }
            CompKind::Set => {
                let e = elem.ok_or_else(|| {
                    RuntimeError::msg("internal: set comprehension missing element")
                })?;
                self.gen_expr(e)?;
                self.cg.emit(Instruction::SetAdd);
            }
            CompKind::Dict => {
                let k = key.ok_or_else(|| {
                    RuntimeError::msg("internal: dict comprehension missing key")
                })?;
                let v = value.ok_or_else(|| {
                    RuntimeError::msg("internal: dict comprehension missing value")
                })?;
                self.gen_expr(k)?;
                self.gen_expr(v)?;
                self.cg.emit(Instruction::DictSet);
            }
        }
        self.emit_store_temp(&result_name);

        self.cg.emit(Instruction::Goto(start));
        self.cg.mark_label(end);
        self.cg.emit(Instruction::IterEnd);
        self.emit_load_temp(&result_name);
        Ok(())
    }

    fn gen_generator_exp(
        &mut self,
        elem: &Expr,
        items: &[ForItem],
        guards: &[Expr],
    ) -> Result<()> {
        if items.is_empty() {
            return Err(RuntimeError::type_err("generator expression requires for clause"));
        }

        // 源可迭代对象（多路时 zip）。
        if items.len() == 1 {
            self.gen_expr(&items[0].iterable)?;
        } else {
            for item in items {
                self.gen_expr(&item.iterable)?;
            }
            self.cg
                .emit(Instruction::Load("__zip_iter__".into()));
            self.cg.emit(Instruction::Call {
                argc: items.len(),
            });
        }

        let params: Vec<FuncParam> = items
            .iter()
            .map(|it| FuncParam {
                name: it.name.clone(),
                is_variadic: false,
                is_kwvariadic: false,
                implicit: false,
                type_expr: None,
                type_strong: false,
                default_expr: None,
            })
            .collect();
        let param_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();

        let elem_free = free_vars::free_vars_in_expr(elem, &param_names);
        let elem_body = vec![LocatedStmt { line: 0, column: 1, stmt: Stmt::Return(Some(elem.clone())),
        }];
        let elem_fn = self.compile_function(
            "<genexpr>",
            &params,
            &elem_body,
            CompileFnExtras {
                return_type: None,
                return_strong: false,
                return_wrapper: None,
                captured_names: elem_free.iter().cloned().collect(),
                is_generator: false,
            },
        )?;
        self.cg
            .emit(Instruction::Push(Value::Function(Arc::new(elem_fn))));
        if !elem_free.is_empty() {
            for name in &elem_free {
                self.cg
                    .emit(Instruction::Push(Value::Text(name.clone())));
                self.cg.emit(Instruction::Load(name.clone()));
            }
            self.cg.emit(Instruction::DictNew(elem_free.len()));
            self.cg
                .emit(Instruction::Load("__make_closure__".into()));
            self.cg.emit(Instruction::Call { argc: 2 });
        }

        for guard in guards {
            let g_free = free_vars::free_vars_in_expr(guard, &param_names);
            let g_body = vec![LocatedStmt { line: 0, column: 1, stmt: Stmt::Return(Some(guard.clone())),
            }];
            let g_fn = self.compile_function(
                "<genexpr_guard>",
                &params,
                &g_body,
                CompileFnExtras {
                    return_type: None,
                    return_strong: false,
                    return_wrapper: None,
                    captured_names: g_free.iter().cloned().collect(),
                    is_generator: false,
                },
            )?;
            self.cg
                .emit(Instruction::Push(Value::Function(Arc::new(g_fn))));
            if !g_free.is_empty() {
                for name in &g_free {
                    self.cg
                        .emit(Instruction::Push(Value::Text(name.clone())));
                    self.cg.emit(Instruction::Load(name.clone()));
                }
                self.cg.emit(Instruction::DictNew(g_free.len()));
                self.cg
                    .emit(Instruction::Load("__make_closure__".into()));
                self.cg.emit(Instruction::Call { argc: 2 });
            }
        }
        self.cg.emit(Instruction::VecNew(guards.len()));

        self.cg
            .emit(Instruction::Load("__make_genexpr__".into()));
        self.cg.emit(Instruction::Call { argc: 3 });
        Ok(())
    }

    fn gen_for(&mut self, items: &[ForItem], body: &Block) -> Result<()> {
        if items.is_empty() {
            return Err(RuntimeError::type_err("for requires at least one iterator"));
        }
        self.gen_for_iter_setup(items)?;
        let start = self.cg.fresh_label();
        let end = self.cg.fresh_label();
        self.cg.mark_label(start);
        self.cg.emit(Instruction::IterNext);
        self.cg.emit(Instruction::GotoIfNot(end));
        self.gen_for_iter_bind(items)?;
        self.loop_break_labels.push(end);
        self.loop_continue_labels.push(start);
        self.loop_handler_depths.push(self.handler_stack.len());
        self.loop_owns_stack_counter.push(false);
        for s in body {
            self.gen_stmt(s, false)?;
        }
        self.loop_break_labels.pop();
        self.loop_continue_labels.pop();
        self.loop_handler_depths.pop();
        self.loop_owns_stack_counter.pop();
        self.cg.emit(Instruction::Goto(start));
        self.cg.mark_label(end);
        self.cg.emit(Instruction::IterEnd);
        Ok(())
    }

    fn gen_for_iter_setup(&mut self, items: &[ForItem]) -> Result<()> {
        if items.len() == 1 {
            self.gen_expr(&items[0].iterable)?;
            self.cg.emit(Instruction::IterNew);
            return Ok(());
        }
        for item in items {
            self.gen_expr(&item.iterable)?;
        }
        self.cg
            .emit(Instruction::Load("__zip_iter__".into()));
        self.cg.emit(Instruction::Call {
            argc: items.len(),
        });
        self.cg.emit(Instruction::IterNew);
        Ok(())
    }

    fn gen_for_iter_bind(&mut self, items: &[ForItem]) -> Result<()> {
        if items.len() == 1 {
            self.emit_bind_name(&items[0].name);
            return Ok(());
        }
        let tuple_name = self.cg.fresh_temp("__zip_tuple");
        self.emit_store_temp(&tuple_name);
        for (i, item) in items.iter().enumerate() {
            self.emit_load_temp(&tuple_name);
            self.cg
                .emit(Instruction::PushSmall(i as i64));
            self.cg.emit(Instruction::Index);
            self.emit_bind_name(&item.name);
        }
        Ok(())
    }

    fn gen_type_convert_type_expr(&mut self, expr: &Expr) -> Result<()> {
        let name = type_convert_type_name(expr)?;
        self.cg.emit(Instruction::Push(Value::type_ref(name)));
        Ok(())
    }

    fn gen_slice_bound(&mut self, bound: Option<&Expr>) -> Result<()> {
        if let Some(expr) = bound {
            self.gen_expr(expr)?;
        } else {
            self.cg.emit(Instruction::Push(Value::None));
        }
        Ok(())
    }

    /// 栈顶为待解构值；绑定后栈净弹出该值。
    fn gen_destruct_bind(
        &mut self,
        pattern: &DestructPattern,
        is_const: bool,
        declaring: bool,
    ) -> Result<()> {
        match pattern {
            DestructPattern::Name(name) => {
                if declaring {
                    self.emit_bind_name_flags(name, is_const);
                } else {
                    self.emit_store_name(name);
                }
            }
            DestructPattern::Discard => {
                self.cg.emit(Instruction::Pop);
            }
            DestructPattern::Tuple(elems) | DestructPattern::List(elems) => {
                let (before, rest, after) = split_destruct_elems(elems)?;
                if let Some(rest_elem) = rest {
                    self.cg.emit(Instruction::UnpackRest {
                        before: before.len(),
                        after: after.len(),
                    });
                    for pat in after.iter().rev() {
                        self.gen_destruct_bind(pat, is_const, declaring)?;
                    }
                    match rest_elem {
                        DestructElem::Rest(name) => {
                            if declaring {
                                self.emit_bind_name_flags(name, is_const);
                            } else {
                                self.emit_store_name(name);
                            }
                        }
                        DestructElem::RestDiscard => {
                            self.cg.emit(Instruction::Pop);
                        }
                        DestructElem::Pat(_) => {
                            return Err(RuntimeError::msg("internal: rest slot is not Rest"));
                        }
                    }
                    for pat in before.iter().rev() {
                        self.gen_destruct_bind(pat, is_const, declaring)?;
                    }
                } else {
                    let n = before.len();
                    self.cg.emit(Instruction::UnpackExact(n));
                    for pat in before.iter().rev() {
                        self.gen_destruct_bind(pat, is_const, declaring)?;
                    }
                }
            }
        }
        Ok(())
    }
}

type DestructSplit<'a> = (
    Vec<&'a DestructPattern>,
    Option<&'a DestructElem>,
    Vec<&'a DestructPattern>,
);

fn split_destruct_elems(elems: &[DestructElem]) -> std::result::Result<DestructSplit<'_>, RuntimeError> {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut rest = None;
    for elem in elems {
        match elem {
            DestructElem::Pat(p) => {
                if rest.is_some() {
                    after.push(p);
                } else {
                    before.push(p);
                }
            }
            DestructElem::Rest(_) | DestructElem::RestDiscard => {
                if rest.is_some() {
                    return Err(RuntimeError::msg(
                        "multiple *rest in destructuring pattern",
                    ));
                }
                rest = Some(elem);
            }
        }
    }
    Ok((before, rest, after))
}

fn destruct_bound_names(pattern: &DestructPattern) -> Vec<String> {
    let mut out = Vec::new();
    collect_destruct_names(pattern, &mut out);
    out
}

fn collect_destruct_names(pattern: &DestructPattern, out: &mut Vec<String>) {
    match pattern {
        DestructPattern::Name(n) => out.push(n.clone()),
        DestructPattern::Discard => {}
        DestructPattern::Tuple(elems) | DestructPattern::List(elems) => {
            for el in elems {
                match el {
                    DestructElem::Pat(p) => collect_destruct_names(p, out),
                    DestructElem::Rest(n) => out.push(n.clone()),
                    DestructElem::RestDiscard => {}
                }
            }
        }
    }
}

fn type_convert_type_name(expr: &Expr) -> Result<String> {
    match &expr.kind {
        ExprKind::Var(name) => Ok(name.clone()),
        ExprKind::Member { object, field } => {
            Ok(format!(
                "{}.{}",
                type_convert_type_name(object)?,
                field
            ))
        }
        _ => Err(RuntimeError::msg("expected type name in convert")),
    }
}

fn const_default_value(expr: &Expr) -> Option<Value> {
    match &expr.kind {
        ExprKind::None => Some(Value::None),
        ExprKind::Bool(b) => Some(Value::Bool(*b)),
        ExprKind::String(s) => Some(Value::Text(s.clone())),
        ExprKind::Number(s) => Num::from_literal(s).ok().map(Value::Num),
        _ => None,
    }
}

/// 识别 `sleep(secs)` / `std.time.sleep(secs)` 等单参数 sleep 调用，返回秒数表达式。
fn select_sleep_seconds_expr(event: &Expr) -> Option<&Expr> {
    let ExprKind::Call { callee, args } = &event.kind else {
        return None;
    };
    if args.len() != 1 || args[0].name.is_some() || args[0].is_splat || args[0].is_kwsplat {
        return None;
    }
    if call_ends_with_sleep(callee) {
        Some(&args[0].value)
    } else {
        None
    }
}

fn call_ends_with_sleep(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Var(name) => name == "sleep",
        ExprKind::Member { field, .. } => field == "sleep",
        _ => false,
    }
}

impl Default for Generator {
    fn default() -> Self {
        Self::new()
    }
}
