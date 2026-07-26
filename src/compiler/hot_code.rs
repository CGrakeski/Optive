//! 紧凑热操作码：与 `Instruction` 等长并行，热循环只 match `u8`，避开巨型枚举分发。

use std::rc::Rc;

use crate::opcode::Instruction;

pub const H_PUSH_SMALL: u8 = 0;
pub const H_LOAD_FAST: u8 = 1;
pub const H_ADD_NUM: u8 = 2;
pub const H_SUB_NUM: u8 = 3;
pub const H_MUL_NUM: u8 = 4;
pub const H_DIV_NUM: u8 = 5;
pub const H_LE: u8 = 6;
pub const H_LT: u8 = 7;
pub const H_GT: u8 = 8;
pub const H_GE: u8 = 9;
pub const H_EQ: u8 = 10;
pub const H_NE: u8 = 11;
pub const H_GOTO: u8 = 12;
pub const H_GOTO_IF: u8 = 13;
pub const H_GOTO_IF_NOT: u8 = 14;
pub const H_CALL_SELF: u8 = 15;
pub const H_RET: u8 = 16;
pub const H_RET_LEAVE: u8 = 17;
pub const H_RET_FAST: u8 = 18;
pub const H_LABEL: u8 = 19;
pub const H_ADD_TEXT: u8 = 20;
pub const H_ADD_LIST: u8 = 21;
pub const H_STORE_FAST: u8 = 22;
pub const H_COLD: u8 = 255;

#[derive(Clone, Default)]
pub struct HotCode {
    pub ops: Rc<[u8]>,
    pub args: Rc<[i64]>,
}

impl HotCode {
    pub fn empty() -> Self {
        Self {
            ops: Rc::from([]),
            args: Rc::from([]),
        }
    }

    pub fn encode(code: &[Instruction]) -> Self {
        let mut ops = Vec::with_capacity(code.len());
        let mut args = Vec::with_capacity(code.len());
        for ins in code {
            let (op, arg) = match ins {
                Instruction::PushSmall(n) => (H_PUSH_SMALL, *n),
                Instruction::LoadFast(s) => (H_LOAD_FAST, *s as i64),
                Instruction::StoreFast(s) => (H_STORE_FAST, *s as i64),
                Instruction::Add | Instruction::AddNumNum => (H_ADD_NUM, 0),
                Instruction::Sub | Instruction::SubNumNum => (H_SUB_NUM, 0),
                Instruction::MulNumNum => (H_MUL_NUM, 0),
                Instruction::DivNumNum => (H_DIV_NUM, 0),
                Instruction::Le | Instruction::LeNumNum => (H_LE, 0),
                Instruction::LtNumNum => (H_LT, 0),
                Instruction::GtNumNum => (H_GT, 0),
                Instruction::GeNumNum => (H_GE, 0),
                Instruction::EqNumNum => (H_EQ, 0),
                Instruction::NeNumNum => (H_NE, 0),
                Instruction::Goto(t) => (H_GOTO, *t as i64),
                Instruction::GotoIf(t) => (H_GOTO_IF, *t as i64),
                Instruction::GotoIfNot(t) => (H_GOTO_IF_NOT, *t as i64),
                Instruction::CallSelf { argc } => (H_CALL_SELF, *argc as i64),
                Instruction::Ret => (H_RET, 0),
                Instruction::RetLeave => (H_RET_LEAVE, 0),
                Instruction::RetFast(s) => (H_RET_FAST, *s as i64),
                Instruction::Label(_) => (H_LABEL, 0),
                Instruction::AddTextText => (H_ADD_TEXT, 0),
                Instruction::AddListList => (H_ADD_LIST, 0),
                _ => (H_COLD, 0),
            };
            ops.push(op);
            args.push(arg);
        }
        Self {
            ops: Rc::from(ops),
            args: Rc::from(args),
        }
    }
}
