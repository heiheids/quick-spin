# 构建发布版：编译 + 校验二进制里没有本机用户名/路径。
#
# 为什么需要它：Rust 会把**第三方 crate 的源码路径**写进 panic 位置的字符串里，
# 形如 `C:\Users\<用户名>\.cargo\registry\src\...`。直接 `cargo build --release`
# 出来的 exe 会带着这个路径，一旦发布就泄漏了本机用户名。
#
# `--remap-path-prefix` 把那段前缀重写成相对形式，从而抹掉用户名。
# 顺带再**校验一次**：将来某条依赖改了路径来源，这个脚本会直接报错拦下来，
# 而不是等发布之后才发现。
#
# 用法：  powershell -ExecutionPolicy Bypass -File .\build-release.ps1
#
# 注意：脚本里的中文注释要求本文件是 **UTF-8 带 BOM**，
# 否则 PowerShell 5.1 会按系统 ANSI 代码页解析而报一串假错。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

# ---- 1. 重映射第三方源码路径，抹掉用户名 ----
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE\.cargo=.cargo"
Write-Host "RUSTFLAGS = $env:RUSTFLAGS"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build 失败（退出码 $LASTEXITCODE）" }

# ---- 2. 校验产物 ----
$exe = Join-Path $root 'target\release\quick-spin.exe'
if (-not (Test-Path $exe)) { throw "找不到产物：$exe" }

$bytes = [System.IO.File]::ReadAllBytes($exe)
# 同时按 UTF-8 与 UTF-16 解一遍：panic 字符串是 UTF-8，但资源里可能是 UTF-16
$texts = @(
    [System.Text.Encoding]::UTF8.GetString($bytes),
    [System.Text.Encoding]::Unicode.GetString($bytes)
)

# 只查"足够长、不会误报"的串。用户名单独查会误伤
# （比如用户名是 `he` 时，仓库名 `heiheids` 就会命中），所以要求 >= 4 个字符。
$probes = @()
if ($env:USERPROFILE) { $probes += $env:USERPROFILE }
if ($env:USERPROFILE) { $probes += ($env:USERPROFILE -replace '\\', '/') }
if ($env:USERNAME -and $env:USERNAME.Length -ge 4) { $probes += $env:USERNAME }
$probes = $probes | Where-Object { $_ -and $_.Length -ge 4 } | Select-Object -Unique

$leaks = @()
foreach ($probe in $probes) {
    foreach ($t in $texts) {
        if ($t.Contains($probe)) { $leaks += $probe; break }
    }
}

if ($leaks.Count -gt 0) {
    Write-Host ""
    Write-Host "构建失败：二进制里仍含本机标识，**不要发布这个 exe**。" -ForegroundColor Red
    foreach ($l in ($leaks | Select-Object -Unique)) { Write-Host "  命中: $l" -ForegroundColor Red }
    Write-Host "（若确认是误报，可临时修改本脚本末尾的检查列表）" -ForegroundColor Yellow
    exit 1
}

$size = [math]::Round((Get-Item $exe).Length / 1KB, 1)
Write-Host ""
Write-Host "构建完成：$exe  ($size KB)" -ForegroundColor Green
Write-Host "已校验：不含本机用户名与用户目录路径。" -ForegroundColor Green
