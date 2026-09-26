# builds the native host reference oracle `refjpx.exe` (SL-1.FILT.08).
# This is a TEST ORACLE for the host only — never shipped, never linked into
# the engine (the engine decodes JPX only through the Tier-2 wasm sandbox).
#
# Compiles the vendored OpenJPEG 2.5.3 with the real host headers plus the
# hand-written native opj_config (shim-host), then links refjpx.c against it.
#
# requires: gcc on PATH (checked: C:\GNU\bin\gcc.exe, GCC 11.1.0)
# output: C:\selis-build\refjpx\refjpx.exe (CARGO_TARGET_DIR-style scratch, D: never written)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
$src = Join-Path $root "openjpeg"
$cfg = Join-Path $src "shim-host"
$outDir = "C:\selis-build\refjpx"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$sources = @(Get-ChildItem $src -Filter "*.c" | ForEach-Object { $_.FullName })
$sources += Join-Path $root "refjpx.c"

$defs = @(
    "-DNDEBUG",
    "-DOPJ_STATIC",
    "-DOPJ_HAVE_MALLOC_H=0"
)
$incs = @(
    "-I", $cfg,
    "-I", (Join-Path $cfg "private"),
    "-I", $src
)
$warns = @(
    "-Wno-unused-parameter",
    "-Wno-sign-compare",
    "-Wno-implicit-const-int-float-conversion",
    "-Wno-bitwise-instead-of-logical"
)

$exe = Join-Path $outDir "refjpx.exe"
& gcc -O2 -std=c99 @defs @incs @warns -o $exe @sources
if ($LASTEXITCODE -ne 0) { throw "gcc failed: $LASTEXITCODE" }
"built: $exe"
