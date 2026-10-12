# Copies the workspace into flat/ with flattened names for a claude.ai
# Project knowledge base: mini_brep/src/lib.rs -> flat/mini_brep__src__lib.rs.
# Usage: ./flatten.ps1   (run from anywhere; flat/ is recreated each time)

$root = $PSScriptRoot
$out = Join-Path $root 'flat'
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory $out | Out-Null

# Top-level notes keep their names.
foreach ($doc in '00_PROJECT_OVERVIEW.md', 'PROJECT_INSTRUCTIONS.md') {
    Copy-Item (Join-Path $root $doc) $out
}

$count = 0
foreach ($crate in 'mini_brep', 'node_graph', 'usd_brep') {
    Get-ChildItem (Join-Path $root $crate) -Recurse -File |
        Where-Object { $_.FullName -notmatch '[\\/]target[\\/]' } |
        ForEach-Object {
            $rel = $_.FullName.Substring($root.Length + 1)
            Copy-Item $_.FullName (Join-Path $out ($rel -replace '[\\/]', '__'))
            $count++
        }
}
Write-Output "Wrote $count source files plus 2 notes to $out"
