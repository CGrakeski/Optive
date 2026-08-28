//! 按源码哈希落盘编译结果（不复用 `Optive.cache` 的 tip/id 语义）。
//!
//! 仅缓存「可编码」程序：无 struct/enum/macro/泛型模板。加载后仍跑
//! `validate_hot_bytecode`。格式对不上则丢弃。

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::ast::FuncParam;
use crate::hot_code::HotCode;
use crate::opcode::{CompiledProgram, FuncFlags, FunctionObject, Instruction, ModuleGlobalEnv};
use crate::shared::SyncCell;
use crate::value::{Num, Value};
use crate::Result;

const MAGIC: &[u8; 4] = b"TIVC";
const FORMAT: u16 = crate::versions::BYTECODE_FORMAT_VERSION;

static STORES: AtomicU64 = AtomicU64::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn stats() -> (u64, u64) {
    (STORES.load(Ordering::Relaxed), HITS.load(Ordering::Relaxed))
}

pub fn cache_enabled() -> bool {
    match std::env::var("OPTIVE_BC_CACHE") {
        Ok(v) => {
            let t = v.trim();
            !(t == "0" || t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("false"))
        }
        Err(_) => true,
    }
}

/// 真实源文件才落盘。`<repl>` / `<script>` / 测试标签一律跳过，避免污染与竞态。
#[must_use]
pub fn should_use(file: &str) -> bool {
    cache_enabled() && !(file.starts_with('<') && file.ends_with('>'))
}

pub fn reset_stats() {
    STORES.store(0, Ordering::Relaxed);
    HITS.store(0, Ordering::Relaxed);
}

pub fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("OPTIVE_BC_DIR") {
        let p = PathBuf::from(p);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Ok(h) = std::env::var("OPTIVE_HOME") {
        let h = h.trim();
        if !h.is_empty() {
            return PathBuf::from(h).join("bc");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".optive")
        .join("bc")
}

pub fn key(version: &str, file: &str, source: &str, dep_ids: &str) -> String {
    let mut h = Sha256::new();
    h.update(version.as_bytes());
    h.update([0]);
    h.update(file.as_bytes());
    h.update([0]);
    h.update(source.as_bytes());
    h.update([0]);
    h.update(dep_ids.as_bytes());
    hex::encode(h.finalize())
}

pub fn load(path: &Path) -> Option<CompiledProgram> {
    let bytes = fs::read(path).ok()?;
    decode(&bytes).ok()
}

pub fn store(path: &Path, prog: &CompiledProgram) -> bool {
    let Ok(bytes) = encode(prog) else {
        return false;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, bytes).is_ok()
}

pub fn note_store() {
    STORES.fetch_add(1, Ordering::Relaxed);
}

pub fn note_hit() {
    HITS.fetch_add(1, Ordering::Relaxed);
}

fn encode(prog: &CompiledProgram) -> Result<Vec<u8>> {
    if !prog.macros.is_empty()
        || !prog.struct_defs.is_empty()
        || !prog.enum_defs.is_empty()
        || !prog.variant_defs.is_empty()
        || !prog.overload_tables.is_empty()
        || !prog.protocols.is_empty()
        || !prog.generic_functions.is_empty()
    {
        return Err(crate::error::RuntimeError::msg("uncacheable program shape"));
    }
    let mut w = Vec::new();
    w.extend_from_slice(MAGIC);
    w.extend_from_slice(&FORMAT.to_le_bytes());
    write_str(&mut w, env!("CARGO_PKG_VERSION"));
    write_hot(&mut w, &prog.hot)?;
    write_usizes(&mut w, &prog.line_map);
    write_usizes(&mut w, &prog.column_map);
    write_strs(&mut w, &prog.global_names);
    write_u32(&mut w, prog.script_frame_slots as u32);
    write_u32(&mut w, prog.script_local_to_global.len() as u32);
    for &(loc, glob) in &prog.script_local_to_global {
        write_u32(&mut w, loc as u32);
        write_u32(&mut w, glob as u32);
    }
    write_ins_slice(&mut w, &prog.code)?;
    write_u32(&mut w, prog.functions.len() as u32);
    for (name, f) in &prog.functions {
        write_func(&mut w, name, f)?;
    }
    Ok(w)
}

fn decode(bytes: &[u8]) -> Result<CompiledProgram> {
    let mut r = Cursor::new(bytes);
    let mut mag = [0u8; 4];
    r.read_exact(&mut mag)
        .map_err(|_| crate::error::RuntimeError::msg("truncated bytecode cache"))?;
    if &mag != MAGIC {
        return Err(crate::error::RuntimeError::msg("bad bytecode cache magic"));
    }
    let fmt = read_u16(&mut r)?;
    if fmt != FORMAT {
        return Err(crate::error::RuntimeError::msg("bytecode cache format"));
    }
    let ver = read_str(&mut r)?;
    if ver != env!("CARGO_PKG_VERSION") {
        return Err(crate::error::RuntimeError::msg("bytecode cache version"));
    }
    let hot = read_hot(&mut r)?;
    let line_map = read_usizes(&mut r)?;
    let column_map = read_usizes(&mut r)?;
    let global_names = read_strs(&mut r)?;
    let script_frame_slots = read_u32(&mut r)? as usize;
    let nflush = read_u32(&mut r)? as usize;
    let mut script_local_to_global = Vec::with_capacity(nflush);
    for _ in 0..nflush {
        let loc = read_u32(&mut r)? as usize;
        let glob = read_u32(&mut r)? as usize;
        script_local_to_global.push((loc, glob));
    }
    let code = read_ins_vec(&mut r)?;
    let nfunc = read_u32(&mut r)? as usize;
    let mut functions = HashMap::new();
    for _ in 0..nfunc {
        let (name, f) = read_func(&mut r)?;
        functions.insert(name, Arc::new(f));
    }
    let mut prog = CompiledProgram::new();
    prog.code = code;
    prog.hot = hot;
    prog.line_map = line_map;
    prog.column_map = column_map;
    prog.global_names = global_names;
    prog.script_frame_slots = script_frame_slots;
    prog.script_local_to_global = script_local_to_global;
    prog.functions = functions;
    Ok(prog)
}

fn write_func(w: &mut Vec<u8>, name: &str, f: &FunctionObject) -> Result<()> {
    if f.captured.is_some()
        || f.return_type.is_some()
        || f.return_wrapper.is_some()
        || f.defaults.iter().any(Option::is_some)
    {
        return Err(crate::error::RuntimeError::msg("uncacheable function"));
    }
    write_str(w, name);
    w.push(f.flags.bits());
    write_u32(w, f.entry_pc as u32);
    write_u32(w, f.frame_slots as u32);
    write_u32(w, f.fast_locals as u32);
    write_u32(w, f.entry_label as u32);
    w.extend_from_slice(&f.hot_call_argc.to_le_bytes());
    write_u32(w, f.params.len() as u32);
    for p in &f.params {
        write_str(w, &p.name);
        w.push(u8::from(p.is_variadic));
        w.push(u8::from(p.is_kwvariadic));
        w.push(u8::from(p.implicit));
        w.push(u8::from(p.type_strong));
    }
    write_hot(w, &f.hot)?;
    write_ins_slice(w, &f.body)?;
    write_usizes(w, &f.line_map);
    write_usizes(w, &f.column_map);
    write_opt_u32(w, f.variadic_param_index);
    write_opt_u32(w, f.kwvariadic_param_index);
    write_str(w, &f.source_file);
    match &f.module_env {
        None => w.push(0),
        Some(env) => {
            w.push(1);
            write_strs(w, &env.global_names);
        }
    }
    Ok(())
}

fn read_func(r: &mut Cursor<&[u8]>) -> Result<(String, FunctionObject)> {
    let name = read_str(r)?;
    let flags = FuncFlags::from_bits(read_u8(r)?);
    let entry_pc = read_u32(r)? as usize;
    let frame_slots = read_u32(r)? as usize;
    let fast_locals = read_u32(r)? as usize;
    let entry_label = read_u32(r)? as usize;
    let mut argc_buf = [0u8; 2];
    r.read_exact(&mut argc_buf)
        .map_err(|_| crate::error::RuntimeError::msg("truncated function"))?;
    let hot_call_argc = u16::from_le_bytes(argc_buf);
    let nparams = read_u32(r)? as usize;
    let mut params = Vec::with_capacity(nparams);
    for _ in 0..nparams {
        params.push(FuncParam {
            name: read_str(r)?,
            is_variadic: read_u8(r)? != 0,
            is_kwvariadic: read_u8(r)? != 0,
            implicit: read_u8(r)? != 0,
            type_expr: None,
            type_strong: read_u8(r)? != 0,
            default_expr: None,
        });
    }
    let hot = read_hot(r)?;
    let body = read_ins_vec(r)?;
    let line_map = read_usizes(r)?;
    let column_map = read_usizes(r)?;
    let variadic_param_index = read_opt_u32(r)?;
    let kwvariadic_param_index = read_opt_u32(r)?;
    let source_file = read_str(r)?;
    let module_env = if read_u8(r)? == 0 {
        None
    } else {
        let names = read_strs(r)?;
        Some(Arc::new(ModuleGlobalEnv {
            global_names: names,
            globals: Arc::new(SyncCell::new(HashMap::new())),
            finalized: false,
        }))
    };
    let n = params.len();
    Ok((
        name.clone(),
        FunctionObject {
            hot,
            entry_pc,
            frame_slots,
            fast_locals,
            entry_label,
            hot_call_argc,
            flags,
            name,
            params,
            body: Arc::new(body),
            line_map: Arc::new(line_map),
            column_map: Arc::new(column_map),
            variadic_param_index,
            kwvariadic_param_index,
            defaults: vec![None; n],
            captured: None,
            return_type: None,
            return_wrapper: None,
            param_types: Vec::new(),
            return_type_value: None,
            module_env,
            source: None,
            source_file,
        },
    ))
}

fn write_hot(w: &mut Vec<u8>, hot: &HotCode) -> Result<()> {
    write_u32(w, hot.ops.len() as u32);
    w.extend_from_slice(&hot.ops);
    write_u32(w, hot.args.len() as u32);
    for a in hot.args.iter() {
        w.extend_from_slice(&a.to_le_bytes());
    }
    Ok(())
}

fn read_hot(r: &mut Cursor<&[u8]>) -> Result<HotCode> {
    let n = read_u32(r)? as usize;
    let mut ops = vec![0u8; n];
    r.read_exact(&mut ops)
        .map_err(|_| crate::error::RuntimeError::msg("truncated hot ops"))?;
    let m = read_u32(r)? as usize;
    let mut args = Vec::with_capacity(m);
    for _ in 0..m {
        args.push(read_i64(r)?);
    }
    Ok(HotCode {
        ops: ops.into(),
        args: args.into(),
    })
}

fn write_ins_slice(w: &mut Vec<u8>, code: &[Instruction]) -> Result<()> {
    write_u32(w, code.len() as u32);
    for ins in code {
        encode_ins(w, ins)?;
    }
    Ok(())
}

fn read_ins_vec(r: &mut Cursor<&[u8]>) -> Result<Vec<Instruction>> {
    let n = read_u32(r)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(decode_ins(r)?);
    }
    Ok(out)
}

fn encode_value(w: &mut Vec<u8>, v: &Value) -> Result<()> {
    match v {
        Value::None => w.push(0),
        Value::Bool(b) => {
            w.push(1);
            w.push(u8::from(*b));
        }
        Value::Num(Num::Small(n)) => {
            w.push(2);
            w.extend_from_slice(&n.to_le_bytes());
        }
        Value::Text(s) => {
            w.push(3);
            write_str(w, s);
        }
        Value::TypeRef(s) => {
            w.push(4);
            write_str(w, s);
        }
        Value::Bytes(b) => {
            w.push(5);
            write_u32(w, b.len() as u32);
            w.extend_from_slice(b);
        }
        Value::Function(f) => {
            w.push(6);
            write_func(w, &f.name, f)?;
        }
        _ => {
            return Err(crate::error::RuntimeError::msg(
                "uncacheable constant value",
            ));
        }
    }
    Ok(())
}

fn decode_value(r: &mut Cursor<&[u8]>) -> Result<Value> {
    match read_u8(r)? {
        0 => Ok(Value::None),
        1 => Ok(Value::Bool(read_u8(r)? != 0)),
        2 => Ok(Value::Num(Num::Small(read_i64(r)?))),
        3 => Ok(Value::Text(read_str(r)?)),
        4 => Ok(Value::TypeRef(read_str(r)?)),
        5 => {
            let n = read_u32(r)? as usize;
            let mut b = vec![0u8; n];
            r.read_exact(&mut b)
                .map_err(|_| crate::error::RuntimeError::msg("truncated bytes const"))?;
            Ok(Value::Bytes(Arc::new(b)))
        }
        6 => {
            let (_name, f) = read_func(r)?;
            Ok(Value::Function(Arc::new(f)))
        }
        _ => Err(crate::error::RuntimeError::msg("bad cached value tag")),
    }
}

fn encode_ins(w: &mut Vec<u8>, ins: &Instruction) -> Result<()> {
    use Instruction::*;
    let unit = |w: &mut Vec<u8>, t: u8| w.push(t);
    match ins {
        Push(v) => {
            w.push(1);
            encode_value(w, v)?;
        }
        PushSmall(n) => {
            w.push(2);
            w.extend_from_slice(&n.to_le_bytes());
        }
        Pop => unit(w, 3),
        Add => unit(w, 4),
        AddNumNum => unit(w, 5),
        AddTextText => unit(w, 6),
        AddListList => unit(w, 7),
        Sub => unit(w, 8),
        SubNumNum => unit(w, 9),
        Mul => unit(w, 10),
        MulNumNum => unit(w, 11),
        Div => unit(w, 12),
        DivNumNum => unit(w, 13),
        Mod => unit(w, 14),
        ModNumNum => unit(w, 15),
        Pow => unit(w, 16),
        PowNumNum => unit(w, 17),
        BitAnd => unit(w, 18),
        BitOr => unit(w, 19),
        BitXor => unit(w, 20),
        LShift => unit(w, 21),
        RShift => unit(w, 22),
        Neg => unit(w, 23),
        Invert => unit(w, 24),
        Not => unit(w, 25),
        TruthyNot => unit(w, 26),
        And => unit(w, 27),
        Or => unit(w, 28),
        Eq => unit(w, 29),
        EqNumNum => unit(w, 30),
        Ne => unit(w, 31),
        NeNumNum => unit(w, 32),
        Lt => unit(w, 33),
        LtNumNum => unit(w, 34),
        Le => unit(w, 35),
        LeNumNum => unit(w, 36),
        Gt => unit(w, 37),
        GtNumNum => unit(w, 38),
        Ge => unit(w, 39),
        GeNumNum => unit(w, 40),
        In => unit(w, 41),
        Is => unit(w, 42),
        IsNot => unit(w, 43),
        Load(s) => {
            w.push(44);
            write_str(w, s);
        }
        LoadGlobal(i) => {
            w.push(45);
            write_u32(w, *i as u32);
        }
        LoadMacro(_) | MacroCall { .. } => {
            return Err(crate::error::RuntimeError::msg("uncacheable macro op"));
        }
        Store(s) => {
            w.push(46);
            write_str(w, s);
        }
        StoreGlobal(i) => {
            w.push(47);
            write_u32(w, *i as u32);
        }
        NewVar { name, is_const } => {
            w.push(48);
            write_str(w, name);
            w.push(u8::from(*is_const));
        }
        NewVarOrLoad(s) => {
            w.push(49);
            write_str(w, s);
        }
        LoadFast(i) => {
            w.push(50);
            write_u32(w, *i as u32);
        }
        StoreFast(i) => {
            w.push(51);
            write_u32(w, *i as u32);
        }
        LoadFastSubImm { slot, imm } => {
            w.push(52);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastLeImm { slot, imm } => {
            w.push(53);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastLtImm { slot, imm } => {
            w.push(123);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastGtImm { slot, imm } => {
            w.push(124);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastEqImm { slot, imm } => {
            w.push(125);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastAddImmStore { slot, imm } => {
            w.push(126);
            write_u32(w, *slot as u32);
            w.extend_from_slice(&imm.to_le_bytes());
        }
        LoadFastAddStore { dst, src } => {
            w.push(129);
            write_u32(w, *dst as u32);
            write_u32(w, *src as u32);
        }
        LoadFastSqrGt { sqr_slot, rhs_slot } => {
            w.push(127);
            write_u32(w, *sqr_slot as u32);
            write_u32(w, *rhs_slot as u32);
        }
        LoadFastModEq0 { lhs_slot, rhs_slot } => {
            w.push(128);
            write_u32(w, *lhs_slot as u32);
            write_u32(w, *rhs_slot as u32);
        }
        BindFast {
            slot,
            name,
            is_const,
        } => {
            w.push(54);
            write_u32(w, *slot as u32);
            write_str(w, name);
            w.push(u8::from(*is_const));
        }
        EnterScope => unit(w, 55),
        LeaveScope => unit(w, 56),
        Label(i) => {
            w.push(57);
            write_u32(w, *i as u32);
        }
        Goto(i) => {
            w.push(58);
            write_u32(w, *i as u32);
        }
        GotoIf(i) => {
            w.push(59);
            write_u32(w, *i as u32);
        }
        GotoIfNot(i) => {
            w.push(60);
            write_u32(w, *i as u32);
        }
        LoopCountdown(i) => {
            w.push(61);
            write_u32(w, *i as u32);
        }
        Call { argc } => {
            w.push(62);
            write_u32(w, *argc as u32);
        }
        CallGlobal { global_idx, argc } => {
            w.push(63);
            write_u32(w, *global_idx as u32);
            write_u32(w, *argc as u32);
        }
        CallSelf { argc } => {
            w.push(64);
            write_u32(w, *argc as u32);
        }
        CallList => unit(w, 65),
        CallEx => unit(w, 66),
        ListAppend => unit(w, 67),
        ListExtend => unit(w, 68),
        DictSet => unit(w, 69),
        SetAdd => unit(w, 70),
        Ret => unit(w, 71),
        RetFast(i) => {
            w.push(72);
            write_u32(w, *i as u32);
        }
        RetLeave => unit(w, 73),
        VecNew(i) => {
            w.push(74);
            write_u32(w, *i as u32);
        }
        DictNew(i) => {
            w.push(75);
            write_u32(w, *i as u32);
        }
        SetNew(i) => {
            w.push(76);
            write_u32(w, *i as u32);
        }
        TupleNew(i) => {
            w.push(77);
            write_u32(w, *i as u32);
        }
        Index => unit(w, 78),
        IndexSet => unit(w, 79),
        SliceGet => unit(w, 80),
        SliceSet => unit(w, 81),
        DelIndex => unit(w, 82),
        DelName(s) => {
            w.push(83);
            write_str(w, s);
        }
        DelAttr(s) => {
            w.push(84);
            write_str(w, s);
        }
        GetAttr(s) => {
            w.push(85);
            write_str(w, s);
        }
        StructNew { .. } | VariantNew { .. } => {
            return Err(crate::error::RuntimeError::msg("uncacheable type op"));
        }
        SetField(s) => {
            w.push(86);
            write_str(w, s);
        }
        IterNew => unit(w, 87),
        IterNext => unit(w, 88),
        IterEnd => unit(w, 89),
        Throw => unit(w, 90),
        Snap => unit(w, 91),
        PushExc => unit(w, 92),
        EnterTry {
            catch_label,
            else_label,
            end_label,
        } => {
            w.push(93);
            write_u32(w, *catch_label as u32);
            write_u32(w, *else_label as u32);
            write_u32(w, *end_label as u32);
        }
        EndTry => unit(w, 94),
        PopTry => unit(w, 95),
        ExcMatch(s) => {
            w.push(96);
            write_str(w, s);
        }
        IsList => unit(w, 97),
        ListLen => unit(w, 98),
        IsInstance(s) => {
            w.push(99);
            write_str(w, s);
        }
        MatchEq => unit(w, 100),
        UnpackExact(i) => {
            w.push(101);
            write_u32(w, *i as u32);
        }
        UnpackRest { before, after } => {
            w.push(102);
            write_u32(w, *before as u32);
            write_u32(w, *after as u32);
        }
        Rethrow => unit(w, 103),
        TypeCheck => unit(w, 104),
        ResolveFuncTypes => unit(w, 105),
        FindMod(parts) => {
            w.push(106);
            write_strs(w, parts);
        }
        FindModFile(s) => {
            w.push(107);
            write_str(w, s);
        }
        RegisterExport(s) => {
            w.push(108);
            write_str(w, s);
        }
        GoCall(i) => {
            w.push(109);
            write_u32(w, *i as u32);
        }
        GoValue => unit(w, 110),
        Await => unit(w, 111),
        Suspend => unit(w, 112),
        Yield => unit(w, 113),
        YieldFrom => unit(w, 114),
        SelectTryRecv => unit(w, 115),
        SelectTrySend => unit(w, 116),
        SelectPollTask => unit(w, 117),
        MakeDeadline => unit(w, 118),
        SelectPollDeadline => unit(w, 119),
        SelectIdle(i) => {
            w.push(120);
            write_u32(w, *i as u32);
        }
        SelectBegin(i) => {
            w.push(121);
            write_u32(w, *i as u32);
        }
        SelectNextIndex => unit(w, 122),
    }
    Ok(())
}

fn decode_ins(r: &mut Cursor<&[u8]>) -> Result<Instruction> {
    use Instruction::*;
    Ok(match read_u8(r)? {
        1 => Push(decode_value(r)?),
        2 => PushSmall(read_i64(r)?),
        3 => Pop,
        4 => Add,
        5 => AddNumNum,
        6 => AddTextText,
        7 => AddListList,
        8 => Sub,
        9 => SubNumNum,
        10 => Mul,
        11 => MulNumNum,
        12 => Div,
        13 => DivNumNum,
        14 => Mod,
        15 => ModNumNum,
        16 => Pow,
        17 => PowNumNum,
        18 => BitAnd,
        19 => BitOr,
        20 => BitXor,
        21 => LShift,
        22 => RShift,
        23 => Neg,
        24 => Invert,
        25 => Not,
        26 => TruthyNot,
        27 => And,
        28 => Or,
        29 => Eq,
        30 => EqNumNum,
        31 => Ne,
        32 => NeNumNum,
        33 => Lt,
        34 => LtNumNum,
        35 => Le,
        36 => LeNumNum,
        37 => Gt,
        38 => GtNumNum,
        39 => Ge,
        40 => GeNumNum,
        41 => In,
        42 => Is,
        43 => IsNot,
        44 => Load(read_str(r)?),
        45 => LoadGlobal(read_u32(r)? as usize),
        46 => Store(read_str(r)?),
        47 => StoreGlobal(read_u32(r)? as usize),
        48 => NewVar {
            name: read_str(r)?,
            is_const: read_u8(r)? != 0,
        },
        49 => NewVarOrLoad(read_str(r)?),
        50 => LoadFast(read_u32(r)? as usize),
        51 => StoreFast(read_u32(r)? as usize),
        52 => LoadFastSubImm {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        53 => LoadFastLeImm {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        123 => LoadFastLtImm {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        124 => LoadFastGtImm {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        125 => LoadFastEqImm {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        126 => LoadFastAddImmStore {
            slot: read_u32(r)? as usize,
            imm: read_i64(r)?,
        },
        129 => LoadFastAddStore {
            dst: read_u32(r)? as usize,
            src: read_u32(r)? as usize,
        },
        127 => LoadFastSqrGt {
            sqr_slot: read_u32(r)? as usize,
            rhs_slot: read_u32(r)? as usize,
        },
        128 => LoadFastModEq0 {
            lhs_slot: read_u32(r)? as usize,
            rhs_slot: read_u32(r)? as usize,
        },
        54 => BindFast {
            slot: read_u32(r)? as usize,
            name: read_str(r)?,
            is_const: read_u8(r)? != 0,
        },
        55 => EnterScope,
        56 => LeaveScope,
        57 => Label(read_u32(r)? as usize),
        58 => Goto(read_u32(r)? as usize),
        59 => GotoIf(read_u32(r)? as usize),
        60 => GotoIfNot(read_u32(r)? as usize),
        61 => LoopCountdown(read_u32(r)? as usize),
        62 => Call {
            argc: read_u32(r)? as usize,
        },
        63 => CallGlobal {
            global_idx: read_u32(r)? as usize,
            argc: read_u32(r)? as usize,
        },
        64 => CallSelf {
            argc: read_u32(r)? as usize,
        },
        65 => CallList,
        66 => CallEx,
        67 => ListAppend,
        68 => ListExtend,
        69 => DictSet,
        70 => SetAdd,
        71 => Ret,
        72 => RetFast(read_u32(r)? as usize),
        73 => RetLeave,
        74 => VecNew(read_u32(r)? as usize),
        75 => DictNew(read_u32(r)? as usize),
        76 => SetNew(read_u32(r)? as usize),
        77 => TupleNew(read_u32(r)? as usize),
        78 => Index,
        79 => IndexSet,
        80 => SliceGet,
        81 => SliceSet,
        82 => DelIndex,
        83 => DelName(read_str(r)?),
        84 => DelAttr(read_str(r)?),
        85 => GetAttr(read_str(r)?),
        86 => SetField(read_str(r)?),
        87 => IterNew,
        88 => IterNext,
        89 => IterEnd,
        90 => Throw,
        91 => Snap,
        92 => PushExc,
        93 => EnterTry {
            catch_label: read_u32(r)? as usize,
            else_label: read_u32(r)? as usize,
            end_label: read_u32(r)? as usize,
        },
        94 => EndTry,
        95 => PopTry,
        96 => ExcMatch(read_str(r)?),
        97 => IsList,
        98 => ListLen,
        99 => IsInstance(read_str(r)?),
        100 => MatchEq,
        101 => UnpackExact(read_u32(r)? as usize),
        102 => UnpackRest {
            before: read_u32(r)? as usize,
            after: read_u32(r)? as usize,
        },
        103 => Rethrow,
        104 => TypeCheck,
        105 => ResolveFuncTypes,
        106 => FindMod(read_strs(r)?),
        107 => FindModFile(read_str(r)?),
        108 => RegisterExport(read_str(r)?),
        109 => GoCall(read_u32(r)? as usize),
        110 => GoValue,
        111 => Await,
        112 => Suspend,
        113 => Yield,
        114 => YieldFrom,
        115 => SelectTryRecv,
        116 => SelectTrySend,
        117 => SelectPollTask,
        118 => MakeDeadline,
        119 => SelectPollDeadline,
        120 => SelectIdle(read_u32(r)? as usize),
        121 => SelectBegin(read_u32(r)? as usize),
        122 => SelectNextIndex,
        t => {
            return Err(crate::error::RuntimeError::msg(format!(
                "bad cached instruction tag {t}"
            )));
        }
    })
}

fn write_u32(w: &mut Vec<u8>, n: u32) {
    w.extend_from_slice(&n.to_le_bytes());
}
fn write_str(w: &mut Vec<u8>, s: &str) {
    write_u32(w, s.len() as u32);
    w.extend_from_slice(s.as_bytes());
}
fn write_strs(w: &mut Vec<u8>, ss: &[String]) {
    write_u32(w, ss.len() as u32);
    for s in ss {
        write_str(w, s);
    }
}
fn write_usizes(w: &mut Vec<u8>, xs: &[usize]) {
    write_u32(w, xs.len() as u32);
    for x in xs {
        write_u32(w, *x as u32);
    }
}
fn write_opt_u32(w: &mut Vec<u8>, v: Option<usize>) {
    match v {
        None => w.push(0),
        Some(n) => {
            w.push(1);
            write_u32(w, n as u32);
        }
    }
}

fn read_u8(r: &mut Cursor<&[u8]>) -> Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)
        .map_err(|_| crate::error::RuntimeError::msg("truncated cache"))?;
    Ok(b[0])
}
fn read_u16(r: &mut Cursor<&[u8]>) -> Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)
        .map_err(|_| crate::error::RuntimeError::msg("truncated cache"))?;
    Ok(u16::from_le_bytes(b))
}
fn read_u32(r: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)
        .map_err(|_| crate::error::RuntimeError::msg("truncated cache"))?;
    Ok(u32::from_le_bytes(b))
}
fn read_i64(r: &mut Cursor<&[u8]>) -> Result<i64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)
        .map_err(|_| crate::error::RuntimeError::msg("truncated cache"))?;
    Ok(i64::from_le_bytes(b))
}
fn read_str(r: &mut Cursor<&[u8]>) -> Result<String> {
    let n = read_u32(r)? as usize;
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)
        .map_err(|_| crate::error::RuntimeError::msg("truncated string"))?;
    String::from_utf8(b).map_err(|_| crate::error::RuntimeError::msg("cached string not utf-8"))
}
fn read_strs(r: &mut Cursor<&[u8]>) -> Result<Vec<String>> {
    let n = read_u32(r)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_str(r)?);
    }
    Ok(out)
}
fn read_usizes(r: &mut Cursor<&[u8]>) -> Result<Vec<usize>> {
    let n = read_u32(r)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_u32(r)? as usize);
    }
    Ok(out)
}
fn read_opt_u32(r: &mut Cursor<&[u8]>) -> Result<Option<usize>> {
    Ok(if read_u8(r)? == 0 {
        None
    } else {
        Some(read_u32(r)? as usize)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;

    #[test]
    fn key_changes_with_source_and_version() {
        let a = key("0.2.0", "a.tive", "1+1", "");
        let b = key("0.2.0", "a.tive", "1+2", "");
        let c = key("0.2.1", "a.tive", "1+1", "");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn special_files_skip_cache() {
        assert!(!should_use("<repl>"));
        assert!(!should_use("<script>"));
        assert!(!should_use("<test>"));
        assert_eq!(should_use("src/main.tive"), cache_enabled());
    }

    #[test]
    fn roundtrip_simple_program() {
        let src = "func add(a, b) { a + b }\nadd(1, 2)\n";
        let prog = crate::compile(src).expect("compile");
        let bytes = encode(&prog).expect("encode");
        let back = decode(&bytes).expect("decode");
        assert_eq!(back.functions.len(), prog.functions.len());
        assert_eq!(back.code.len(), prog.code.len());
        assert_eq!(back.hot.ops.len(), prog.hot.ops.len());
        assert_eq!(back.script_frame_slots, prog.script_frame_slots);
        assert_eq!(back.script_local_to_global, prog.script_local_to_global);
        let mut vm = Vm::new();
        vm.load_program(back).expect("load");
        let v = vm.run().expect("run");
        assert_eq!(v.display_string(), "3");
    }

    #[test]
    fn roundtrip_script_fast_locals() {
        let src = "let sum = 0\nsum = sum + 1\nsum\n";
        let prog = crate::compile(src).expect("compile");
        assert!(prog.script_frame_slots > 0);
        let bytes = encode(&prog).expect("encode");
        let back = decode(&bytes).expect("decode");
        assert_eq!(back.script_frame_slots, prog.script_frame_slots);
        assert_eq!(back.script_local_to_global, prog.script_local_to_global);
        let mut vm = Vm::new();
        vm.load_program(back).expect("load");
        let v = vm.run().expect("run");
        assert_eq!(v.display_string(), "1");
    }

    #[test]
    fn struct_program_not_encoded() {
        let prog = crate::compile("struct P { let x }\nP(1).x\n").expect("compile");
        assert!(encode(&prog).is_err());
    }

    #[test]
    fn store_load_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "optive_bc_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let src = "1 + 2\n";
        let prog = crate::compile(src).expect("compile");
        let path = dir.join("t.tivc");
        assert!(store(&path, &prog));
        let loaded = load(&path).expect("load");
        let mut vm = Vm::new();
        vm.load_program(loaded).expect("load vm");
        assert_eq!(vm.run().expect("run").display_string(), "3");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_run_hits_disk_cache() {
        if !cache_enabled() {
            return;
        }
        static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "optive_bc_hit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let prev = std::env::var("OPTIVE_BC_DIR").ok();
        std::env::set_var("OPTIVE_BC_DIR", &dir);
        reset_stats();
        let src = "func add(a, b) { a + b }\nadd(2, 3)\n";
        let file = dir.join("hit.tive");
        fs::write(&file, src).unwrap();
        let file_s = file.to_str().unwrap();
        let mut vm = crate::vm::Vm::new();
        let v = crate::run_source_in_vm(&mut vm, src, file_s).expect("first run");
        assert_eq!(v.display_string(), "5");
        let (stores, hits) = stats();
        assert!(
            stores >= 1,
            "expected a store, got stores={stores} hits={hits}"
        );
        let mut vm2 = crate::vm::Vm::new();
        let v2 = crate::run_source_in_vm(&mut vm2, src, file_s).expect("second run");
        assert_eq!(v2.display_string(), "5");
        let (_stores2, hits2) = stats();
        assert!(
            hits2 > hits,
            "second run should hit cache (hits {hits} -> {hits2})"
        );
        match prev {
            Some(p) => std::env::set_var("OPTIVE_BC_DIR", p),
            None => std::env::remove_var("OPTIVE_BC_DIR"),
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
