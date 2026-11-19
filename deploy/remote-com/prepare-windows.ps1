# Run next to qccom.exe. Add -CreatePair on the flashing PC in an elevated shell.
param([switch]$CreatePair)
$ErrorActionPreference = 'Stop'
$dest = Join-Path $PSScriptRoot 'remote-com'
New-Item -ItemType Directory -Force $dest | Out-Null
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
function Install-Helper($name, $url, $hash, $exeHash) {
    $installed = Join-Path $dest "$name.exe"
    if ((Test-Path -PathType Leaf $installed) -and (Get-FileHash $installed -Algorithm SHA256).Hash -eq $exeHash) { return }
    $archive = Join-Path $tmp "$name.zip"
    # SourceForge sends its HTML download page to browser user agents.
    Invoke-WebRequest -UseBasicParsing -UserAgent "QCCOM/1.9.8" $url -OutFile $archive
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $hash) { throw "SHA-256 mismatch: $name" }
    $unpacked = Join-Path $tmp $name
    Expand-Archive $archive -DestinationPath $unpacked
    $exe = Get-ChildItem $unpacked -Recurse -Filter "$name.exe" | Select-Object -First 1
    if (!$exe) { throw "Missing $name.exe" }
    Copy-Item $exe.FullName $dest -Force
}
try {
    Install-Helper frpc 'https://github.com/fatedier/frp/releases/download/v0.72.0/frp_0.72.0_windows_amd64.zip' '5197c1b69564089cdc229fc1260a0e25db1e722b1b7db45fe1542c3c43edf9ae' 'b8580b916ecb2e27ecc348bc67bc8b93b0e65266f85fd5d703e2f2f69862b089'
    Install-Helper hub4com 'https://downloads.sourceforge.net/project/com0com/hub4com/2.1.0.0/hub4com-2.1.0.0-386.zip' '24cca36ccf0cab0f988bb59851b5ec947667efe53c4f43f290392ad308ac0e01' '98a5ffd853fee50ff9cc54b7b8c31ac271705c873843a6ba9026c3d0a69bdaaf'
    if ($CreatePair) {
        $setup = @("$env:ProgramFiles\com0com\setupc.exe", "${env:ProgramFiles(x86)}\com0com\setupc.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
        if (!$setup) { throw 'Install the signed com0com driver first.' }
        # A monitors the application opening B via RI. Allocate fresh port numbers.
        & $setup install 'PortName=COM#,ri=ropen' 'PortName=COM#,EmuBR=yes'
        if ($LASTEXITCODE -ne 0) { throw 'com0com setup failed' }
        & $setup list
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}
