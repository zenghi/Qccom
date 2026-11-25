function script:Get-QccomLegacyEncoding {
    # Use the system ANSI code page used by legacy Windows builds of GNU Make.
    $codePage = [int][Microsoft.Win32.Registry]::GetValue(
        'HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Nls\CodePage', 'ACP', 0);
    if ($codePage -eq 0 -or $codePage -eq 65001) {
        $codePage = [Globalization.CultureInfo]::InstalledUICulture.TextInfo.ANSICodePage;
    }
    if ($codePage -eq 0 -or $codePage -eq 65001) { $codePage = 936 }
    [Text.Encoding]::GetEncoding($codePage);
}

function script:Invoke-QccomMakePipe($program, $toolArgs) {
    $info = New-Object Diagnostics.ProcessStartInfo;
    $info.FileName = $program;
    $info.WorkingDirectory = $PWD.ProviderPath;
    $info.UseShellExecute = $false;
    $info.RedirectStandardOutput = $true;
    $info.RedirectStandardError = $true;
    # ProcessStartInfo.Arguments also works on Windows PowerShell 5.1.
    $info.Arguments = ($toolArgs | ForEach-Object {
        '"' + (([string]$_ -replace '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
    }) -join ' ';

    $utf8 = New-Object Text.UTF8Encoding($false, $true);
    $legacy = Get-QccomLegacyEncoding;
    # Latin-1 is used only for one-to-one byte framing. Decode the original bytes
    # after assembling each complete line, including lines split across reads.
    $bytesEncoding = [Text.Encoding]::GetEncoding(28591);
    $process = New-Object Diagnostics.Process;
    $process.StartInfo = $info;
    $started = $false;
    try {
        $started = $process.Start();
        $readers = @(
            [IO.StreamReader]::new($process.StandardOutput.BaseStream, $bytesEncoding, $false),
            [IO.StreamReader]::new($process.StandardError.BaseStream, $bytesEncoding, $false)
        );
        $pending = @($readers[0].ReadLineAsync(), $readers[1].ReadLineAsync());
        while ($pending[0] -or $pending[1]) {
            for ($i = 0; $i -lt $pending.Length; ++$i) {
                $read = $pending[$i];
                if (!$read -or !$read.IsCompleted) { continue }
                $line = $read.GetAwaiter().GetResult();
                if ($null -eq $line) {
                    $pending[$i] = $null;
                    continue;
                }
                $data = $bytesEncoding.GetBytes($line);
                try { $utf8.GetString($data).TrimStart([char]0xFEFF) }
                catch { $legacy.GetString($data) }
                $pending[$i] = $readers[$i].ReadLineAsync();
            }
            $waiting = @($pending | Where-Object { $_ });
            if ($waiting.Count) {
                $null = [Threading.Tasks.Task]::WaitAny([Threading.Tasks.Task[]]$waiting, 100);
            }
        }
        $process.WaitForExit();
        $global:LASTEXITCODE = $process.ExitCode;
    } finally {
        if ($started -and !$process.HasExited) { $process.Kill() }
        $process.Dispose();
    }
}

function global:Invoke-QccomWchTool {
    $program = $args[0];
    $toolArgs = @($args | Select-Object -Skip 1);
    $global:LASTEXITCODE = 1;
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT -and
        $program -eq $env:QCCOM_REAL_MAKE -and [IO.Path]::GetExtension($program) -eq '.exe') {
        Invoke-QccomMakePipe $program $toolArgs;
    } else {
        & $program @toolArgs 2>&1 | ForEach-Object { $_.ToString() };
    }
}
