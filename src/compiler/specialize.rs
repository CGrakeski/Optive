//! 赋值区间类型特化：对可证标签的运算下降为特化指令。
//!
//! 在基本块内对操作数栈与快局部槽做标签数据流；汇合点（`Label`）清空槽位事实。
//! 证明失败则保留通用指令。
//!
//! 入参：强注解可在编译期播种；无强注解时标签由调用时**访问实参运行时类型**确定，
//! 单份函数体无法在编译期写下具体 Tag，故不猜测播种——执行期通用算子通过访问类型分发。

use crate::ast::Expr;
use crate::opcode::Instruction;
use crate::types::{static_type_value_from_expr, type_value_base};
use crate::value::Value;

/// 分析用的粗粒度运行时标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    Num,
    Text,
    Bool,
    None,
    List,
    Dict,
    Set,
    Tuple,
    Bytes,
}

/// 由强类型注解得到可证入口标签（与 `check_strong_params` 一致）。
pub(crate) fn tag_from_strong_type(ty: &Expr) -> Option<Tag> {
    let val = static_type_value_from_expr(ty)?;
    let name = type_value_base(&val)?;
    match name {
        "num" | "int" | "float" => Some(Tag::Num),
        "text" | "str" | "string" => Some(Tag::Text),
        "bool" => Some(Tag::Bool),
        "none" | "nonetype" => Some(Tag::None),
        "list" => Some(Tag::List),
        "dict" => Some(Tag::Dict),
        "set" => Some(Tag::Set),
        "tuple" => Some(Tag::Tuple),
        "bytes" => Some(Tag::Bytes),
        _ => None,
    }
}

/// 对指令序列做就地特化改写（无入口播种）。
pub fn specialize_instructions(code: &mut [Instruction]) {
    specialize_with_entry(code, &[]);
}

/// 带函数入口槽位标签播种的特化（如强注解参数 `x:: num` → 槽 0 为 `Num`）。
///
/// `entry_env[i]` 为参数槽 `i` 在入口可证的标签；长度可短于实际槽数，缺省为未知。
/// 无强注解的参数不得填入猜测标签——其实参类型在调用绑定时通过访问运行时值确定。
pub(crate) fn specialize_with_entry(code: &mut [Instruction], entry_env: &[Option<Tag>]) {
    let mut env: Vec<Option<Tag>> = entry_env.to_vec();
    let mut stack: Vec<Option<Tag>> = Vec::new();

    for ins in code.iter_mut() {
        match ins {
            Instruction::Label(_) => {
                // 汇合后槽位事实不可靠；入口播种只对落到 Label 之前的区间有效。
                env.fill(None);
                stack.clear();
            }
            Instruction::Push(v) => {
                stack.push(tag_of_value(v));
            }
            Instruction::PushSmall(_) => {
                stack.push(Some(Tag::Num));
            }
            Instruction::Pop => {
                let _ = stack.pop();
            }
            Instruction::LoadFast(slot) => {
                let t = env_get(&env, *slot);
                stack.push(t);
            }
            Instruction::StoreFast(slot) | Instruction::BindFast { slot, .. } => {
                let t = stack.pop().unwrap_or(None);
                env_set(&mut env, *slot, t);
            }
            Instruction::LoadFastSubImm { slot, .. } => {
                let t = env_get(&env, *slot);
                // 结果类型：若槽为 Num 则差仍为 Num；否则未知。
                stack.push(if t == Some(Tag::Num) { Some(Tag::Num) } else { None });
            }
            Instruction::LoadFastLeImm { slot, .. } => {
                let t = env_get(&env, *slot);
                let _ = t;
                stack.push(Some(Tag::Bool));
            }
            Instruction::Load(_)
            | Instruction::LoadGlobal(_)
            | Instruction::LoadMacro(_)
            | Instruction::NewVarOrLoad(_) => {
                stack.push(None);
            }
            Instruction::Store(_) | Instruction::StoreGlobal(_) => {
                let _ = stack.pop();
            }
            // NewVar 不弹栈（与 VM 一致）；误弹会让标签栈与真实栈失衡。
            Instruction::NewVar { .. } => {}
            Instruction::Call { argc }
            | Instruction::CallSelf { argc }
            | Instruction::MacroCall { argc } => {
                let argc = *argc;
                env.fill(None);
                for _ in 0..argc {
                    let _ = stack.pop();
                }
                let _ = stack.pop(); // 被调者
                stack.push(None);
            }
            Instruction::CallGlobal { argc, .. } => {
                let argc = *argc;
                env.fill(None);
                for _ in 0..argc {
                    let _ = stack.pop();
                }
                stack.push(None);
            }
            Instruction::CallList | Instruction::CallEx => {
                env.fill(None);
                stack.clear();
                stack.push(None);
            }
            Instruction::Ret | Instruction::RetLeave | Instruction::RetFast(_) => {
                stack.clear();
            }
            Instruction::Goto(_) => {
                stack.clear();
            }
            Instruction::GotoIf(_) | Instruction::GotoIfNot(_) => {
                let _ = stack.pop();
                // 落空路径保留当前 env/stack；跳转目标在 Label 处清空。
            }
            Instruction::LoopCountdown(_) => {
                // 继续路径：计数器仍在栈顶；跳出路径在 Label 处清空。
                if let Some(top) = stack.last_mut() {
                    *top = Some(Tag::Num);
                }
            }
            Instruction::VecNew(n) => {
                for _ in 0..*n {
                    let _ = stack.pop();
                }
                stack.push(Some(Tag::List));
            }
            Instruction::DictNew(n) => {
                for _ in 0..(2 * *n) {
                    let _ = stack.pop();
                }
                stack.push(Some(Tag::Dict));
            }
            Instruction::SetNew(n) => {
                for _ in 0..*n {
                    let _ = stack.pop();
                }
                stack.push(Some(Tag::Set));
            }
            Instruction::TupleNew(n) => {
                for _ in 0..*n {
                    let _ = stack.pop();
                }
                stack.push(Some(Tag::Tuple));
            }
            Instruction::ListAppend | Instruction::ListExtend => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::List));
            }
            Instruction::SetAdd => {
                let _ = stack.pop(); // 元素
                let _ = stack.pop(); // 集合
                stack.push(Some(Tag::Set));
            }
            Instruction::DictSet => {
                let _ = stack.pop(); // 值
                let _ = stack.pop(); // 键
                let _ = stack.pop(); // 字典
                stack.push(Some(Tag::Dict));
            }
            Instruction::Neg | Instruction::Invert => {
                let t = stack.pop().unwrap_or(None);
                stack.push(match t {
                    Some(Tag::Num) => Some(Tag::Num),
                    _ => None,
                });
            }
            Instruction::Mod => {
                let (rb, ra) = pop2(&mut stack);
                if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
                    *ins = Instruction::ModNumNum;
                    stack.push(Some(Tag::Num));
                } else {
                    stack.push(None);
                }
            }
            Instruction::BitAnd
            | Instruction::BitOr
            | Instruction::BitXor
            | Instruction::LShift
            | Instruction::RShift => {
                let (rb, ra) = pop2(&mut stack);
                stack.push(match (ra, rb) {
                    (Some(Tag::Num), Some(Tag::Num)) => Some(Tag::Num),
                    _ => None,
                });
            }
            Instruction::Not | Instruction::TruthyNot => {
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::Add => {
                let (rb, ra) = pop2(&mut stack);
                if let Some(spec) = specialize_add(ra, rb) {
                    *ins = spec;
                    stack.push(result_tag_bin(ra, rb, OpKind::Add));
                } else {
                    stack.push(result_tag_bin(ra, rb, OpKind::Add));
                }
            }
            Instruction::Sub => {
                let (rb, ra) = pop2(&mut stack);
                if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
                    *ins = Instruction::SubNumNum;
                    stack.push(Some(Tag::Num));
                } else {
                    stack.push(result_tag_bin(ra, rb, OpKind::Sub));
                }
            }
            Instruction::Mul => {
                let (rb, ra) = pop2(&mut stack);
                if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
                    *ins = Instruction::MulNumNum;
                    stack.push(Some(Tag::Num));
                } else {
                    stack.push(None);
                }
            }
            Instruction::Div => {
                let (rb, ra) = pop2(&mut stack);
                if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
                    *ins = Instruction::DivNumNum;
                    stack.push(Some(Tag::Num));
                } else {
                    stack.push(None);
                }
            }
            Instruction::Pow => {
                let (rb, ra) = pop2(&mut stack);
                if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
                    *ins = Instruction::PowNumNum;
                    stack.push(Some(Tag::Num));
                } else {
                    stack.push(None);
                }
            }
            Instruction::Eq => {
                rewrite_cmp(ins, &mut stack, Instruction::EqNumNum, false);
            }
            Instruction::Ne => {
                rewrite_cmp(ins, &mut stack, Instruction::NeNumNum, false);
            }
            Instruction::Lt => {
                rewrite_cmp(ins, &mut stack, Instruction::LtNumNum, true);
            }
            Instruction::Le => {
                rewrite_cmp(ins, &mut stack, Instruction::LeNumNum, true);
            }
            Instruction::Gt => {
                rewrite_cmp(ins, &mut stack, Instruction::GtNumNum, true);
            }
            Instruction::Ge => {
                rewrite_cmp(ins, &mut stack, Instruction::GeNumNum, true);
            }
            Instruction::In | Instruction::Is | Instruction::IsNot | Instruction::MatchEq => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::UnpackExact(n) => {
                let _ = stack.pop();
                for _ in 0..*n {
                    stack.push(None);
                }
            }
            Instruction::UnpackRest { before, after } => {
                let _ = stack.pop();
                for _ in 0..*before {
                    stack.push(None);
                }
                stack.push(Some(Tag::List));
                for _ in 0..*after {
                    stack.push(None);
                }
            }
            Instruction::Index => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(None);
            }
            Instruction::IndexSet | Instruction::SliceSet => {
                stack.clear();
            }
            Instruction::SliceGet => {
                for _ in 0..4 {
                    let _ = stack.pop();
                }
                stack.push(None);
            }
            Instruction::GetAttr(_) => {
                let _ = stack.pop();
                stack.push(None);
            }
            Instruction::SetField(_) => {
                let _ = stack.pop();
                let _ = stack.pop();
            }
            Instruction::StructNew { argc, .. } => {
                for _ in 0..*argc {
                    let _ = stack.pop();
                }
                stack.push(None);
            }
            Instruction::VariantNew { .. } => {
                let _ = stack.pop();
                stack.push(None);
            }
            Instruction::IterNew => {
                let _ = stack.pop();
                stack.push(None);
            }
            Instruction::IterNext => {
                stack.push(Some(Tag::Bool));
            }
            Instruction::IterEnd | Instruction::EnterScope | Instruction::LeaveScope => {}
            Instruction::Throw => {
                let _ = stack.pop();
                stack.clear();
                env.fill(None);
            }
            Instruction::PushExc => {
                stack.push(None);
            }
            Instruction::EnterTry { .. } | Instruction::EndTry | Instruction::PopTry => {}
            Instruction::ExcMatch(_) | Instruction::IsList | Instruction::IsInstance(_) => {
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::Rethrow => {
                stack.clear();
                env.fill(None);
            }
            Instruction::ListLen => {
                let _ = stack.pop();
                stack.push(Some(Tag::Num));
            }
            Instruction::TypeCheck => {
                // 栈顶值保留，类型仍按原标签（检查失败会抛错）。
            }
            Instruction::ResolveFuncTypes => {
                // 栈顶仍为 Function。
            }
            Instruction::FindMod(_) => {
                stack.push(None);
            }
            Instruction::RegisterExport(_) => {}
            Instruction::GoCall(argc) => {
                for _ in 0..*argc {
                    let _ = stack.pop();
                }
                let _ = stack.pop(); // callee
                stack.push(None); // Task
            }
            Instruction::GoValue | Instruction::Await | Instruction::Snap => {
                let _ = stack.pop();
                stack.push(None);
            }
            Instruction::Suspend => {}
            Instruction::Yield | Instruction::YieldFrom => {
                let _ = stack.pop();
            }
            Instruction::SelectTryRecv | Instruction::SelectPollTask => {
                let _ = stack.pop();
                // value? + bool 或仅 bool — 保守清空后压未知
                stack.push(None);
                stack.push(Some(Tag::Bool));
            }
            Instruction::SelectTrySend => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::MakeDeadline => {
                let _ = stack.pop();
                stack.push(Some(Tag::Num));
            }
            Instruction::SelectPollDeadline => {
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::SelectIdle(n) => {
                for _ in 0..*n {
                    let _ = stack.pop();
                }
            }
            Instruction::DelIndex => {
                let _ = stack.pop();
                let _ = stack.pop();
            }
            Instruction::DelName(_) | Instruction::DelAttr(_) => {}
            // 已是特化或其余：做保守栈效果
            Instruction::AddNumNum | Instruction::SubNumNum | Instruction::MulNumNum | Instruction::DivNumNum | Instruction::ModNumNum | Instruction::PowNumNum => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Num));
            }
            Instruction::AddTextText => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Text));
            }
            Instruction::AddListList => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::List));
            }
            Instruction::EqNumNum
            | Instruction::NeNumNum
            | Instruction::LtNumNum
            | Instruction::LeNumNum
            | Instruction::GtNumNum
            | Instruction::GeNumNum => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
            Instruction::And | Instruction::Or => {
                let _ = stack.pop();
                let _ = stack.pop();
                stack.push(Some(Tag::Bool));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum OpKind {
    Add,
    Sub,
}

fn rewrite_cmp(ins: &mut Instruction, stack: &mut Vec<Option<Tag>>, num_op: Instruction, _ord: bool) {
    let (rb, ra) = pop2(stack);
    if ra == Some(Tag::Num) && rb == Some(Tag::Num) {
        *ins = num_op;
    }
    stack.push(Some(Tag::Bool));
}

fn specialize_add(left: Option<Tag>, right: Option<Tag>) -> Option<Instruction> {
    match (left, right) {
        (Some(Tag::Num), Some(Tag::Num)) => Some(Instruction::AddNumNum),
        (Some(Tag::Text), Some(Tag::Text)) => Some(Instruction::AddTextText),
        (Some(Tag::List), Some(Tag::List)) => Some(Instruction::AddListList),
        _ => None,
    }
}

fn result_tag_bin(left: Option<Tag>, right: Option<Tag>, op: OpKind) -> Option<Tag> {
    match op {
        OpKind::Add => match (left, right) {
            (Some(Tag::Num), Some(Tag::Num)) => Some(Tag::Num),
            (Some(Tag::Text), Some(Tag::Text)) => Some(Tag::Text),
            (Some(Tag::List), Some(Tag::List)) => Some(Tag::List),
            _ => None,
        },
        OpKind::Sub => match (left, right) {
            (Some(Tag::Num), Some(Tag::Num)) => Some(Tag::Num),
            _ => None,
        },
    }
}

fn pop2(stack: &mut Vec<Option<Tag>>) -> (Option<Tag>, Option<Tag>) {
    let b = stack.pop().unwrap_or(None);
    let a = stack.pop().unwrap_or(None);
    (b, a)
}

fn env_get(env: &[Option<Tag>], slot: usize) -> Option<Tag> {
    env.get(slot).copied().flatten()
}

fn env_set(env: &mut Vec<Option<Tag>>, slot: usize, tag: Option<Tag>) {
    if slot >= env.len() {
        env.resize(slot + 1, None);
    }
    env[slot] = tag;
}

fn tag_of_value(v: &Value) -> Option<Tag> {
    match v {
        Value::Num(_) => Some(Tag::Num),
        Value::Text(_) => Some(Tag::Text),
        Value::Bool(_) => Some(Tag::Bool),
        Value::None => Some(Tag::None),
        Value::List(_) => Some(Tag::List),
        Value::Dict(_) => Some(Tag::Dict),
        Value::Set(_) => Some(Tag::Set),
        Value::Tuple(_) => Some(Tag::Tuple),
        Value::Bytes(_) => Some(Tag::Bytes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    #[test]
    fn specializes_num_add_of_small_pushes() {
        let mut code = vec![
            Instruction::PushSmall(1),
            Instruction::PushSmall(2),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_instructions(&mut code);
        assert!(matches!(code[2], Instruction::AddNumNum));
    }

    #[test]
    fn specializes_fast_local_num_interval() {
        let mut code = vec![
            Instruction::PushSmall(1),
            Instruction::StoreFast(0),
            Instruction::LoadFast(0),
            Instruction::PushSmall(1),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_instructions(&mut code);
        assert!(matches!(code[4], Instruction::AddNumNum));
    }

    #[test]
    fn does_not_specialize_across_label_join() {
        let mut code = vec![
            Instruction::PushSmall(1),
            Instruction::StoreFast(0),
            Instruction::Label(0),
            Instruction::LoadFast(0),
            Instruction::PushSmall(1),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_instructions(&mut code);
        assert!(matches!(code[5], Instruction::Add));
    }

    #[test]
    fn specializes_text_add() {
        let mut code = vec![
            Instruction::Push(Value::Text("a".into())),
            Instruction::Push(Value::Text("b".into())),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_instructions(&mut code);
        assert!(matches!(code[2], Instruction::AddTextText));
    }

    #[test]
    fn entry_seed_specializes_param_add() {
        let mut code = vec![
            Instruction::LoadFast(0),
            Instruction::PushSmall(1),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_with_entry(&mut code, &[Some(Tag::Num)]);
        assert!(matches!(code[2], Instruction::AddNumNum));
    }

    #[test]
    fn untyped_param_not_guessed_at_compile_time() {
        let mut code = vec![
            Instruction::LoadFast(0),
            Instruction::PushSmall(1),
            Instruction::Add,
            Instruction::Ret,
        ];
        specialize_with_entry(&mut code, &[None]);
        assert!(matches!(code[2], Instruction::Add));
    }
}
