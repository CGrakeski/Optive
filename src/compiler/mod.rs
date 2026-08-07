//! 字节码生成与编译期变换。

pub mod codegen;
pub mod free_vars;
pub mod hot_code;
pub mod monomorph;
pub mod opcode;
pub mod protocol;
pub mod specialize;
pub mod stack_effect;
