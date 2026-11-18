# SL-5.EDIT.03 DoD: "PDFium and Acrobat both open it".
#
# This script uses Acrobat READER's AcroPDF.PDF COM control to load each PDF.
# Reader (unlike Acrobat Pro) does NOT expose the AcroExch.* automation object
# (E_NOINTERFACE) and AcroPDF.PDF has no page-count/validation method — only
# navigation and LoadFile. LoadFile returning 0 means the reader opened the file
# with no fatal error. A full visual/validation verdict is therefore a MANUAL
# GUI check in Acrobat; the scriptable interlock oracles in CI are qpdf, mutool
# and PDFium (pypdfium2). Run this script to smoke-open a set of outputs.
param([Parameter(ValueFromRemainingArguments=$true)][string[]]$Files)

if ($Files.Count -eq 0) {
  Write-Output "usage: acrobat_open_check.ps1 <pdf> [pdf ...]"
  exit 2
}
try {
  $ctl = New-Object -ComObject AcroPDF.PDF -ErrorAction Stop
} catch {
  Write-Output "ACROBAT: unavailable ($($_.Exception.Message)); manual GUI check required"
  exit 3
}
$versions = $ctl.GetVersions()
$ok = 0
foreach ($f in $Files) {
  if (-not (Test-Path $f)) { Write-Output "MISSING: $f"; continue }
  $full = (Resolve-Path $f).Path
  $r = $ctl.LoadFile($full)
  # Acrobat API: LoadFile returns 0 on success.
  if ($r -eq 0) { Write-Output "OPENED: $full"; $ok++ }
  else { Write-Output "FAILED: $full (LoadFile=$r)" }
}
Write-Output "ACROBAT Reader smoke-open: $ok/$($Files.Count) opened"
Write-Output "NOTE: Reader exposes no validation verdict; full check is manual GUI."
if ($ok -eq $Files.Count) { exit 0 } else { exit 1 }
