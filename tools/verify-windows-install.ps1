param(
    [Parameter(Mandatory)][string]$Msi,
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [ValidateRange(10, 120)][int]$TimeoutSeconds = 60
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-VerificationExitCode($Receipt) {
    if ($Receipt.installation -ne 'PASS' -or $Receipt.cleanup.errors.Count -ne 0 `
        -or $Receipt.cleanup.uninstallExitCode -ne 0) { return 1 }
    foreach ($field in @('productAbsent', 'installDirectoryAbsent', 'shortcutAbsent', 'registrationsAbsent', 'associationAbsent')) {
        if ($Receipt.cleanup[$field] -ne $true) { return 1 }
    }
    if ($null -eq $Receipt.failure -and $Receipt.shortcut -eq 'PASS' -and $Receipt.association -eq 'PASS' `
        -and $Receipt.launches.Count -eq 2 -and $Receipt.launches[0].kind -eq 'start-menu' `
        -and $Receipt.launches[1].kind -eq 'association' -and $Receipt.launches[0].accepted -eq $true `
        -and $Receipt.launches[1].accepted -eq $true) { return 0 }
    if ($Receipt.shortcut -ne 'FAIL' -or $Receipt.association -ne 'NOT_RUN' -or $Receipt.launches.Count -ne 1) { return 1 }
    $launch = $Receipt.launches[0]
    $diagnostic = $launch.directDiagnostic
    if ($launch.kind -eq 'start-menu' -and $launch.accepted -eq $false -and $null -ne $diagnostic `
        -and $diagnostic.exited -eq $true -and $diagnostic.exitCode -eq 1 -and $diagnostic.hwnd -eq 0 `
        -and $diagnostic.stderr -ceq 'Error: OpenGL(PainterError("egui_glow requires opengl 2.0+. "))' `
        -and $Receipt.failure -eq 'start-menu did not produce an installed editor window and owned MCP listener before the deadline.') { return 2 }
    return 1
}

if (-not $IsWindows -or $env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted' `
    -or -not [Environment]::Is64BitProcess) {
    throw 'This verifier installs a product and requires a disposable Windows x64 GitHub Actions runner.'
}

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class InstalledSoul2D {
    [DllImport("msi.dll", CharSet = CharSet.Unicode)]
    public static extern int MsiQueryProductState(string product);
    [DllImport("msi.dll", CharSet = CharSet.Unicode)]
    public static extern uint MsiGetShortcutTarget(string shortcut, StringBuilder product, StringBuilder feature, StringBuilder component);
    [DllImport("msi.dll", CharSet = CharSet.Unicode)]
    public static extern int MsiGetComponentPath(string product, string component, StringBuilder path, ref uint length);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
    [DllImport("kernel32.dll")]
    private static extern bool FreeLibrary(IntPtr module);
    private delegate bool ResourceName(IntPtr module, IntPtr type, IntPtr name, IntPtr parameter);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern bool EnumResourceNames(IntPtr module, IntPtr type, ResourceName callback, IntPtr parameter);
    public static bool HasIcon(string path) {
        IntPtr module = LoadLibraryEx(path, IntPtr.Zero, 2);
        if (module == IntPtr.Zero) throw new Exception("Cannot load executable resources");
        try {
            bool found = false;
            ResourceName callback = (m, t, n, p) => { found = true; return false; };
            EnumResourceNames(module, new IntPtr(14), callback, IntPtr.Zero);
            GC.KeepAlive(callback);
            return found;
        } finally { FreeLibrary(module); }
    }
}
'@

$msiPath = (Resolve-Path -LiteralPath $Msi).Path
$fixturePath = (Resolve-Path -LiteralPath $Fixture).Path
$evidence = [IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Force $evidence | Out-Null
$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($msiPath, 0)
$view = $database.OpenView('SELECT `Value` FROM `Property` WHERE `Property`=''ProductCode''')
$view.Execute()
$productCode = $view.Fetch().StringData(1)
$view.Close()
if ([InstalledSoul2D]::MsiQueryProductState($productCode) -ne -1) {
    throw 'The product already exists; refusing to alter a pre-existing installation.'
}
$installDirectory = Join-Path $env:ProgramFiles 'Soul2D'
if (Test-Path -LiteralPath $installDirectory) { throw 'The installation directory already exists.' }
$editorPath = Join-Path $installDirectory 'soul2d.exe'
$ownedRegistryKeys = @('HKLM:\SOFTWARE\Classes\soul2d.inx',
    'HKLM:\SOFTWARE\Classes\Applications\soul2d.exe',
    'HKLM:\SOFTWARE\Classes\SystemFileAssociations\.inx\shell\edit.soul2d.exe',
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\soul2d.exe')
$knownShortcuts = @(
    @([Environment]::GetFolderPath('CommonPrograms'), [Environment]::GetFolderPath('Programs')) |
        ForEach-Object { Join-Path $_ 'Soul2D\Soul2D.lnk' }
    @([Environment]::GetFolderPath('CommonDesktopDirectory'), [Environment]::GetFolderPath('DesktopDirectory')) |
        ForEach-Object { Join-Path $_ 'Soul2D.lnk' }
)
foreach ($path in @($ownedRegistryKeys) + @($knownShortcuts)) {
    if (Test-Path -LiteralPath $path) { throw "Pre-existing product registration or shortcut: $path" }
}
$header = [IO.File]::ReadAllText((Join-Path $PSScriptRoot '../build-aux/windows/version.h'))
$expectedResources = @{ FileVersion = 'VER_FILEVERSION_STR'; ProductVersion = 'VER_PRODUCTVERSION_STR';
    LegalCopyright = 'VER_LEGALCOPYRIGHT_STR'; ProductName = 'VER_PRODUCTNAME_STR' }
foreach ($field in @($expectedResources.Keys)) {
    $macro = $expectedResources[$field]
    $match = [regex]::Match($header, ('(?m)^#define\s+' + $macro + '\s+"([^"]+)"'))
    if (-not $match.Success) { throw "Missing expected version macro $macro" }
    $expectedResources[$field] = $match.Groups[1].Value.Replace('\0', '')
}
$result = [ordered]@{ productCode = $productCode; files = @(); binaries = @(); launches = @();
    cleanup = @{ uninstallExitCode = $null; productAbsent = $false; installDirectoryAbsent = $false;
        shortcutAbsent = $false; registrationsAbsent = $false; associationAbsent = $false; errors = @() };
    installation = 'NOT_RUN'; shortcut = 'NOT_RUN'; association = 'NOT_RUN'; native = 'NOT_RUN'; failure = $null }
$ownedEditors = [Collections.Generic.List[int]]::new()
$ownedConfigs = [Collections.Generic.List[string]]::new()
$cleanupFailures = [Collections.Generic.List[string]]::new()
$oldConfig = $env:INOCHI_CONFIG_PATH
$oldBearer = $env:SOUL2D_MCP_BEARER
$installationAttempted = $false
$shortcutPath = $null

function Invoke-Msi([string[]]$MsiArguments, [string]$Log) {
    $process = Start-Process msiexec.exe -Wait -PassThru -ArgumentList ($MsiArguments + @('/qn', '/norestart', '/l*v', "`"$Log`""))
    if ($process.ExitCode -notin @(0, 3010)) { throw "msiexec failed with exit code $($process.ExitCode); see $Log" }
    return $process.ExitCode
}

function Read-Resource([string]$Uri) {
    $request = @{ jsonrpc = '2.0'; id = 1; method = 'resources/read'; params = @{ uri = $Uri } } | ConvertTo-Json -Compress
    $response = Invoke-RestMethod -Uri $script:endpoint -Method Post -ContentType 'application/json; charset=utf-8' `
        -Headers @{ Authorization = "Bearer $env:SOUL2D_MCP_BEARER" } -Body $request -TimeoutSec 5
    if ($response.PSObject.Properties.Name -contains 'error') { throw 'Editor MCP returned a resource error.' }
    return ($response.result.contents[0].text | ConvertFrom-Json -AsHashtable)
}

function Wait-Editor([string]$Kind, [string]$ShellPath, [AllowNull()][object]$ExpectedPath) {
    $config = Join-Path $env:RUNNER_TEMP "soul2d-installed-$Kind-$([guid]::NewGuid())"
    New-Item -ItemType Directory $config | Out-Null
    $ownedConfigs.Add($config)
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $env:INOCHI_CONFIG_PATH = $config
    $env:SOUL2D_MCP_BEARER = [guid]::NewGuid().ToString('N')
    $settings = @{ firstrun_complete = $true; hasDoneQuickSetup = $true; 'MCP.Enabled' = $true;
        'MCP.Host' = '127.0.0.1'; 'MCP.Port' = $port; 'MCP.authEnabled' = $true; AutosaveEnabled = $false }
    [IO.File]::WriteAllText((Join-Path $config 'settings.json'), ($settings | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
    $script:endpoint = "http://127.0.0.1:$port/mcp"
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $ShellPath
    $start.UseShellExecute = $true
    $start.WorkingDirectory = $installDirectory
    $observation = [ordered]@{ kind = $Kind; shellPath = $ShellPath; accepted = $false;
        shellPid = $null; processes = @(); httpError = $null; configFiles = @(); directDiagnostic = $null }
    $result.launches += $observation
    $started = [Diagnostics.Process]::Start($start)
    if ($null -ne $started) { $observation.shellPid = $started.Id; $started.Dispose() }
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $editor = $null
    $state = $null
    while ([DateTime]::UtcNow -lt $deadline) {
        $matches = @(Get-CimInstance Win32_Process -Filter "Name='soul2d.exe'" | Where-Object { $_.ExecutablePath -eq $editorPath })
        if ($matches.Count -gt 1) { throw 'Multiple installed editor processes make launch ownership ambiguous.' }
        if ($matches.Count -eq 1) {
            $editor = Get-Process -Id $matches[0].ProcessId
            if (-not $ownedEditors.Contains($editor.Id)) { $ownedEditors.Add($editor.Id) }
            $editor.Refresh()
            $observation.processes = @(@{ pid = $editor.Id; parentPid = $matches[0].ParentProcessId;
                sessionId = $editor.SessionId; hwnd = $editor.MainWindowHandle.ToInt64();
                visible = [InstalledSoul2D]::IsWindowVisible($editor.MainWindowHandle);
                listeners = @(Get-NetTCPConnection -OwningProcess $editor.Id -State Listen -ErrorAction SilentlyContinue |
                    Select-Object LocalAddress, LocalPort) })
            $connections = @(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $port -State Listen -ErrorAction SilentlyContinue)
            if ($connections.Count -eq 1 -and $connections[0].OwningProcess -eq $editor.Id) {
                try { $state = Read-Resource 'resource://soul2d/editor/state' } catch {
                    $observation.httpError = $_.Exception.Message.Replace($env:SOUL2D_MCP_BEARER, '[redacted]')
                    $state = $null
                }
                $editor.Refresh()
                if ($null -ne $state -and $editor.MainWindowHandle -ne [IntPtr]::Zero) { break }
            }
        }
        Start-Sleep -Milliseconds 250
    }
    if ($null -eq $state -or $null -eq $editor -or $editor.MainWindowHandle -eq [IntPtr]::Zero) {
        $observation.configFiles = @(Get-ChildItem -LiteralPath $config -Recurse -File |
            ForEach-Object { $_.FullName.Substring($config.Length + 1) })
        foreach ($editorId in $ownedEditors) {
            $process = Get-Process -Id $editorId -ErrorAction SilentlyContinue
            if ($null -ne $process -and $process.Path -eq $editorPath) { $process.Kill(); $process.WaitForExit() }
        }
        $stdout = Join-Path $evidence "$Kind-direct-stdout.log"
        $stderr = Join-Path $evidence "$Kind-direct-stderr.log"
        $probe = Start-Process -FilePath $editorPath -WorkingDirectory $installDirectory -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $ownedEditors.Add($probe.Id)
        $exited = $probe.WaitForExit(10000)
        $probe.Refresh()
        $observation.directDiagnostic = @{ pid = $probe.Id; exited = $exited;
            hwnd = $probe.MainWindowHandle.ToInt64(); exitCode = $(if ($exited) { $probe.ExitCode } else { $null }) }
        if (-not $exited) { $probe.Kill(); $probe.WaitForExit() }
        foreach ($log in @($stdout, $stderr)) {
            $text = [IO.File]::ReadAllText($log).Replace($env:SOUL2D_MCP_BEARER, '[redacted]')
            [IO.File]::WriteAllText($log, $text)
        }
        $observation.directDiagnostic.stderr = [IO.File]::ReadAllText($stderr).Trim()
        throw "$Kind did not produce an installed editor window and owned MCP listener before the deadline."
    }
    if (-not $state.editor -or $state.document.modified -or $state.document.path -ne $ExpectedPath) {
        throw "$Kind opened an unexpected document."
    }
    if ($null -ne $ExpectedPath -and $state.document.format -ne 'inx') { throw 'File association opened the wrong document format.' }
    if (-not [InstalledSoul2D]::IsWindowVisible($editor.MainWindowHandle)) { throw "$Kind window lacks visible style." }
    $snapshot = Read-Resource 'resource://vsoul/snapshot'
    if ($null -ne $ExpectedPath -and $snapshot.model.meta.name -ne $script:expectedName) {
        throw 'File association did not load the fixture identity.'
    }
    $observation.accepted = $true
    $observation.pid = $editor.Id
    $observation.executablePath = $editor.Path
    $observation.listenerOwnerPid = $connections[0].OwningProcess
    $observation.hwnd = $editor.MainWindowHandle.ToInt64()
    $observation.windowVisibleStyle = $true
    $observation.editorState = $state
    $observation.modelName = $snapshot.model.meta.name
    $quit = @{ jsonrpc = '2.0'; id = 2; method = 'tools/call'; params = @{ name = 'editor_quit'; arguments = @{} } } | ConvertTo-Json -Depth 5 -Compress
    $reply = Invoke-RestMethod -Uri $script:endpoint -Method Post -ContentType 'application/json; charset=utf-8' `
        -Headers @{ Authorization = "Bearer $env:SOUL2D_MCP_BEARER" } -Body $quit -TimeoutSec 5
    if (($reply.PSObject.Properties.Name -contains 'error') -or $reply.result.isError) { throw 'Installed editor refused clean Quit.' }
    if (-not $editor.WaitForExit(15000)) { throw 'Installed editor did not exit after clean Quit.' }
    $observation.exitCode = $editor.ExitCode
    if ($editor.ExitCode -ne 0) { throw 'Installed editor exited unsuccessfully.' }
}

try {
    $env:SOUL2D_MCP_BEARER = $null
    $installationAttempted = $true
    $result.installExitCode = Invoke-Msi @('/i', "`"$msiPath`"") (Join-Path $evidence 'install.log')
    if ($result.installExitCode -eq 3010) { throw 'The installation requires a reboot; launch acceptance is unverified.' }
    foreach ($name in @('soul2d.exe', 'soul2d-core.exe', 'njc.exe')) {
        $path = Join-Path $installDirectory $name
        $bytes = [IO.File]::ReadAllBytes($path)
        $pe = [BitConverter]::ToInt32($bytes, 60)
        $machine = [BitConverter]::ToUInt16($bytes, $pe + 4)
        $subsystem = [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
        $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($path)
        $icon = [InstalledSoul2D]::HasIcon($path)
        if ($machine -ne 0x8664 -or $subsystem -ne 3 -or -not $icon) { throw "Invalid installed PE/resources for $name" }
        foreach ($field in $expectedResources.Keys) {
            if ($version.$field -cne $expectedResources[$field]) { throw "Incorrect installed $field for $name" }
        }
        $result.binaries += @{ name = $name; machine = $machine; subsystem = $subsystem; icon = $icon;
            fileVersion = $version.FileVersion; productVersion = $version.ProductVersion;
            productName = $version.ProductName; copyright = $version.LegalCopyright }
    }
    $result.files = @(Get-ChildItem -LiteralPath $installDirectory -Recurse -File | ForEach-Object { $_.FullName.Substring($installDirectory.Length + 1) })
    python tools/verify-mcp-initialize.py (Join-Path $installDirectory 'soul2d-core.exe') --mcp
    if ($LASTEXITCODE -ne 0) { throw 'Installed core MCP initialization failed.' }
    $shortcuts = @(@([Environment]::GetFolderPath('CommonPrograms'), [Environment]::GetFolderPath('Programs')) |
        ForEach-Object { Join-Path $_ 'Soul2D\Soul2D.lnk' } | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -Unique)
    if ($shortcuts.Count -ne 1) { throw 'Expected one installed Start Menu shortcut.' }
    $shortcutPath = $shortcuts[0]
    $shortcutProduct = [Text.StringBuilder]::new(39)
    $feature = [Text.StringBuilder]::new(39)
    $component = [Text.StringBuilder]::new(39)
    if ([InstalledSoul2D]::MsiGetShortcutTarget($shortcutPath, $shortcutProduct, $feature, $component) -ne 0 `
        -or $shortcutProduct.ToString() -ne $productCode) { throw 'Shortcut does not advertise the installed product.' }
    $target = [Text.StringBuilder]::new(32768)
    [uint32]$length = $target.Capacity
    if ([InstalledSoul2D]::MsiGetComponentPath($productCode, $component.ToString(), $target, [ref]$length) -ne 3 `
        -or $target.ToString() -ne $editorPath) { throw 'Shortcut does not resolve to the installed editor component.' }
    $result.shortcutTarget = $target.ToString()
    $result.installation = 'PASS'
    $result.shortcut = 'FAIL'
    Wait-Editor 'start-menu' $shortcutPath $null
    $result.shortcut = 'PASS'
    $association = Join-Path $evidence 'installed project-日本語.inx'
    Copy-Item -LiteralPath $fixturePath -Destination $association
    $before = (Get-FileHash -LiteralPath $association).Hash
    $fixtureBytes = [IO.File]::ReadAllBytes($association)
    $jsonLength = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($fixtureBytes, 8))
    $script:expectedName = ([Text.Encoding]::UTF8.GetString($fixtureBytes, 12, $jsonLength) | ConvertFrom-Json).meta.name
    $result.association = 'FAIL'
    Wait-Editor 'association' $association $association
    if ((Get-FileHash -LiteralPath $association).Hash -ne $before) { throw 'Installed launch changed the input fixture.' }
    $result.association = 'PASS'
} catch {
    $result.failure = $_.Exception.Message
} finally {
    foreach ($editorId in $ownedEditors) {
        try {
            $process = Get-Process -Id $editorId -ErrorAction SilentlyContinue
            if ($null -ne $process -and $process.Path -eq $editorPath) { Stop-Process -Id $editorId -Force }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    $env:SOUL2D_MCP_BEARER = $null
    if ($installationAttempted) {
        try {
            $result.cleanup.uninstallExitCode = Invoke-Msi @('/x', "`"$msiPath`"") (Join-Path $evidence 'uninstall.log')
            $result.cleanup.productAbsent = [InstalledSoul2D]::MsiQueryProductState($productCode) -eq -1
            $result.cleanup.installDirectoryAbsent = -not (Test-Path -LiteralPath $installDirectory)
            $result.cleanup.shortcutAbsent = @($knownShortcuts | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0
            $result.cleanup.registrationsAbsent = @($ownedRegistryKeys | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0
            $extension = Get-Item 'HKLM:\SOFTWARE\Classes\.inx' -ErrorAction SilentlyContinue
            $result.cleanup.associationAbsent = $null -eq $extension -or $extension.GetValue('') -ne 'soul2d.inx'
            if (-not $result.cleanup.productAbsent -or -not $result.cleanup.installDirectoryAbsent -or -not $result.cleanup.shortcutAbsent `
                -or -not $result.cleanup.registrationsAbsent -or -not $result.cleanup.associationAbsent) {
                throw 'Uninstall left installed product files or shortcut state.'
            }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    foreach ($config in $ownedConfigs) {
        try { Remove-Item -LiteralPath $config -Recurse -Force } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    $result.cleanup.errors = @($cleanupFailures.ToArray())
    $env:INOCHI_CONFIG_PATH = $oldConfig
    $env:SOUL2D_MCP_BEARER = $oldBearer
    $result.exitCode = Get-VerificationExitCode $result
    $result.native = switch ($result.exitCode) { 0 { 'PASS' }; 2 { 'UNSUPPORTED' }; default { 'FAIL' } }
    $result | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $evidence 'result.json') -Encoding utf8NoBOM
}
if ($result.exitCode -eq 2) {
    Write-Output 'UNSUPPORTED: hosted Windows OpenGL is below 2.0. Installation checks passed; native acceptance remains unmet.'
    exit 2
}
if ($result.exitCode -ne 0) { throw 'Installed Windows acceptance failed; see result.json and MSI logs.' }
Write-Output 'PASS: installed x64 resources, advertised shortcut, INX association and uninstall.'
