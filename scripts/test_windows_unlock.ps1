# Disposable GitHub-hosted VM only. No user-machine credentials are used.
param([Parameter(Mandatory=$true)][string]$Binary)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
    throw 'Unlock fixture is restricted to disposable GitHub-hosted Windows runners.'
}
$account = Get-LocalUser -Name $env:USERNAME
if (-not $account.Enabled) { throw 'Runner account is disabled.' }
$entropy = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(16)
$password = 'Aa1!' + [System.Convert]::ToHexString($entropy).ToLowerInvariant()
$secure = ConvertTo-SecureString $password -AsPlainText -Force
# This VM is destroyed after the job. Never apply this to a personal machine.
Set-LocalUser -Name $account.Name -Password $secure
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = (Resolve-Path $Binary).Path
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$start.ArgumentList.Add('windows-desktop-probe')
$start.ArgumentList.Add('--lock')
$start.ArgumentList.Add('--ci-unlock')
$process = [System.Diagnostics.Process]::Start($start)
try {
    # Pipe the generated fixture secret; never put it in argv, env or logs.
    $process.StandardInput.Write($password)
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(90000)) {
        $process.Kill()
        throw 'Unlock fixture timed out.'
    }
    exit $process.ExitCode
} finally {
    $secure.Dispose()
    [System.Array]::Clear($entropy, 0, $entropy.Length)
    $password = $null
    $process.Dispose()
}
