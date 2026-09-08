param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Connect", "Disconnect")]
    [string]$Action,
    [Parameter(Mandatory = $true)]
    [string]$SettingsPath,
    [Parameter(Mandatory = $true)]
    [string]$PidPath,
    [Parameter(Mandatory = $true)]
    [string]$LogPath,
    [Parameter(Mandatory = $true)]
    [string]$CredentialTarget
)

$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

internal static class VergeCredentialReader
{
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct Credential
    {
        public uint Flags;
        public uint Type;
        public IntPtr TargetName;
        public IntPtr Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public IntPtr TargetAlias;
        public IntPtr UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CredRead(string target, uint type, uint flags, out IntPtr credential);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern void CredFree(IntPtr credential);

    internal static string Read(string target)
    {
        IntPtr pointer;
        if (!CredRead(target, 1, 0, out pointer))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());

        try
        {
            Credential credential = (Credential)Marshal.PtrToStructure(pointer, typeof(Credential));
            byte[] bytes = new byte[credential.CredentialBlobSize];
            Marshal.Copy(credential.CredentialBlob, bytes, 0, bytes.Length);
            return Encoding.UTF8.GetString(bytes);
        }
        finally
        {
            CredFree(pointer);
        }
    }
}
'@

function Get-RunningOpenConnectProcess {
    if (-not (Test-Path -LiteralPath $PidPath)) {
        return $null
    }

    $processId = 0
    if (-not [int]::TryParse((Get-Content -LiteralPath $PidPath -Raw).Trim(), [ref]$processId)) {
        Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
        return $null
    }

    $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
    if ($process -and $process.ProcessName -ieq "openconnect") {
        return $process
    }

    Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
    return $null
}

if ($Action -eq "Disconnect") {
    $process = Get-RunningOpenConnectProcess
    if ($process) {
        Stop-Process -Id $process.Id -Force
    }
    Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
    exit 0
}

if (Get-RunningOpenConnectProcess) {
    exit 0
}

$settings = Get-Content -LiteralPath $SettingsPath -Raw | ConvertFrom-Json
if (-not (Test-Path -LiteralPath $settings.executable)) {
    throw "OpenConnect executable not found: $($settings.executable)"
}
if ($settings.vpncScript -and -not (Test-Path -LiteralPath $settings.vpncScript)) {
    throw "vpnc script not found: $($settings.vpncScript)"
}

$uri = [Uri]$settings.endpoint
$defaultRoutes = Get-NetRoute -AddressFamily IPv4 -DestinationPrefix "0.0.0.0/0" |
    Where-Object {
        $_.InterfaceAlias -ne $settings.vpnInterface -and
        $_.InterfaceAlias -notmatch "(?i)(mihomo|meta|clash|singbox|tun|vethernet)"
    }
if ($settings.physicalInterface) {
    $defaultRoutes = $defaultRoutes | Where-Object InterfaceAlias -eq $settings.physicalInterface
}
$physicalRoute = $defaultRoutes | Sort-Object RouteMetric, InterfaceMetric | Select-Object -First 1
if (-not $physicalRoute) {
    throw "No physical IPv4 default route is available for the VPN gateway"
}

[Net.Dns]::GetHostAddresses($uri.DnsSafeHost) |
    Where-Object AddressFamily -eq ([Net.Sockets.AddressFamily]::InterNetwork) |
    ForEach-Object {
        $prefix = "$($_.IPAddressToString)/32"
        $existing = Get-NetRoute -AddressFamily IPv4 -DestinationPrefix $prefix -ErrorAction SilentlyContinue |
            Where-Object InterfaceIndex -eq $physicalRoute.InterfaceIndex
        if (-not $existing) {
            New-NetRoute -DestinationPrefix $prefix `
                -InterfaceIndex $physicalRoute.InterfaceIndex `
                -NextHop $physicalRoute.NextHop `
                -RouteMetric 1 | Out-Null
        }
    }

$arguments = [System.Collections.Generic.List[string]]::new()
$arguments.Add("--protocol=$($settings.protocol)")
$arguments.Add("--user=$($settings.username)")
$arguments.Add("--passwd-on-stdin")
$arguments.Add("--interface=$($settings.vpnInterface)")
if ($settings.authGroup) { $arguments.Add("--authgroup=$($settings.authGroup)") }
if ($settings.vpncScript) { $arguments.Add("--script=$($settings.vpncScript)") }
$arguments.Add("--reconnect-timeout=1000")
$arguments.Add($settings.endpoint)

$password = [VergeCredentialReader]::Read($CredentialTarget)
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $settings.executable
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardInput = $true

if ($startInfo.PSObject.Properties.Name -contains "ArgumentList") {
    foreach ($argument in $arguments) { $startInfo.ArgumentList.Add($argument) }
} else {
    function Quote-Argument([string]$value) {
        if ($value -notmatch '[\s"]') { return $value }
        return '"' + ($value -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
    }
    $startInfo.Arguments = ($arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
}

$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
try {
    New-Item -ItemType Directory -Path (Split-Path -Parent $LogPath) -Force | Out-Null
    Add-Content -LiteralPath $LogPath -Value "[$(Get-Date -Format o)] Starting $($settings.name)"
    if (-not $process.Start()) { throw "OpenConnect failed to start" }
    $process.StandardInput.WriteLine($password)
    $process.StandardInput.Close()
    Set-Content -LiteralPath $PidPath -Value $process.Id -Encoding ASCII
} finally {
    $password = $null
}

Start-Sleep -Milliseconds 800
if ($process.HasExited) {
    Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue
    throw "OpenConnect exited before the tunnel became ready (exit code $($process.ExitCode))"
}
