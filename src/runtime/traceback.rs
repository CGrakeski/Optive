//! Frame / Traceback 运行时值。

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::{FieldTypeInfo, StructDef, StructInstance, Value};
use crate::vm::Vm;

pub const FRAME_TYPE: &str = "Frame";
pub const TRACEBACK_TYPE: &str = "Traceback";

fn frame_def() -> Rc<StructDef> {
    Rc::new(StructDef {
        name: FRAME_TYPE.into(),
        base: None,
        fields: vec![
            "file".into(),
            "line".into(),
            "func".into(),
            "module".into(),
        ],
        mutable_fields: vec![false, false, false, false],
        typed: true,
        field_types: vec![
            FieldTypeInfo::default(),
            FieldTypeInfo::default(),
            FieldTypeInfo::default(),
            FieldTypeInfo::default(),
        ],
        type_params: Vec::new(),
    })
}

fn traceback_def() -> Rc<StructDef> {
    Rc::new(StructDef {
        name: TRACEBACK_TYPE.into(),
        base: None,
        fields: vec!["frames".into()],
        mutable_fields: vec![false],
        typed: true,
        field_types: vec![FieldTypeInfo::default()],
        type_params: Vec::new(),
    })
}

pub fn install(vm: &mut Vm) {
    vm.struct_defs
        .entry(FRAME_TYPE.into())
        .or_insert_with(frame_def);
    vm.struct_defs
        .entry(TRACEBACK_TYPE.into())
        .or_insert_with(traceback_def);
    vm.globals
        .entry(FRAME_TYPE.into())
        .or_insert_with(|| Value::type_ref(FRAME_TYPE));
    vm.globals
        .entry(TRACEBACK_TYPE.into())
        .or_insert_with(|| Value::type_ref(TRACEBACK_TYPE));
}

pub fn make_frame(
    file: impl Into<String>,
    line: i64,
    func: impl Into<String>,
    module: impl Into<String>,
) -> Value {
    let def = frame_def();
    Value::Struct(Rc::new(StructInstance {
        def,
        slots: RefCell::new(vec![
            Value::Text(file.into()),
            Value::Num(crate::value::Num::Small(line)),
            Value::Text(func.into()),
            Value::Text(module.into()),
        ]),
        generic_args: Vec::new(),
    }))
}

pub fn make_traceback(frames: Vec<Value>) -> Value {
    let def = traceback_def();
    Value::Struct(Rc::new(StructInstance {
        def,
        slots: RefCell::new(vec![Value::List(Rc::new(RefCell::new(frames)))]),
        generic_args: Vec::new(),
    }))
}

pub fn capture_traceback(vm: &Vm) -> Value {
    let mut frames = Vec::new();

    for frame in vm.func_frames.iter().rev() {
        frames.push(make_frame(
            &frame.file,
            frame.line as i64,
            &frame.name,
            vm.globals
                .get("__package__")
                .map(|v| v.print_string())
                .unwrap_or_else(|| "<main>".into()),
        ));
    }

    if frames.is_empty() {
        let file = vm.source_file.clone();
        let line = vm.current_line() as i64;
        let module = vm
            .globals
            .get("__package__")
            .map(|v| v.print_string())
            .unwrap_or_else(|| "<main>".into());
        frames.push(make_frame(&file, line, "<module>", &module));
    }
    make_traceback(frames)
}

pub fn is_traceback(val: &Value) -> bool {
    matches!(val, Value::Struct(s) if s.def.name == TRACEBACK_TYPE)
}

pub fn set_exception_traceback(exc: &Value, tb: Value) -> Value {
    let Value::Struct(s) = exc else {
        return exc.clone();
    };
    let mut slots = s.slots.borrow().clone();
    if slots.len() >= 2 {
        slots[1] = tb;
    } else if slots.len() == 1 {
        slots.push(tb);
    }
    Value::Struct(Rc::new(StructInstance {
        def: s.def.clone(),
        slots: RefCell::new(slots),
        generic_args: s.generic_args.clone(),
    }))
}

pub fn get_exception_traceback(exc: &Value) -> Option<Value> {
    let Value::Struct(s) = exc else {
        return None;
    };
    if s.def.fields.len() >= 2 && s.def.fields[1] == "traceback" {
        return Some(s.slots.borrow()[1].clone());
    }
    None
}
