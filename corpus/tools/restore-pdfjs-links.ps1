# Restore the pdf.js test-corpus files that upstream does not vendor
# (SL-0.CORP.01/02). pdf.js ships `test/pdfs/*.pdf.link` pointer files whose
# first line is a URL; the build fetches them on demand. This script replays
# that fetch for every pointer whose target is missing from the local corpus,
# so a disk clean can be repaired from the same sources.
#
# The corpus is fetch-only and never committed (SL-0.LEGAL.04): files land in
# corpus/pdfs/ (gitignored). Fetch failures are reported, never silently
# skipped - a missing file with a committed expectation shows up in
# `cargo xtask corpus verify` as unchecked.
#
# Usage:
#   ./restore-pdfjs-links.ps1                       # default roots
#   ./restore-pdfjs-links.ps1 -MaxParallel 4        # gentler on archive.org
#
# Requirements: tar, curl (both present on Windows 10+).
[CmdletBinding()]
param(
    # The pdf.js tarball cached by `cargo xtask corpus fetch`.
    [string]$Tarball = "$env:USERPROFILE\.cache\selis-corpus\pdfjs.gz",
    # Where the extracted tarball lives (temporary; pointer files are read from here).
    [string]$ExtractDir = "$env:TEMP\selis-pdfjs-restore",
    # The local corpus root (the pdfs directory itself).
    [string]$PdfsRoot = "corpus\pdfs",
    # Concurrent curl transfers.
    [int]$MaxParallel = 6
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Tarball)) {
    throw "pdf.js tarball not found at $Tarball - run 'cargo xtask corpus fetch' first"
}
if (-not (Test-Path $PdfsRoot)) {
    throw "corpus pdfs root not found at $PdfsRoot - create it (or fix -PdfsRoot) first"
}

# 1. Extract the tarball to read the pointer files (kept out of the corpus root).
if (Test-Path $ExtractDir) { Remove-Item -Recurse -Force $ExtractDir }
New-Item -ItemType Directory -Force -Path $ExtractDir | Out-Null
Write-Host "== extracting $Tarball =="
tar -xzf $Tarball -C $ExtractDir
$repoDir = Get-ChildItem $ExtractDir -Directory | Select-Object -First 1
$linkDir = Join-Path $repoDir.FullName "test\pdfs"
if (-not (Test-Path $linkDir)) { throw "test/pdfs not found in the tarball" }

# 2. Every pointer whose target PDF is missing becomes a fetch job.
$jobs = @()
foreach ($link in (Get-ChildItem $linkDir -Filter *.link)) {
    $target = Join-Path $PdfsRoot ($link.BaseName + ".pdf")
    if (Test-Path $target) { continue }
    $url = (Get-Content $link.FullName -TotalCount 1).Trim()
    if ($url -notmatch '^https?://') { Write-Warning "skipping $($link.Name): not a URL: $url"; continue }
    $jobs += [pscustomobject]@{ Name = $link.BaseName; Url = $url; Out = $target }
}
Write-Host "== $($jobs.Count) pointer files to fetch =="

# 3. Fetch with curl's native parallelism (one process, -o per URL).
# Config-file paths use forward slashes: backslashes inside quoted curl-config
# values are treated as escapes and would mangle the output path.
$failed = New-Object System.Collections.Generic.List[string]
$batch = 60
for ($i = 0; $i -lt $jobs.Count; $i += $batch) {
    $slice = $jobs[$i..([Math]::Min($i + $batch - 1, $jobs.Count - 1))]
    $cfg = New-TemporaryFile
    foreach ($j in $slice) {
        Add-Content $cfg "url = `"$($j.Url)`""
        Add-Content $cfg "output = `"$($j.Out -replace '\\', '/')`""
    }
    Write-Host ("  fetching {0}..{1} of {2}" -f ($i + 1), ($i + $slice.Count), $jobs.Count)
    & curl.exe --parallel --parallel-max $MaxParallel --fail --location --silent --show-error `
        --connect-timeout 20 --max-time 180 --config $cfg.FullName
    Remove-Item $cfg
}

# 4. Verify every output is really a PDF; retry failures sequentially once.
$retry = New-Object System.Collections.Generic.List[string]
foreach ($j in $jobs) {
    if (-not (Test-Path $j.Out)) { $retry.Add($j.Name); continue }
    $bytes = [System.IO.File]::ReadAllBytes($j.Out)[0..3]
    $magic = [System.Text.Encoding]::ASCII.GetString($bytes)
    if ($magic -ne "%PDF") {
        Remove-Item $j.Out -Force
        $retry.Add($j.Name)
    }
}
if ($retry.Count -gt 0) {
    Write-Host "== retrying $($retry.Count) failures sequentially =="
    foreach ($name in $retry) {
        $j = $jobs | Where-Object { $_.Name -eq $name }
        & curl.exe --fail --location --silent --show-error --retry 3 --retry-delay 2 `
            --connect-timeout 20 --max-time 300 -o $j.Out $j.Url
        if ((Test-Path $j.Out) -and
            ([System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($j.Out)[0..3]) -eq "%PDF")) {
            $failed.Remove($name) | Out-Null
        } else {
            if (Test-Path $j.Out) { Remove-Item $j.Out -Force }
            $failed.Add($name)
        }
    }
}

Write-Host "== done: $($jobs.Count - $failed.Count) fetched, $($failed.Count) failed =="
# The extracted tarball was only needed for the pointer files.
Remove-Item -Recurse -Force $ExtractDir
if ($failed.Count -gt 0) {
    $failed | Sort-Object | ForEach-Object { Write-Host "  FAILED $_" }
    exit 1
}
