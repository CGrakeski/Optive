//! `C.types.*` 目录的单一数据源：ABI、模块导出、转换表共用此表。

use crate::ffi::AbiType;

/// 一条 C 类型登记。
pub struct CTypeDef {
    /// 规范名（挂在 `C.types.` 下，如 `int`、`unsigned long`、`void*`）。
    pub c_name: &'static str,
    /// 本机 ABI。
    pub abi: fn() -> AbiType,
    /// getattr 别名 → 仍指向 `C.types.{c_name}`（如 `unsigned_int`）。
    pub export_aliases: &'static [&'static str],
    /// 额外可被 `from_type_name` 识别的名字（不含 `C.types.` 前缀），不单独导出。
    pub type_name_alts: &'static [&'static str],
}

const fn abi_void() -> AbiType {
    AbiType::Void
}
const fn abi_bool() -> AbiType {
    AbiType::Bool
}
const fn abi_i8() -> AbiType {
    AbiType::I8
}
const fn abi_u8() -> AbiType {
    AbiType::U8
}
const fn abi_i16() -> AbiType {
    AbiType::I16
}
const fn abi_u16() -> AbiType {
    AbiType::U16
}
const fn abi_i32() -> AbiType {
    AbiType::I32
}
const fn abi_u32() -> AbiType {
    AbiType::U32
}
const fn abi_i64() -> AbiType {
    AbiType::I64
}
const fn abi_u64() -> AbiType {
    AbiType::U64
}
const fn abi_isize() -> AbiType {
    AbiType::Isize
}
const fn abi_usize() -> AbiType {
    AbiType::Usize
}
const fn abi_f32() -> AbiType {
    AbiType::F32
}
const fn abi_f64() -> AbiType {
    AbiType::F64
}
const fn abi_ptr() -> AbiType {
    AbiType::Pointer
}

/// Windows LLP64：`long` / `unsigned long` 为 32 位；其余平台随指针宽。
const fn abi_host_long() -> AbiType {
    if cfg!(all(windows, target_pointer_width = "64")) {
        AbiType::I32
    } else {
        AbiType::Isize
    }
}

const fn abi_host_ulong() -> AbiType {
    if cfg!(all(windows, target_pointer_width = "64")) {
        AbiType::U32
    } else {
        AbiType::Usize
    }
}

/// 全部 `C.types` 条目（唯一清单）。
pub static C_TYPES: &[CTypeDef] = &[
    CTypeDef {
        c_name: "void",
        abi: abi_void,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "bool",
        abi: abi_bool,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "_Bool",
        abi: abi_bool,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "char",
        abi: abi_i8,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "signed char",
        abi: abi_i8,
        export_aliases: &["signed_char"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "unsigned char",
        abi: abi_u8,
        export_aliases: &["unsigned_char"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "short",
        abi: abi_i16,
        export_aliases: &["short_int"],
        type_name_alts: &["short int"],
    },
    CTypeDef {
        c_name: "int",
        abi: abi_i32,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "long",
        abi: abi_host_long,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "long long",
        abi: abi_i64,
        export_aliases: &["long_long"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "unsigned short",
        abi: abi_u16,
        export_aliases: &["unsigned_short"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "unsigned int",
        abi: abi_u32,
        export_aliases: &["unsigned_int"],
        type_name_alts: &["unsigned"],
    },
    CTypeDef {
        c_name: "unsigned long",
        abi: abi_host_ulong,
        export_aliases: &["unsigned_long"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "unsigned long long",
        abi: abi_u64,
        export_aliases: &["unsigned_long_long"],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "float",
        abi: abi_f32,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "double",
        abi: abi_f64,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "size_t",
        abi: abi_usize,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "ptrdiff_t",
        abi: abi_isize,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "intptr_t",
        abi: abi_isize,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "uintptr_t",
        abi: abi_usize,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "int8_t",
        abi: abi_i8,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "uint8_t",
        abi: abi_u8,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "int16_t",
        abi: abi_i16,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "uint16_t",
        abi: abi_u16,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "int32_t",
        abi: abi_i32,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "uint32_t",
        abi: abi_u32,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "int64_t",
        abi: abi_i64,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "uint64_t",
        abi: abi_u64,
        export_aliases: &[],
        type_name_alts: &[],
    },
    CTypeDef {
        c_name: "void*",
        abi: abi_ptr,
        export_aliases: &["void_ptr"],
        type_name_alts: &[],
    },
    // 护照指针（与 void* 同宽）；可写 `ptr[T]` / `C.types.ptr[T]` 作类型形式。
    CTypeDef {
        c_name: "ptr",
        abi: abi_ptr,
        export_aliases: &[],
        type_name_alts: &["pointer"],
    },
    // UTF-8 C 字符串指针：ABI 同 void*，extern 可从 text 临时编组。
    CTypeDef {
        c_name: "char*",
        abi: abi_char_ptr,
        export_aliases: &["char_ptr"],
        type_name_alts: &[],
    },
    // UTF-16 宽字符串指针（Windows wchar_t* / LPCWSTR）。
    CTypeDef {
        c_name: "wchar_t*",
        abi: abi_wchar_ptr,
        export_aliases: &["wchar_ptr"],
        type_name_alts: &[],
    },
    // 明确的无符号 char（char 在本实现中固定为 signed i8）。
    CTypeDef {
        c_name: "uchar",
        abi: abi_u8,
        export_aliases: &[],
        type_name_alts: &[],
    },
];

const fn abi_char_ptr() -> AbiType {
    AbiType::CharPtr
}

const fn abi_wchar_ptr() -> AbiType {
    AbiType::WCharPtr
}

/// 语言侧可直接作 ABI 注解的名字（非 `C.types.*`）。
type LangAbiEntry = (&'static str, fn() -> AbiType);
static LANG_ABI_NAMES: &[LangAbiEntry] = &[
    ("void", abi_void),
    ("nonetype", abi_void),
    ("bool", abi_bool),
    ("i8", abi_i8),
    ("u8", abi_u8),
    ("i16", abi_i16),
    ("u16", abi_u16),
    ("i32", abi_i32),
    ("u32", abi_u32),
    ("i64", abi_i64),
    ("u64", abi_u64),
    ("isize", abi_isize),
    ("usize", abi_usize),
    ("f32", abi_f32),
    ("f64", abi_f64),
    ("ptr", abi_ptr),
    ("pointer", abi_ptr),
    ("char_ptr", abi_char_ptr),
    ("wchar_ptr", abi_wchar_ptr),
];

impl CTypeDef {
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("C.types.{}", self.c_name)
    }
}

/// 按类型名解析到表项（接受 `int` / `C.types.int` / alt / export alias）。
#[must_use]
pub fn lookup_c_type(name: &str) -> Option<&'static CTypeDef> {
    let bare = name.strip_prefix("C.types.").unwrap_or(name);
    C_TYPES.iter().find(|e| {
        e.c_name == bare
            || e.export_aliases.contains(&bare)
            || e.type_name_alts.contains(&bare)
    })
}

/// ABI 解析：语言定宽名 + `C.types.*` 表。
pub fn abi_from_type_name(name: &str) -> Option<AbiType> {
    if let Some((_, abi)) = LANG_ABI_NAMES.iter().find(|(n, _)| *n == name) {
        return Some(abi());
    }
    lookup_c_type(name).map(|e| (e.abi)())
}

/// 所有应安装 `__convert__` 的 `C.types.*` 全名（含 `type_name_alts`）。
#[must_use]
pub fn all_c_type_convert_names() -> Vec<String> {
    let mut out = Vec::new();
    for e in C_TYPES {
        out.push(e.full_name());
        for alt in e.type_name_alts {
            out.push(format!("C.types.{alt}"));
        }
    }
    out
}
