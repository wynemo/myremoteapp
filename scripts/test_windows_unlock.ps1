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
Set-LocalUser -Name $account.Name -Password $secure
# Test fixture only: reach a password screen without needing SAS generation.
New-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' -Name DisableCAD -PropertyType DWord -Value 1 -Force | Out-Null
# Make the disposable fixture enumerate its local account and the standard
# password provider. This setup is never used on the user's Windows machine.
$policies = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
$provider = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\Credential Providers\{60B78E88-EAD8-445C-9CFD-0B87F74EA6CD}'
if (-not (Test-Path $provider)) { throw 'Fixture image has no Windows password credential provider.' }
$disabled = (Get-Item $provider).GetValue('Disabled', $null)
$excluded = (Get-Item $policies).GetValue('ExcludedCredentialProviders', $null)
Write-Host "Fixture password provider: registered=true; disabled=$disabled; exclusions=$excluded"
New-ItemProperty $provider -Name Disabled -PropertyType DWord -Value 0 -Force | Out-Null
if ($null -ne $excluded) { Remove-ItemProperty $policies -Name ExcludedCredentialProviders }
New-ItemProperty $policies -Name DontDisplayLastUserName -PropertyType DWord -Value 0 -Force | Out-Null
New-ItemProperty $policies -Name DontDisplayLockedUserId -PropertyType DWord -Value 1 -Force | Out-Null
$userList = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList'
New-Item -Path $userList -Force | Out-Null
$hidden = (Get-Item $userList).GetValue($account.Name, $null)
Write-Host "Fixture local account enumeration setting before setup: $hidden"
New-ItemProperty $userList -Name $account.Name -PropertyType DWord -Value 1 -Force | Out-Null
# Turn off auto-logon in the disposable fixture; cached auto-logon credentials
# must not suppress the password provider after resetting its test password.
$logon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
foreach ($name in @('AutoAdminLogon', 'ForceAutoLogon')) {
    $value = (Get-Item $logon).GetValue($name, $null)
    Write-Host "Fixture $name before setup: $value"
    New-ItemProperty $logon -Name $name -PropertyType String -Value '0' -Force | Out-Null
}
$pipeName = 'myremoteapp-ci-' + [System.Guid]::NewGuid().ToString('N')
$pipe = [System.IO.Pipes.NamedPipeServerStream]::new($pipeName, [System.IO.Pipes.PipeDirection]::InOut, 1, [System.IO.Pipes.PipeTransmissionMode]::Byte, [System.IO.Pipes.PipeOptions]::Asynchronous)
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = (Resolve-Path $Binary).Path
$start.UseShellExecute = $false
$start.ArgumentList.Add('windows-desktop-probe')
$start.ArgumentList.Add('--lock')
$start.ArgumentList.Add('--ci-unlock')
$start.ArgumentList.Add('--ci-password-pipe')
$start.ArgumentList.Add($pipeName)
$process = [System.Diagnostics.Process]::Start($start)
try {
    $connected = $pipe.WaitForConnectionAsync()
    if (-not $connected.Wait(30000)) { throw 'Replacement process did not connect to the fixture pipe.' }
    # The original admin process must exit. Only its SYSTEM replacement remains.
    if (-not $process.WaitForExit(10000) -or $process.ExitCode -ne 0) {
        throw 'The original process failed to exit after self-restart.'
    }
    $payload = [System.Text.Encoding]::UTF8.GetBytes($password)
    $length = [System.BitConverter]::GetBytes([uint32]$payload.Length)
    $pipe.Write($length, 0, $length.Length)
    $pipe.Write($payload, 0, $payload.Length)
    $pipe.Flush()
    [System.Array]::Clear($payload, 0, $payload.Length)
    $reader = [System.IO.StreamReader]::new($pipe)
    $read = $reader.ReadLineAsync()
    if (-not $read.Wait(60000)) { throw 'Actual unlock verification timed out.' }
    $line = $read.Result
    if (-not $line) { throw 'Replacement exited without an unlock report.' }
    Write-Host "unlock_report=$line"
    $report = $line | ConvertFrom-Json
    if ($report.process_id -eq $process.Id) { throw 'Self-restart did not create a replacement.' }
    if (-not $report.actual_unlock_verified) { throw 'Actual password submission did not unlock the Windows session.' }
    if (-not $report.preview_continuity_verified) { throw 'Preview session did not survive unlock.' }
    if (-not $report.locked_connect_verified) { throw 'Fresh connection while locked did not deliver frames.' }
    if (-not $report.native_copy_resize_verified) { throw 'Native BitBlt capture and CPU resize verification failed.' }
    Write-Host 'Original process exited; actual password unlock succeeded and the existing preview resumed without reconnecting.'
} finally {
    $pipe.Dispose()
    $secure.Dispose()
    [System.Array]::Clear($entropy, 0, $entropy.Length)
    $password = $null
    $process.Dispose()
}
