# Optive 单文件可执行程序示例

这个目录是一个完整的 Optive 项目。入口引用另一个模块中的编译期常量，并用
本模块的 `const func` 生成默认消息；构建器会先完成 CTFE，再把字节码、模块接口和 Runner
封装成一个自包含程序。

在 Optive 仓库根目录构建当前平台版本：

```powershell
cargo run --bin Optive -- build examples/standalone_hello --exe --clean
```

Windows 运行：

```powershell
examples/standalone_hello/.optive/build/app.exe
examples/standalone_hello/.optive/build/app.exe Alice
```

Linux 或 macOS 运行：

```bash
examples/standalone_hello/.optive/build/app
examples/standalone_hello/.optive/build/app Alice
```

也可以指定任意由本机 Rust/Cargo 工具链支持的 target。Optive 不维护 target
白名单，参数会原样传给 Cargo：

```powershell
Optive build examples/standalone_hello --exe --target <target-triple>
Optive build examples/standalone_hello --exe --target targets/custom.json
Optive build examples/standalone_hello --exe --output dist/hello
```

目标工具链、linker 和系统库必须已经安装。普通 target triple 会按连字符拆分并
完整保留所有组成部分；输出扩展名来自 Cargo 返回的实际 Runner artifact。
