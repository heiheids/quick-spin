# 打发布包：构建（含用户名重映射）+ 组装 zip + 逐项校验。
#
# 为什么需要它：v0.1.0 / v0.1.1 的包是手工拼的。手工拼的代价不是"多花几分钟"，
# 而是**错了没人拦** —— 少一个 `使用说明.txt`、多混进一个 `shortcuts\` 里的
# `.lnk`、或者拿一份忘了重映射用户名的 exe 去发布，这三件事都能悄无声息地发生。
# 所以这个脚本的重点在最后那一步**校验**：不满足就删掉产物并非零退出。
#
# 用法：  powershell -ExecutionPolicy Bypass -File .\package-release.ps1
#
# 产物：  ..\quick-spin-<版本>-windows-x64.zip   （与仓库同级，不改仓库内容）
#
# 注意：脚本里的中文注释要求本文件是 **UTF-8 带 BOM**，
# 否则 PowerShell 5.1 会按系统 ANSI 代码页解析而报一串假错。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $root

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

# ---- 1. 版本号（从 Cargo.toml 取，不手写：写死迟早和 exe 里的对不上）----
$cargo = Get-Content 'Cargo.toml' -Encoding UTF8
$version = $null
foreach ($line in $cargo) {
    if ($line -match '^\s*version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
}
if (-not $version) { throw '无法从 Cargo.toml 读到版本号' }
Write-Host "版本：$version"

# ---- 2. 构建（用户名重映射 + 它自带的校验）----
# 交给 build-release.ps1，不要在这里重写一遍：那会把"发布产物必须不含本机
# 用户名"这条规则的实现变成两份。
& (Join-Path $root 'build-release.ps1')
if ($LASTEXITCODE -ne 0) { throw "build-release.ps1 失败（退出码 $LASTEXITCODE）" }

$exe = Join-Path $root 'target\release\quick-spin.exe'
if (-not (Test-Path $exe)) { throw "找不到产物：$exe" }

# ---- 3. 组装 zip ----
# 结构照 v0.1.0 / v0.1.1：**四个文件平铺，没有外层目录**。
# 平铺是有意的：使用者解压出来直接就是程序所在目录（便携布局要求
# config.toml / shortcuts\ 与 exe 同级）。
$payload = [ordered]@{
    'quick-spin.exe' = $exe
    '使用说明.txt'   = (Join-Path $root 'assets\使用说明.txt')
    'LICENSE'        = (Join-Path $root 'LICENSE')
    'README.md'      = (Join-Path $root 'README.md')
}
foreach ($k in $payload.Keys) {
    if (-not (Test-Path $payload[$k])) { throw "缺少打包文件：$($payload[$k])" }
}

$outDir = Split-Path -Parent $root
$out = Join-Path $outDir "quick-spin-$version-windows-x64.zip"
if (Test-Path $out) { Remove-Item $out -Force }

$zip = [System.IO.Compression.ZipFile]::Open($out, [System.IO.Compression.ZipArchiveMode]::Create)
$C = [System.IO.Compression.ZipFileExtensions]
try {
    foreach ($k in $payload.Keys) {
        $null = $C::CreateEntryFromFile($zip, $payload[$k], $k)
    }
} finally {
    $zip.Dispose()
}

# ---- 4. 校验产物：不通过就删掉并报错 ----
$problems = @()
$z = [System.IO.Compression.ZipFile]::OpenRead($out)
try {
    $names = @($z.Entries | ForEach-Object { $_.FullName })

    # 4.1 不多不少
    foreach ($k in $payload.Keys) {
        if ($names -notcontains $k) { $problems += "缺少条目：$k" }
    }
    foreach ($n in $names) {
        if ($payload.Keys -notcontains $n) { $problems += "多出条目：$n" }
    }

    # 4.2 绝不能进包的东西。
    # `.lnk` 进来意味着把使用者自己的快捷方式发出去了；`state.toml` 进来会让
    # 收件人首次运行时把记录判为失效并清掉（见使用说明里的那段注意）。
    foreach ($n in $names) {
        if ($n -like '*.lnk')      { $problems += "不该包含 .lnk：$n" }
        if ($n -like '*state.toml') { $problems += "不该包含 state.toml：$n" }
        if ($n -like '*.pdb' -or $n -like '*.log' -or $n -like '*.d') {
            $problems += "不该包含调试/日志文件：$n"
        }
    }

    # 4.3 exe 不能是空壳
    $e = $z.Entries | Where-Object { $_.FullName -eq 'quick-spin.exe' }
    if ($e -and $e.Length -lt 500KB) { $problems += "exe 太小（$($e.Length) 字节），像是没编上" }
    $exeSize = if ($e) { $e.Length } else { 0 }
} finally {
    $z.Dispose()
}

if ($problems.Count -gt 0) {
    Write-Host ''
    Write-Host '打包失败，产物已删除：' -ForegroundColor Red
    foreach ($p in $problems) { Write-Host "  $p" -ForegroundColor Red }
    Remove-Item $out -Force -ErrorAction SilentlyContinue
    exit 1
}

$mb = [math]::Round((Get-Item $out).Length / 1MB, 2)
Write-Host ''
Write-Host "打包完成：$out  ($mb MB)" -ForegroundColor Green
Write-Host "  条目（$($payload.Count)）：$($payload.Keys -join ' / ')" -ForegroundColor Green
Write-Host "  exe：$([math]::Round($exeSize / 1KB, 1)) KB（已由 build-release.ps1 校验不含本机用户名）" -ForegroundColor Green
