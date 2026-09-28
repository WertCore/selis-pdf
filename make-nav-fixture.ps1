# Generates the SL-3.DOC-NAV conformance fixture with a byte-exact xref table.
# One-shot generator; the artifact it writes is committed and this script is
# deleted with it, so the PDF is the reviewable artifact.
$objs = [ordered]@{}
$objs['1']  = '<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /Names 41 0 R /PageLabels 43 0 R >>'
$objs['2']  = '<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>'
$objs['3']  = '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [20 0 R 21 0 R 23 0 R 25 0 R 27 0 R 29 0 R 30 0 R] >>'
$objs['4']  = '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>'
$objs['10'] = '<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 99 >>'
$objs['11'] = '<< /Title (Chapter One) /Parent 10 0 R /Next 12 0 R /First 13 0 R /Last 13 0 R /Count -2 /Dest [4 0 R /XYZ 100 200 1.5] >>'
$objs['12'] = '<< /Title (Appendix) /Parent 10 0 R /Prev 11 0 R /Dest (chapter-one) >>'
$objs['13'] = '<< /Title (Section 1.1) /Parent 11 0 R /First 11 0 R /A 14 0 R >>'
$objs['14'] = '<< /S /GoTo /D [3 0 R /Fit] >>'
$objs['20'] = '<< /Type /Annot /Subtype /Link /Rect [10 700 200 720] /Dest [3 0 R /FitH 700] /Contents (Top of page one) >>'
$objs['30'] = '<< /Type /Annot /Subtype /Widget /Rect [10 10 100 30] /FT /Tx /T (field1) >>'
$objs['21'] = '<< /Type /Annot /Subtype /Link /Rect [10 660 200 680] /A 22 0 R >>'
$objs['22'] = '<< /S /URI /URI (https://example.test/a?b=1&c=2) >>'
$objs['23'] = '<< /Type /Annot /Subtype /Link /Rect [10 620 200 640] /A 24 0 R >>'
$objs['24'] = '<< /S /Launch /F (cmd.exe /c calc.exe) >>'
$objs['25'] = '<< /Type /Annot /Subtype /Link /Rect [10 580 200 600] /A 26 0 R >>'
$objs['26'] = '<< /S /Rendition /R << /Type /MediaPlayback /D 4 0 R >> >>'
$objs['27'] = '<< /Type /Annot /Subtype /Link /Rect [10 540 200 560] /A 28 0 R >>'
$objs['28'] = '<< /S /SubmitForm /F 31 0 R /Flags 4 >>'
$objs['31'] = '<< /Type /Filespec /F (https://example.test/post) /UF (https://example.test/post) >>'
$objs['29'] = '<< /Type /Annot /Subtype /Link >>'
$objs['41'] = '<< /Dests 42 0 R >>'
$objs['42'] = '<< /Names [(chapter-one) [4 0 R /Fit] (page-two) 4 0 R] >>'
$objs['43'] = '<< /Nums [0 << /S /r >> 1 << /S /D /P (A-) /St 1 >>] >>'

$sb = New-Object System.Text.StringBuilder
[void]$sb.Append("%PDF-1.7`n")
$offsets = @{}
$max = 0
foreach ($key in $objs.Keys) {
    $n = [int]$key
    if ($n -gt $max) { $max = $n }
    $offsets[$key] = $sb.Length
    [void]$sb.Append("$n 0 obj`n$($objs[$key])`nendobj`n")
}
$xref = $sb.Length
[void]$sb.Append("xref`n0 $($max + 1)`n")
[void]$sb.Append("0000000000 65535 f `n")
for ($i = 1; $i -le $max; $i++) {
    $k = [string]$i
    if ($offsets.ContainsKey($k)) {
        [void]$sb.Append((('{0:D10} 00000 n ' -f $offsets[$k]) + "`n"))
    } else {
        [void]$sb.Append("0000000000 65535 f `n")
    }
}
[void]$sb.Append("trailer`n<< /Size $($max + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")
$bytes = [System.Text.Encoding]::GetEncoding(28591).GetBytes($sb.ToString())
[System.IO.File]::WriteAllBytes("$PWD\crates\selis-pdf-engine\src\fixtures\nav.pdf", $bytes)
"wrote $($bytes.Length) bytes, $($objs.Count) objects, startxref $xref"
