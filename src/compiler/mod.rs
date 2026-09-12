//! 字节码生成与编译期变换。

pub mod bc_cache;
pub mod bundle;
pub mod codegen;
pub mod const_effect;
pub mod const_eval;
pub mod const_imports;
pub mod free_vars;
pub mod hot_code;
pub mod module_interface;
pub mod module_resolve;
pub mod monomorph;
pub mod opcode;
pub mod protocol;
pub mod specialize;
pub mod stack_effect;
pub mod standalone;
