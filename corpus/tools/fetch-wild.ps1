<#
.SYNOPSIS
    Wild-corpus sample fetcher (SL-0.CORP.05). Fetches a *sample* of real-world PDFs for the
    robustness gate. Read pdf-plan/06-CORPUS-POLICY.md before running anything beyond -ListOnly.

.DESCRIPTION
    Two sources:

      safedocs (default)  SAFEDOCS / CC-MAIN-2021-31-PDF-UNTRUNCATED on Digital Corpora
                          (s3://digitalcorpora). 7.93M untruncated real-world PDFs harvested
                          from Common Crawl CC-MAIN-2021-31 by NASA JPL / DARPA SafeDocs,
                          packaged as ~1000-PDF zips (~1.0-2.8 GB each) with full provenance
                          metadata. No WARC processing needed. 10 zips = ~10k PDFs (the DoD).

      commoncrawl         Direct Common Crawl: query the CDX index server for application/pdf,
                          byte-range the WARC record out of data.commoncrawl.org, carve the
                          PDF payload. EXPERIMENTAL: raw CC stores content truncated at 1 MB,
                          and this path exists to prove the fallback works, not as the default.

    Default mode is a dry run (-ListOnly): it prints what it WOULD fetch. Nothing is downloaded
    without -Execute.

    Policy obligations enforced/assumed here (06-CORPUS-POLICY.md):
      - fetch-only: files land in the wildcard cache OUTSIDE the repo, never committed,
        never redistributed, never uploaded to CI
      - encrypted at rest: refuses to run if the destination volume reports no encryption
        indicators (best effort on Windows)
      - provenance: writes a JSONL record per file (source, crawl, sha256, timestamp)

.EXAMPLE
    ./fetch-wild.ps1                          # dry run: what the DoD fetch would look like
    ./fetch-wild.ps1 -Execute -Zips 10        # the 10k-PDF DoD fetch (~13-16 GB, ~30-60 min)
    ./fetch-wild.ps1 -Execute -Source commoncrawl -CcLimit 200 -CcDomain nps.gov
#>
[CmdletBinding()]
param(
    # SAFEDOCS route
    [int]$Zips = 1,                          # number of 1000-PDF zips; DoD 10k = -Zips 10
    [int]$ZipStart = 0,                      # first zip index (stratify samples by varying this)

    # Common Crawl route
    [string]$Source = "safedocs",            # safedocs | commoncrawl
    [string]$Crawl = "",                     # CC crawl id; resolved from collinfo.json when empty
    [string]$CcDomain = "nps.gov",           # unbounded index queries are against ToU spirit; sample a domain
    [int]$CcLimit = 200,                     # index results to fetch (each ~1 WARC record)

    # Behaviour
    [switch]$Execute,                        # actually download; default is a dry run
    [switch]$ListOnly,                       # print the plan and exit (default when -Execute absent)
    [string]$OutDir = "",                    # default: %USERPROFILE%\.cache\selis-corpus\wild\<batch>
    [int]$MinFreeGB = 5                      # per-zip free-space floor
)

$ErrorActionPreference = "Stop"
$UtcNow = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

Write-Host "== wild-corpus sample fetcher (SL-0.CORP.05) =="
Write-Host "Policy: fetch-only, encrypted at rest, no redistribution, no human review without cause"
Write-Host "        -> pdf-plan/06-CORPUS-POLICY.md  (read it before -Execute)"
Write-Host ""

if (-not $Execute -or $ListOnly) { Write-Host "DRY RUN - nothing will be downloaded (pass -Execute to fetch)" }

# ---------------------------------------------------------------- policy: cache location
if ($OutDir -eq "") {
    $cacheRoot = Join-Path $env:USERPROFILE ".cache\selis-corpus\wild"
    $OutDir = Join-Path $cacheRoot ("batch-" + (Get-Date).ToUniversalTime().ToString("yyyyMMdd-HHmmss"))
}
if ($OutDir -like "*\selis\*" -or $OutDir -like "*/selis/*") {
    throw "policy: wild cache must live OUTSIDE the repo (corpus/pdfs and the worktree are for seed corpora only)"
}

function Test-VolumeEncrypted([string]$path) {
    # best-effort: BitLocker protection status on the volume backing $path
    try {
        $drive = (Get-Item (Split-Path -Parent $path) -ErrorAction SilentlyContinue).PSDrive.Name
        if (-not $drive) { return $true }
        $vol = Get-BitLockerVolume -MountPoint "$($drive):" -ErrorAction SilentlyContinue
        return ($null -eq $vol) -or ($vol.ProtectionStatus -eq "On")
    } catch { return $true }  # cannot verify -> do not block, the warning below suffices
}

function Assert-FreeGB([string]$path, [int]$needGB) {
    $drive = (New-Object System.IO.DriveInfo((Split-Path -Qualifier $path))).Name
    $freeGB = [math]::Round(((New-Object System.IO.DriveInfo($drive)).AvailableFreeSpace) / 1GB, 1)
    Write-Host ("  free space on {0}: {1} GB (floor {2} GB)" -f $drive, $freeGB, $needGB)
    if ($freeGB -lt $needGB) { throw "policy: only $freeGB GB free on $drive - need >= $needGB GB; free space or reduce the batch" }
}

function Get-Sha256([string]$file) { (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() }

function Write-Provenance($records, [string]$batchDir) {
    # provenance stays with the batch, outside the repo (06-CORPUS-POLICY.md §5)
    $path = Join-Path $batchDir "provenance.json"
    $records | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $path -Encoding UTF8
    Write-Host "provenance -> $path"
}

$provRecords = New-Object System.Collections.Generic.List[object]

# ================================================================ Route A: SAFEDOCS
if ($Source -eq "safedocs") {
    Write-Host ("source: SAFEDOCS (CC-MAIN-2021-31-PDF-UNTRUNCATED), zips {0}..{1} ({2} files)" -f $ZipStart, ($ZipStart + $Zips - 1), ($Zips * 1000))

    function Get-SafedocsZipUrl([int]$i) {
        # keys: corpora/files/CC-MAIN-2021-31-PDF-UNTRUNCATED/zipfiles/<group>/<NNNN>.zip
        # group = floor(i/1000)*1000, rendered as 0000-0999 style (verified via ListObjectsV2)
        $g = [int]([math]::Floor($i / 1000)) * 1000
        "https://downloads.digitalcorpora.org/corpora/files/CC-MAIN-2021-31-PDF-UNTRUNCATED/zipfiles/{0:d4}-{1:d4}/{2:d4}.zip" -f $g, ($g + 999), $i
    }

    $urls = 0..($Zips - 1) | ForEach-Object { Get-SafedocsZipUrl ($_ + $ZipStart) }
    $urls | ForEach-Object { Write-Host "  would fetch: $_" }

    if (-not $Execute) { Write-Host "DRY RUN complete."; exit 0 }

    Assert-FreeGB $OutDir ($MinFreeGB * $Zips)
    if (-not (Test-VolumeEncrypted $OutDir)) { Write-Warning "destination volume does not report BitLocker protection - policy requires encryption at rest" }
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    $total = 0
    foreach ($url in $urls) {
        $zipName = [IO.Path]::GetFileName($url)
        $zipPath = Join-Path $OutDir $zipName
        if (-not (Test-Path $zipPath)) {
            Write-Host "fetching $zipName ..."
            # curl.exe (not the PS alias), fail on HTTP errors, resume-friendly
            & curl.exe --fail --location --silent --show-error --retry 3 --output $zipPath $url
            if ($LASTEXITCODE -ne 0) { throw "download failed: $url" }
        }
        $sha = Get-Sha256 $zipPath
        Write-Host ("  sha256({0}) = {1}" -f $zipName, $sha)

        $ex = Join-Path $OutDir ([IO.Path]::GetFileNameWithoutExtension($zipName))
        if (-not (Test-Path $ex)) {
            Expand-Archive -LiteralPath $zipPath -DestinationPath $ex
        }
        $pdfs = Get-ChildItem -LiteralPath $ex -Filter *.pdf -Recurse
        $total += $pdfs.Count
        foreach ($p in $pdfs) {
            $provRecords.Add([pscustomobject]@{
                file = $p.FullName; sha256 = (Get-Sha256 $p.FullName); bytes = $p.Length
                safedocs_zip = $zipName; crawl = "CC-MAIN-2021-31"
                source = "https://digitalcorpora.org/corpora/file-corpora/cc-main-2021-31-pdf-untruncated/"
                fetched_utc = $UtcNow
            })
        }
        Write-Host ("  {0}: {1} pdfs (running total {2})" -f $zipName, $pdfs.Count, $total)
    }
    Write-Provenance $provRecords $OutDir
    Write-Host ("done: {0} pdfs in {1}" -f $total, $OutDir)
    exit 0
}

# ================================================================ Route B: Common Crawl direct
if ($Source -eq "commoncrawl") {
    Write-Host "source: Common Crawl CDX index + WARC range fetch (EXPERIMENTAL; CC truncates stored content at 1 MB)"

    if ($Crawl -eq "") {
        $colls = Invoke-RestMethod "https://index.commoncrawl.org/collinfo.json"
        $Crawl = $colls[0].id
        Write-Host "  resolved latest crawl: $Crawl"
    }
    $idxUrl = "https://index.commoncrawl.org/$Crawl-index" +
              "?url=*$CcDomain/*&matchType=domain&filter=mimetype:application/pdf&output=json&limit=$CcLimit"
    Write-Host "  index query: $idxUrl"
    if (-not $Execute) { Write-Host "DRY RUN complete."; exit 0 }

    Assert-FreeGB $OutDir $MinFreeGB
    if (-not (Test-VolumeEncrypted $OutDir)) { Write-Warning "destination volume does not report BitLocker protection - policy requires encryption at rest" }
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    $lines = (Invoke-WebRequest -Uri $idxUrl -UseBasicParsing).Content -split "`n" | Where-Object { $_.Trim() }
    $records = $lines | ForEach-Object {
        $r = $_ | ConvertFrom-Json
        if ($r.mimetype -eq "application/pdf") { $r }
    }
    Write-Host ("  {0} pdf records in index response" -f @($records).Count)

    $warcs = @{}
    foreach ($r in $records) {                      # group byte-ranges per WARC segment
        if (-not $warcs.ContainsKey($r.filename)) { $warcs[$r.filename] = @() }
        $warcs[$r.filename] += $r
    }

    foreach ($warcName in $warcs.Keys) {
        foreach ($r in $warcs[$warcName]) {
            $off = [long]$r.offset; $len = [long]$r.length
            $req = [Net.HttpWebRequest]::Create("https://data.commoncrawl.org/$warcName")
            $req.AddRange($off, $off + $len - 1)
            $resp = $req.GetResponse()
            try {
                $ms = New-Object IO.MemoryStream
                $resp.GetResponseStream().CopyTo($ms)
                $ms.Position = 0
                $gz = New-Object IO.Compression.GzipStream($ms, [IO.Compression.CompressionMode]::Decompress)
                $out = New-Object IO.MemoryStream; $gz.CopyTo($out); $gz.Dispose()
                $bytes = $out.ToArray()

                # WARC record: header block, blank line, HTTP response block, blank line, payload
                $text = [Text.Encoding]::ASCII.GetString($bytes, 0, [Math]::Min($bytes.Length, 8192))
                $h1 = $text.IndexOf("`r`n`r`n"); if ($h1 -lt 0) { continue }
                $cl = 0
                if ($text -match "(?im)^Content-Length:\s*(\d+)") { $cl = [int]$Matches[1] }
                $h2 = $text.IndexOf("`r`n`r`n", $h1 + 4); if ($h2 -lt 0) { continue }
                $bodyStart = $h2 + 4
                $bodyLen = [Math]::Min($cl, $bytes.Length - $bodyStart)
                if ($bodyLen -le 4) { continue }

                $pdf = New-Object byte[] $bodyLen
                [Array]::Copy($bytes, $bodyStart, $pdf, 0, $bodyLen)
                $msBody = New-Object IO.MemoryStream(,$pdf)
                $msBody.Position = 0
                if ([Text.Encoding]::ASCII.GetString($pdf, 0, 4) -ne "%PDF") { continue }  # truncated-record guard

                $name = ($r.urlkey -replace '[^a-zA-Z0-9._-]', '_') + "-" + $r.timestamp + ".pdf"
                $dest = Join-Path $OutDir $name
                [IO.File]::WriteAllBytes($dest, $pdf)
                $provRecords.Add([pscustomobject]@{
                    file = $dest; sha256 = (Get-Sha256 $dest); bytes = $bodyLen
                    original_url = $r.url; urlkey = $r.urlkey; timestamp = $r.timestamp
                    warc = $warcName; offset = $off; length = $len; crawl = $Crawl
                    source = "https://commoncrawl.org/terms-of-use"
                    fetched_utc = $UtcNow
                })
            } finally { $resp.Close() }
        }
    }
    Write-Provenance $provRecords $OutDir
    Write-Host ("done: {0} pdfs in {1}" -f $provRecords.Count, $OutDir)
    exit 0
}

throw "unknown -Source '$Source' (safedocs | commoncrawl)"
