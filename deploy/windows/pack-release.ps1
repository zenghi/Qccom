# Run before code signing. UPX must be on PATH or supplied explicitly.
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][string]$Destination,
    [string]$Upx = 'upx'
)
$ErrorActionPreference = 'Stop'
$inputPath = (Resolve-Path $Executable).Path
$outputPath = [IO.Path]::GetFullPath($Destination)
if ($inputPath -eq $outputPath) { throw 'Choose a separate output file' }
$directory = [IO.Path]::GetDirectoryName($outputPath)
[void][IO.Directory]::CreateDirectory($directory)
$work = Join-Path $directory ([guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($work)
try {
    $packed = Join-Path $work 'qccom.exe'
    $restored = Join-Path $work 'restored.exe'
    & $Upx --exact --best --no-lzma -o $packed $inputPath
    if ($LASTEXITCODE) { throw 'NRV packing failed' }
    & $Upx -t $packed
    if ($LASTEXITCODE) { throw 'UPX integrity check failed' }
    & $Upx -d -o $restored $packed
    if ($LASTEXITCODE) { throw 'UPX restore failed' }
    if ((Get-FileHash $inputPath).Hash -ne (Get-FileHash $restored).Hash) { throw 'Restored binary differs from input' }
    if ([IO.File]::Exists($outputPath)) {
        [IO.File]::Replace($packed, $outputPath, (Join-Path $work 'previous.exe'))
    } else {
        [IO.File]::Move($packed, $outputPath)
    }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}
