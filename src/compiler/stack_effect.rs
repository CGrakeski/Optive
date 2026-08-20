//! 指令栈效应表与测试期平衡检查（零运行时开销）。
//!
//! - `Instruction::stack_effect`：纯数据，供 verifier / 文档使用，不进解释器热循环。
//! - `verify_stack_balance`：CFG 工作表分析，仅在测试或显式调用时运行。
//!
//! Call 族按「同步调用」近似（pop args+callee, push 1），与 specialize 一致；
//! 用户函数的延迟返回不改变调用方净效应。

use crate::opcode::Instruction;

/// 单条指令对操作数栈深度的效应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackEffect {
    /// `depth = depth - pop + push`；若有 `alt_push`，另探索 `depth - pop + alt_push`。
    Adjust {
        pop: u16,
        push: u16,
        alt_push: Option<u16>,
    },
    /// 无条件跳转：深度不变，无顺序后继。
    Jump { target: usize },
    /// 条件跳转：先 pop，再同时走 fallthrough 与 target。
    CondJump { pop: u16, target: usize },
    /// 计数循环：继续路径净 0（计数器仍在）；退出路径 pop 1 后跳到 target。
    LoopCountdown { target: usize },
    /// 终止本路径（Ret / Throw / Rethrow）。
    Exit { pop: u16 },
    /// `EnterTry`：顺序后继；同时以**当前深度**登记 catch 入口（抛出时栈回滚至此）。
    EnterTry {
        catch_label: usize,
        else_label: usize,
        end_label: usize,
    },
    /// `EndTry`：成功离开 try，跳到 else（非 0）或 end；同时弹出路径上的 `EnterTry`。
    EndTry,
    /// `PopTry`：关闭路径上最近的 EnterTry（Handle / catch 清理），不改栈深。
    PopTry,
}

impl Instruction {
    /// 本指令的栈效应（紧化后的绝对 PC 跳转目标）。
    ///
    /// 不分配、可内联；仅供静态分析，不影响执行。
    #[inline]
    #[must_use]
    pub const fn stack_effect(&self) -> StackEffect {
        use StackEffect::{Adjust, PopTry, Exit, Jump, CondJump, LoopCountdown, EnterTry, EndTry};
        match self {
            Self::Push(_) | Self::PushSmall(_) => Adjust {
                pop: 0,
                push: 1,
                alt_push: None,
            },
            Self::Pop => Adjust {
                pop: 1,
                push: 0,
                alt_push: None,
            },
            Self::Add
            | Self::AddNumNum
            | Self::AddTextText
            | Self::AddListList
            | Self::Sub
            | Self::SubNumNum
            | Self::Mul
            | Self::MulNumNum
            | Self::Div
            | Self::DivNumNum
            | Self::Mod
            | Self::ModNumNum
            | Self::Pow
            | Self::PowNumNum
            | Self::BitAnd
            | Self::BitOr
            | Self::BitXor
            | Self::LShift
            | Self::RShift
            | Self::And
            | Self::Or
            | Self::Eq
            | Self::EqNumNum
            | Self::Ne
            | Self::NeNumNum
            | Self::Lt
            | Self::LtNumNum
            | Self::Le
            | Self::LeNumNum
            | Self::Gt
            | Self::GtNumNum
            | Self::Ge
            | Self::GeNumNum
            | Self::In
            | Self::Is
            | Self::IsNot
            | Self::MatchEq
            | Self::Index => Adjust {
                pop: 2,
                push: 1,
                alt_push: None,
            },
            Self::SelectTrySend => Adjust {
                pop: 2,
                push: 1,
                alt_push: None,
            },
            Self::Neg
            | Self::Invert
            | Self::Not
            | Self::TruthyNot
            | Self::GetAttr(_)
            | Self::IsList
            | Self::ListLen
            | Self::IsInstance(_)
            | Self::GoValue
            | Self::Await
            | Self::Snap
            | Self::MakeDeadline
            | Self::SelectPollDeadline => Adjust {
                pop: 1,
                push: 1,
                alt_push: None,
            },
            Self::Load(_)
            | Self::LoadGlobal(_)
            | Self::LoadMacro(_)
            | Self::NewVarOrLoad(_)
            | Self::LoadFast(_)
            | Self::LoadFastSubImm { .. }
            | Self::LoadFastLeImm { .. }
            | Self::FindMod(_)
            | Self::FindModFile(_)
            | Self::PushExc
            | Self::ExcMatch(_) => Adjust {
                pop: 0,
                push: 1,
                alt_push: None,
            },
            Self::Store(_)
            | Self::StoreGlobal(_)
            | Self::StoreFast(_)
            | Self::BindFast { .. }
            | Self::DelName(_)
            | Self::DelAttr(_)
            | Self::Yield
            | Self::YieldFrom => Adjust {
                pop: 1,
                push: 0,
                alt_push: None,
            },
            Self::EnterScope
            | Self::LeaveScope
            | Self::Label(_)
            | Self::TypeCheck
            | Self::ResolveFuncTypes
            | Self::RegisterExport(_)
            | Self::Suspend
            | Self::IterEnd => Adjust {
                pop: 0,
                push: 0,
                alt_push: None,
            },
            Self::NewVar { .. } => Adjust {
                pop: 0,
                push: 0,
                alt_push: None,
            },
            Self::PopTry => PopTry,
            Self::Call { argc }
            | Self::CallSelf { argc }
            | Self::MacroCall { argc } => Adjust {
                pop: (*argc as u16).saturating_add(1),
                push: 1,
                alt_push: None,
            },
            // callee 已编码在指令里，只弹参数。
            Self::CallGlobal { argc, .. } => Adjust {
                pop: *argc as u16,
                push: 1,
                alt_push: None,
            },
            Self::CallList => Adjust {
                pop: 2,
                push: 1,
                alt_push: None,
            },
            Self::CallEx => Adjust {
                pop: 3,
                push: 1,
                alt_push: None,
            },
            Self::GoCall(argc) => Adjust {
                pop: (*argc as u16).saturating_add(1),
                push: 1,
                alt_push: None,
            },
            Self::ListAppend | Self::ListExtend | Self::SetAdd => Adjust {
                pop: 2,
                push: 1,
                alt_push: None,
            },
            Self::DictSet => Adjust {
                pop: 3,
                push: 1,
                alt_push: None,
            },
            // Ret 族不弹栈：栈顶即返回值（空栈时 VM 补 none）。
            Self::Ret | Self::RetLeave | Self::RetFast(_) => Exit { pop: 0 },
            Self::VecNew(n) | Self::SetNew(n) | Self::TupleNew(n) => Adjust {
                pop: *n as u16,
                push: 1,
                alt_push: None,
            },
            Self::DictNew(n) => Adjust {
                pop: (*n as u16).saturating_mul(2),
                push: 1,
                alt_push: None,
            },
            Self::IndexSet => Adjust {
                pop: 3,
                push: 0,
                alt_push: None,
            },
            Self::SliceGet => Adjust {
                pop: 4,
                push: 1,
                alt_push: None,
            },
            Self::SliceSet => Adjust {
                pop: 5,
                push: 0,
                alt_push: None,
            },
            Self::DelIndex => Adjust {
                pop: 2,
                push: 0,
                alt_push: None,
            },
            Self::StructNew { argc, .. } => Adjust {
                pop: *argc as u16,
                push: 1,
                alt_push: None,
            },
            Self::VariantNew { .. } | Self::IterNew => Adjust {
                pop: 1,
                push: 1,
                alt_push: None,
            },
            Self::SetField(_) => Adjust {
                pop: 2,
                push: 0,
                alt_push: None,
            },
            // ready: value+bool；not ready: 仅 bool
            Self::SelectTryRecv | Self::SelectPollTask => Adjust {
                pop: 1,
                push: 1,
                alt_push: Some(2),
            },
            Self::SelectIdle(n) => Adjust {
                pop: *n as u16,
                push: 0,
                alt_push: None,
            },
            Self::SelectBegin(_) => Adjust {
                pop: 0,
                push: 0,
                alt_push: None,
            },
            Self::SelectNextIndex => Adjust {
                pop: 0,
                push: 1,
                alt_push: None,
            },
            Self::IterNext => Adjust {
                pop: 0,
                push: 1,
                alt_push: Some(2),
            },
            Self::UnpackExact(n) => Adjust {
                pop: 1,
                push: *n as u16,
                alt_push: None,
            },
            Self::UnpackRest { before, after } => Adjust {
                pop: 1,
                push: (*before as u16)
                    .saturating_add(1)
                    .saturating_add(*after as u16),
                alt_push: None,
            },
            Self::Throw => Exit { pop: 1 },
            Self::Rethrow => Exit { pop: 0 },
            Self::Goto(t) => Jump { target: *t },
            Self::GotoIf(t) | Self::GotoIfNot(t) => CondJump {
                pop: 1,
                target: *t,
            },
            Self::LoopCountdown(t) => LoopCountdown { target: *t },
            Self::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => EnterTry {
                catch_label: *catch_label,
                else_label: *else_label,
                end_label: *end_label,
            },
            Self::EndTry => EndTry,
        }
    }
}

/// 检查字节码操作数栈深度永不下溢，且控制流汇合点深度一致。
///
/// 供测试使用；不在解释器热路径调用。
///
/// 可变效应指令（`SelectTryRecv` / `SelectPollTask` / `IterNext`）须紧跟
/// `GotoIf`/`GotoIfNot`：ready 路径多压一个值，由条件跳转分流，不能对同一
/// 后继合并两个深度。
///
/// `EnterTry` 沿控制流路径压栈；`PopTry`/`EndTry` 弹出。Handle 表达式用
/// `Goto`+`PopTry` 关闭而无 `EndTry`，不得被后续 `EndTry` 误匹配。
pub fn verify_stack_balance(code: &[Instruction]) -> Result<(), String> {
    let n = code.len();
    if n == 0 {
        return Ok(());
    }

    // 每个 PC：首次到达的 (depth, try_stack)；再访须完全一致。
    let mut state_at: Vec<Option<(u32, Vec<usize>)>> = vec![None; n];
    let mut enter_try_meta: Vec<Option<(usize, usize, usize)>> = vec![None; n];
    // (pc, depth, try_stack = EnterTry 指令 PC 栈)
    let mut work: Vec<(usize, u32, Vec<usize>)> = vec![(0, 0, Vec::new())];

    while let Some((pc, depth, try_stack)) = work.pop() {
        if pc >= n {
            continue;
        }
        if let Some((prev_d, prev_ts)) = &state_at[pc] {
            if *prev_d != depth || *prev_ts != try_stack {
                return Err(format!(
                    "stack/try-stack mismatch at pc={pc}: depth {prev_d}/{depth}, try_stack {prev_ts:?}/{try_stack:?} (ins={:?})",
                    code.get(pc)
                ));
            }
            continue;
        }
        state_at[pc] = Some((depth, try_stack.clone()));

        match code[pc].stack_effect() {
            StackEffect::Adjust {
                pop,
                push,
                alt_push: Some(alt),
            } => {
                if depth < u32::from(pop) {
                    return Err(format!(
                        "stack underflow at pc={pc}: depth={depth} pop={pop} (ins={:?})",
                        code[pc]
                    ));
                }
                let base = depth - u32::from(pop);
                let d_lo = base + u32::from(push);
                let d_hi = base + u32::from(alt);
                let Some(next_ins) = code.get(pc + 1) else {
                    return Err(format!(
                        "variable-effect op at pc={pc} needs a following CondJump"
                    ));
                };
                match next_ins {
                    Instruction::GotoIfNot(target) => {
                        if d_lo < 1 || d_hi < 1 {
                            return Err(format!(
                                "stack underflow before GotoIfNot after pc={pc}"
                            ));
                        }
                        work.push((pc + 2, d_hi - 1, try_stack.clone()));
                        work.push((*target, d_lo - 1, try_stack));
                    }
                    Instruction::GotoIf(target) => {
                        if d_lo < 1 || d_hi < 1 {
                            return Err(format!(
                                "stack underflow before GotoIf after pc={pc}"
                            ));
                        }
                        work.push((pc + 2, d_lo - 1, try_stack.clone()));
                        work.push((*target, d_hi - 1, try_stack));
                    }
                    _ => {
                        return Err(format!(
                            "variable-effect op at pc={pc} must be followed by GotoIf/GotoIfNot, got {next_ins:?}"
                        ));
                    }
                }
            }
            StackEffect::Adjust {
                pop,
                push,
                alt_push: _none,
            } => {
                if depth < u32::from(pop) {
                    return Err(format!(
                        "stack underflow at pc={pc}: depth={depth} pop={pop} (ins={:?})",
                        code[pc]
                    ));
                }
                work.push((
                    pc + 1,
                    depth - u32::from(pop) + u32::from(push),
                    try_stack,
                ));
            }
            StackEffect::Jump { target } => {
                work.push((target, depth, try_stack));
            }
            StackEffect::CondJump { pop, target } => {
                if depth < u32::from(pop) {
                    return Err(format!(
                        "stack underflow at pc={pc}: depth={depth} pop={pop} (ins={:?})",
                        code[pc]
                    ));
                }
                let d = depth - u32::from(pop);
                work.push((pc + 1, d, try_stack.clone()));
                work.push((target, d, try_stack));
            }
            StackEffect::LoopCountdown { target } => {
                work.push((pc + 1, depth, try_stack.clone()));
                if depth < 1 {
                    return Err(format!(
                        "stack underflow at pc={pc}: LoopCountdown with depth=0"
                    ));
                }
                work.push((target, depth - 1, try_stack));
            }
            StackEffect::Exit { pop } => {
                if depth < u32::from(pop) {
                    return Err(format!(
                        "stack underflow at pc={pc}: depth={depth} pop={pop} (ins={:?})",
                        code[pc]
                    ));
                }
            }
            StackEffect::EnterTry {
                catch_label,
                else_label,
                end_label,
            } => {
                enter_try_meta[pc] = Some((catch_label, else_label, end_label));
                let mut inner = try_stack;
                inner.push(pc);
                work.push((catch_label, depth, inner.clone()));
                work.push((pc + 1, depth, inner));
            }
            StackEffect::PopTry => {
                let mut ts = try_stack;
                if ts.pop().is_none() {
                    return Err(format!("PopTry without EnterTry at pc={pc}"));
                }
                work.push((pc + 1, depth, ts));
            }
            StackEffect::EndTry => {
                let Some(enter_pc) = try_stack.last().copied() else {
                    return Err(format!("EndTry without EnterTry at pc={pc}"));
                };
                let Some((_catch, else_l, end_l)) = enter_try_meta[enter_pc] else {
                    return Err(format!(
                        "EndTry at pc={pc}: missing meta for EnterTry pc={enter_pc}"
                    ));
                };
                let mut ts = try_stack;
                ts.pop();
                let target = if else_l != 0 { else_l } else { end_l };
                work.push((target, depth, ts));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::Instruction;
    use crate::value::Value;

    #[test]
    fn suspend_then_push_none_is_balanced_with_pop() {
        let code = vec![
            Instruction::Suspend,
            Instruction::Push(Value::None),
            Instruction::Pop,
            Instruction::PushSmall(1),
            Instruction::Ret,
        ];
        verify_stack_balance(&code).expect("balanced");
    }

    #[test]
    fn bare_suspend_then_pop_underflows() {
        let code = vec![
            Instruction::Suspend,
            Instruction::Pop, // 失衡：Suspend 不压值
            Instruction::Ret,
        ];
        let err = verify_stack_balance(&code).unwrap_err();
        assert!(err.contains("underflow"), "err={err}");
    }

    #[test]
    fn handle_poptry_closes_enter_try_on_path() {
        // Handle：EnterTry … Goto end; catch; end; PopTry（无 EndTry）
        let code = vec![
            Instruction::EnterTry {
                catch_label: 3,
                else_label: 0,
                end_label: 4,
            },
            Instruction::PushSmall(1),
            Instruction::Goto(4),
            Instruction::Push(Value::None),
            Instruction::PopTry,
            Instruction::Ret,
        ];
        verify_stack_balance(&code).expect("handle PopTry balanced");
    }

    #[test]
    fn endtry_uses_path_enter_not_prior_handle() {
        // 先 Handle（PopTry 关闭），再真正的 try/EndTry，路径栈不得串台。
        let code = vec![
            Instruction::EnterTry {
                catch_label: 3,
                else_label: 0,
                end_label: 4,
            },
            Instruction::PushSmall(1),
            Instruction::Goto(4),
            Instruction::Push(Value::None),
            Instruction::PopTry,
            Instruction::EnterTry {
                catch_label: 8,
                else_label: 0,
                end_label: 11,
            },
            Instruction::PushSmall(2),
            Instruction::EndTry,
            Instruction::PopTry,
            Instruction::PushSmall(0),
            Instruction::Goto(11),
            Instruction::Ret,
        ];
        verify_stack_balance(&code).expect("handle then try EndTry balanced");
    }
}
