//! 虚拟机、值系统与模块加载。

pub mod builtins;
pub mod c_types;
pub mod caps;
pub mod concurrency;
pub mod debug;
pub mod enum_variant;
pub mod exceptions;
pub mod ffi;
pub mod ffi_extra;
pub mod ffi_pool;
pub mod gc;
pub mod module;
pub mod ptr_registry;
pub mod runtime_ast;
pub mod scheduler;
pub mod shared;
pub mod sized;
pub mod traceback;
pub mod type_registry;
pub mod types;
pub mod value;
pub mod vm;
