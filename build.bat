echo off
rem 将目录cd到build.bat所在目录
set OriginalDir=%cd%
cd /d %~dp0
echo Building Optive at path: %cd%
echo on
cargo build --release
echo off
echo Build completed successfully.
echo Test the build:
echo on
cargo test --release -- --nocapture --ignored
echo off
echo Tests completed successfully.
cd /d %OriginalDir%