//! 紧凑热操作码：与 `Instruction` 等长并行，热循环只 match `u8`，避开巨型枚举分发。

use std::sync::Arc;

use crate::opcode::Instruction;
use crate::value::{Num, Value};

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
pub const H_LOOP_COUNTDOWN: u8 = 23;
pub const H_LOAD_FAST_SUB_IMM: u8 = 24;
pub const H_LOAD_FAST_LE_IMM: u8 = 25;
pub const H_MOD_NUM: u8 = 26;
pub const H_LOAD_GLOBAL: u8 = 27;
pub const H_STORE_GLOBAL: u8 = 28;
pub const H_CALL: u8 = 29;
pub const H_CALL_GLOBAL: u8 = 30;
pub const H_PUSH_BOOL: u8 = 31;
pub const H_LOAD_FAST_LT_IMM: u8 = 32;
pub const H_LOAD_FAST_GT_IMM: u8 = 33;
pub const H_LOAD_FAST_EQ_IMM: u8 = 34;
pub const H_LOAD_FAST_ADD_IMM_STORE: u8 = 35;
pub const H_LOAD_FAST_SQR_GT: u8 = 36;
pub const H_LOAD_FAST_MOD_EQ0: u8 = 37;
pub const H_COLD: u8 = 255;

#[derive(Clone, Default)]
pub struct HotCode {
    pub ops: Arc<[u8]>,
    pub args: Arc<[i64]>,
}

impl HotCode {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ops: Arc::from([]),
            args: Arc::from([]),
        }
    }

    #[must_use]
    pub fn encode(code: &[Instruction]) -> Self {
        let mut ops = Vec::with_capacity(code.len());
        let mut args = Vec::with_capacity(code.len());
        for ins in code {
            let (op, arg) = match ins {
                Instruction::PushSmall(n) => (H_PUSH_SMALL, *n),
                Instruction::LoadFast(s) => (H_LOAD_FAST, *s as i64),
                Instruction::StoreFast(s) => (H_STORE_FAST, *s as i64),
                Instruction::LoadGlobal(s) => (H_LOAD_GLOBAL, *s as i64),
                Instruction::StoreGlobal(s) => (H_STORE_GLOBAL, *s as i64),
                Instruction::LoadFastSubImm { slot, imm } => {
                    (H_LOAD_FAST_SUB_IMM, encode_slot_imm(*slot, *imm))
                }
                Instruction::LoadFastLeImm { slot, imm } => {
                    (H_LOAD_FAST_LE_IMM, encode_slot_imm(*slot, *imm))
                }
                Instruction::Add | Instruction::AddNumNum => (H_ADD_NUM, 0),
                Instruction::Sub | Instruction::SubNumNum => (H_SUB_NUM, 0),
                Instruction::Mul | Instruction::MulNumNum => (H_MUL_NUM, 0),
                Instruction::Div | Instruction::DivNumNum => (H_DIV_NUM, 0),
                Instruction::Mod | Instruction::ModNumNum => (H_MOD_NUM, 0),
                Instruction::Le | Instruction::LeNumNum => (H_LE, 0),
                Instruction::Lt | Instruction::LtNumNum => (H_LT, 0),
                Instruction::Gt | Instruction::GtNumNum => (H_GT, 0),
                Instruction::Ge | Instruction::GeNumNum => (H_GE, 0),
                Instruction::Eq | Instruction::EqNumNum => (H_EQ, 0),
                Instruction::Ne | Instruction::NeNumNum => (H_NE, 0),
                Instruction::Push(Value::Bool(b)) => (H_PUSH_BOOL, i64::from(*b)),
                Instruction::BindFast {
                    slot,
                    is_const: false,
                    ..
                } => (H_STORE_FAST, *slot as i64),
                Instruction::LoadFastLtImm { slot, imm } => {
                    (H_LOAD_FAST_LT_IMM, encode_slot_imm(*slot, *imm))
                }
                Instruction::LoadFastGtImm { slot, imm } => {
                    (H_LOAD_FAST_GT_IMM, encode_slot_imm(*slot, *imm))
                }
                Instruction::LoadFastEqImm { slot, imm } => {
                    (H_LOAD_FAST_EQ_IMM, encode_slot_imm(*slot, *imm))
                }
                Instruction::LoadFastAddImmStore { slot, imm } => {
                    (H_LOAD_FAST_ADD_IMM_STORE, encode_slot_imm(*slot, *imm))
                }
                Instruction::LoadFastSqrGt { sqr_slot, rhs_slot } => {
                    (H_LOAD_FAST_SQR_GT, encode_two_slots(*sqr_slot, *rhs_slot))
                }
                Instruction::LoadFastModEq0 { lhs_slot, rhs_slot } => {
                    (H_LOAD_FAST_MOD_EQ0, encode_two_slots(*lhs_slot, *rhs_slot))
                }
                Instruction::Goto(t) => (H_GOTO, *t as i64),
                Instruction::GotoIf(t) => (H_GOTO_IF, *t as i64),
                Instruction::GotoIfNot(t) => (H_GOTO_IF_NOT, *t as i64),
                Instruction::LoopCountdown(t) => (H_LOOP_COUNTDOWN, *t as i64),
                Instruction::CallSelf { argc } => (H_CALL_SELF, *argc as i64),
                Instruction::Call { argc } => (H_CALL, *argc as i64),
                Instruction::CallGlobal { global_idx, argc } => {
                    (H_CALL_GLOBAL, encode_slot_imm(*global_idx, *argc as i64))
                }
                Instruction::Ret => (H_RET, 0),
                Instruction::RetLeave => (H_RET_LEAVE, 0),
                Instruction::RetFast(s) => (H_RET_FAST, *s as i64),
                Instruction::Label(_) => (H_LABEL, 0),
                Instruction::AddTextText => (H_ADD_TEXT, 0),
                Instruction::AddListList => (H_ADD_LIST, 0),
                // 兜底：完整 Push(Small) 仍走热路径，避免漏改 codegen 再掉进冷分发。
                Instruction::Push(Value::Num(Num::Small(n))) => (H_PUSH_SMALL, *n),
                _ => (H_COLD, 0),
            };
            ops.push(op);
            args.push(arg);
        }
        Self {
            ops: Arc::from(ops),
            args: Arc::from(args),
        }
    }
}

/// 将 `(slot, imm)` 打包为单个 `i64` 热操作数。slot 占低 32 位，imm 占高 32 位。
/// 调用方需保证 `slot < 2^32` 且 `imm` 落在 `i32` 范围内。
#[inline(always)]
#[must_use]
pub fn encode_slot_imm(slot: usize, imm: i64) -> i64 {
    i64::from(slot as u32) | (i64::from(imm as i32) << 32)
}

/// 解包 `encode_slot_imm` 的结果。
#[inline(always)]
#[must_use]
pub fn decode_slot_imm(arg: i64) -> (usize, i64) {
    let slot = (arg & 0xFFFF_FFFF) as u32 as usize;
    let imm = i64::from((arg >> 32) as i32);
    (slot, imm)
}

/// 打包两个快局部槽（均须 `< 2^32`）。
#[inline(always)]
#[must_use]
pub fn encode_two_slots(a: usize, b: usize) -> i64 {
    encode_slot_imm(a, b as i64)
}

/// 解包 `encode_two_slots`。
#[inline(always)]
#[must_use]
pub fn decode_two_slots(arg: i64) -> (usize, usize) {
    let (a, b) = decode_slot_imm(arg);
    (a, b as usize)
}
