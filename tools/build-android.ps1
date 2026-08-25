# 交叉编 aarch64-linux-android（arm64-v8a）。
# 依赖：rustup target aarch64-linux-android、cargo-ndk、Android NDK。
# 该目标不链接 libffi：extern / C.callback 运行时报错（见 docs/ffi.md）。
# 用法：pwsh tools/build-android.ps1
#       pwsh tools/build-android.ps1 -- -p optive --bin Optive

$ErrorActionPreference = "Stop"

function Find-Ndk {
    $envNames = @("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME")
    foreach ($n in $envNames) {
        $p = [Environment]::GetEnvironmentVariable($n)
        if ($p -and (Test-Path $p)) { return $p }
    }
    $sdkRoots = @(
        $env:ANDROID_SDK_ROOT,
        $env:ANDROID_HOME,
        (Join-Path $env:LOCALAPPDATA "Android\Sdk"),
        "C:\Android\sdk"
    ) | Where-Object { $_ }
    foreach ($sdk in $sdkRoots) {
        $ndkDir = Join-Path $sdk "ndk"
        if (Test-Path $ndkDir) {
            $latest = Get-ChildItem $ndkDir -Directory | Sort-Object Name -Descending | Select-Object -First 1
            if ($latest) { return $latest.FullName }
        }
        $bundle = Join-Path $sdk "ndk-bundle"
        if (Test-Path $bundle) { return $bundle }
    }
    return $null
}

$ndk = Find-Ndk
if (-not $ndk) {
    Write-Error @"
未找到 Android NDK。请先安装并用环境变量指向它，例如：

  winget install Google.PlatformTools
  # 或 Android Studio → SDK Manager → NDK

  `$env:ANDROID_NDK_HOME = 'C:\Users\<you>\AppData\Local\Android\Sdk\ndk\<version>'

不要用 ``cargo build --target aarch64-linux-android``：Windows 上 rustc 默认找不到 NDK clang。
用本脚本或：

  cargo ndk -t arm64-v8a build --release --bin Optive
"@
}

$env:ANDROID_NDK_HOME = $ndk
Write-Host "ANDROID_NDK_HOME=$ndk"

if (-not (Get-Command cargo-ndk -ErrorAction SilentlyContinue)) {
    Write-Error "未安装 cargo-ndk。执行: cargo install cargo-ndk"
}

$extra = $args
if ($extra.Count -eq 0) {
    $extra = @("--release", "--bin", "Optive")
}

Set-Location (Split-Path $PSScriptRoot -Parent)
& cargo ndk -t arm64-v8a build @extra
exit $LASTEXITCODE
