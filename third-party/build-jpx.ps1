# builds the OpenJPEG codec module (SL-1.FILT.08) for wasm32-unknown-unknown.
# requires: LLVM clang with the wasm32 target (checked: clang 18.1.8 works;
# the npm wasm-opt shim on this host is broken â€” use a clean PATH).
# output: crates/selis-pdf-filter/assets/selis-jpx.wasm (+ .sha256)
# pinned: OpenJPEG v2.5.3 commit 210a8a5690d0da66f02d49420d7176a21ef409dc
$ErrorActionPreference = "Stop"
$clang = "C:\Program Files\LLVM\bin\clang.exe"
$root = $PSScriptRoot
$src = Join-Path $root "openjpeg"
$shim = Join-Path $src "shim"
$outDir = Join-Path $root "..\crates\selis-pdf-filter\assets"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$env:PATH = "C:\Windows\System32;C:\Windows;C:\Program Files\LLVM\bin"

$common = @(
    "--target=wasm32-unknown-unknown",
    "-O2",
    "-std=c99",
    "-DNDEBUG",
    "-DOPJ_HAVE_MALLOC_H=0",
    "-DOPJ_STATIC",                # not strictly needed, harmless
    "-I", "$shim",                 # shim libc headers + opj_config*.h first
    "-I", "$src",
    "-fno-builtin",
    "-fno-stack-protector",
    "-fvisibility=hidden",
    "-Wno-unknown-pragmas",
    "-Wno-unused-parameter",
    "-Wno-sign-compare",
    "-Wno-implicit-const-int-float-conversion",
    "-Wno-bitwise-instead-of-logical"
)

$sources = @(
    (Join-Path $shim "selis_libc.c"),
    (Join-Path $shim "selis_event.c"),
    (Join-Path $shim "selis_jpx.c")
) + (Get-ChildItem $src -Filter "*.c" | Where-Object { $_.Name -ne "event.c" } | ForEach-Object { $_.FullName })

$wasm = Join-Path $outDir "selis-jpx.wasm"
& $clang @common -nostdlib `
    "-Wl,--no-entry" `
    "-Wl,--export=init", "-Wl,--export=decode", "-Wl,--export=output", `
    "-Wl,--export=output_ptr", "-Wl,--export=finish", `
    "-Wl,--export=selis_heap_set_limit", `
    "-Wl,-z,stack-size=1048576", "-Wl,--stack-first" `
    "-Wl,--max-memory=67108864" `
    -o $wasm @sources
if ($LASTEXITCODE -ne 0) { throw "clang failed: $LASTEXITCODE" }
# strip the npm wasm-opt hook risk: done above via PATH scrub
& "C:\Program Files\LLVM\bin\llvm-objcopy.exe" --strip-all $wasm 2>$null
$hash = (Get-FileHash $wasm -Algorithm SHA256).Hash.ToLower()
Set-Content -Path "$wasm.sha256" -Value $hash -NoNewline -Encoding ascii
"built: $wasm"
"size: $((Get-Item $wasm).Length) bytes"
"sha256: $hash"
