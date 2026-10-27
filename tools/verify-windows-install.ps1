param(
    [Parameter(Mandatory)][string]$Msi,
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [string]$SoftwareGlArchive,
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
        -and $Receipt.launches[1].accepted -eq $true -and $Receipt.editorStdio -eq 'PASS') {
        if ($Receipt.graphicsContext -notin @('system', 'test-software-wgl')) { return 1 }
        foreach ($launch in $Receipt.launches) {
            if ($launch.exitCode -ne 0 -or $launch.paint.status -ne 'PASS' -or $launch.paint.restored -ne $true `
                -or $launch.console.attached -ne $true -or $launch.console.detached -ne $true `
                -or $launch.console.processIds -notcontains $launch.pid) { return 1 }
            if ($Receipt.graphicsContext -eq 'test-software-wgl' -and $launch.softwareModule.verified -ne $true) { return 1 }
        }
        if ($Receipt.graphicsContext -eq 'test-software-wgl' -and ($Receipt.graphicsFixture.verified -ne $true `
            -or $Receipt.cleanup.fixtureRemoved -ne $true -or $Receipt.cleanup.inventoryRestored -ne $true)) { return 1 }
        return 0
    }
    if ($Receipt.graphicsContext -ne 'system') { return 1 }
    if ($Receipt.shortcut -ne 'FAIL' -or $Receipt.association -ne 'NOT_RUN' -or $Receipt.launches.Count -ne 1) { return 1 }
    $launch = $Receipt.launches[0]
    $diagnostic = $launch.directDiagnostic
    if ($launch.kind -eq 'start-menu' -and $launch.accepted -eq $false -and $null -ne $diagnostic `
        -and $diagnostic.exited -eq $true -and $diagnostic.exitCode -eq 1 -and $diagnostic.hwnd -eq 0 `
        -and $diagnostic.stderr -ceq 'Error: OpenGL(PainterError("egui_glow requires opengl 2.0+. "))' `
        -and $Receipt.failure -eq 'start-menu did not produce an installed editor window and owned MCP listener before the deadline.') { return 2 }
    return 1
}

function Get-ObservedProcess([int]$ProcessId) {
    $process = Get-Process -Id $ProcessId
    # PID-discovered processes need a retained handle to read ExitCode after they exit.
    if ($process.get_Handle() -eq [IntPtr]::Zero) { throw 'Cannot retain the observed process handle.' }
    return $process
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
Add-Type -Path (Join-Path $PSScriptRoot 'windows-editor-observation.cs')

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
$ownedRegistryValues = @(
    @{ path = 'HKCU:\Software\Soul2D contributors\Soul2D'; name = '' }
    @{ path = 'HKCU:\Software\Microsoft\soul2d'; name = 'installed' }
)
function Test-OwnedRegistryValues {
    foreach ($value in $ownedRegistryValues) {
        $key = Get-Item -LiteralPath $value.path -ErrorAction SilentlyContinue
        if ($null -ne $key -and $key.GetValueNames() -contains $value.name) { return $true }
    }
    return $false
}
if (Test-OwnedRegistryValues) { throw 'Pre-existing product marker values; refusing to change them.' }
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
        shortcutAbsent = $false; registrationsAbsent = $false; associationAbsent = $false;
        fixtureRemoved = $false; inventoryRestored = $false; errors = @() };
    sourceRevision = $env:GITHUB_SHA; msiSha256 = (Get-FileHash -LiteralPath $msiPath).Hash;
    graphicsContext = $(if ($SoftwareGlArchive) { 'test-software-wgl' } else { 'system' }); graphicsFixture = $null;
    installation = 'NOT_RUN'; shortcut = 'NOT_RUN'; association = 'NOT_RUN'; editorStdio = 'NOT_RUN';
    native = 'NOT_RUN'; softwareGl = 'NOT_RUN'; failure = $null }
$ownedEditors = [Collections.Generic.List[int]]::new()
$ownedConfigs = [Collections.Generic.List[string]]::new()
$cleanupFailures = [Collections.Generic.List[string]]::new()
$oldConfig = $env:INOCHI_CONFIG_PATH
$oldBearer = $env:SOUL2D_MCP_BEARER
$installationAttempted = $false
$shortcutPath = $null
$stagedDll = $null
$originalInventory = $null
$softwareDll = $null

function Get-InstalledInventory {
    $inventory = [ordered]@{}
    Get-ChildItem -LiteralPath $installDirectory -Recurse -File | Sort-Object FullName | ForEach-Object {
        $inventory[$_.FullName.Substring($installDirectory.Length + 1)] = (Get-FileHash -LiteralPath $_.FullName).Hash
    }
    return ($inventory | ConvertTo-Json -Compress)
}

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

function Invoke-EditorTool([string]$Name, [hashtable]$Arguments) {
    $request = @{ jsonrpc = '2.0'; id = 2; method = 'tools/call'; params = @{ name = $Name; arguments = $Arguments } } |
        ConvertTo-Json -Depth 10 -Compress
    $response = Invoke-RestMethod -Uri $script:endpoint -Method Post -ContentType 'application/json; charset=utf-8' `
        -Headers @{ Authorization = "Bearer $env:SOUL2D_MCP_BEARER" } -Body $request -TimeoutSec 5
    if (($response.PSObject.Properties.Name -contains 'error') -or $response.result.isError) { throw "Editor refused $Name." }
    return $response.result.structuredContent
}

function Observe-Paint($Editor, [string]$Kind, $Observation) {
    $state = Read-Resource 'resource://soul2d/editor/state'
    $snapshot = Read-Resource 'resource://vsoul/snapshot' | ConvertTo-Json -Depth 100 -Compress
    $background = @($state.viewport.background)
    $Observation.paint = [ordered]@{ status = 'FAIL'; restored = $false; changedPixels = 0;
        minimumChangedPixels = 1000; rgbTolerance = 3 }
    $a = [int[]]@(239, 31, 151); $b = [int[]]@(23, 211, 67)
    try {
        [void][EditorDesktop]::ShowWindow($Editor.MainWindowHandle, 9)
        [void][EditorDesktop]::SetForegroundWindow($Editor.MainWindowHandle)
        [void](Invoke-EditorTool 'editor_viewport_set' @{ background = $a })
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        $first = $null
        do {
            Start-Sleep -Milliseconds 250
            $first = [EditorDesktop]::Capture($Editor.MainWindowHandle, [uint32]$Editor.Id)
        } while ([EditorDesktop]::CountColor($first, $a) -lt 1000 -and [DateTime]::UtcNow -lt $deadline)
        $firstPath = Join-Path $evidence "$Kind-background-a.bmp"
        $first.Save($firstPath)
        if ([EditorDesktop]::CountColor($first, $a) -lt 1000) { throw 'Desktop did not present background A.' }
        [void](Invoke-EditorTool 'editor_viewport_set' @{ background = $b })
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            Start-Sleep -Milliseconds 250
            $second = [EditorDesktop]::Capture($Editor.MainWindowHandle, [uint32]$Editor.Id)
            $Observation.paint.changedPixels = [EditorDesktop]::CountTransitions($first, $second, $a, $b)
        } while ($Observation.paint.changedPixels -lt 1000 -and [DateTime]::UtcNow -lt $deadline)
        $secondPath = Join-Path $evidence "$Kind-background-b.bmp"
        $second.Save($secondPath)
        if ($Observation.paint.changedPixels -lt 1000) { throw 'The same desktop pixels did not respond to background B.' }
        $Observation.paint.bounds = @{ left = $first.Left; top = $first.Top; width = $first.Width; height = $first.Height }
        $Observation.paint.colors = @($a, $b)
        $Observation.paint.frames = @($firstPath, $secondPath) | ForEach-Object {
            @{ file = [IO.Path]::GetFileName($_); sha256 = (Get-FileHash -LiteralPath $_).Hash }
        }
        $Observation.paint.status = 'PASS'
    } finally {
        [void](Invoke-EditorTool 'editor_viewport_set' @{ background = $background })
        $after = Read-Resource 'resource://soul2d/editor/state'
        $afterSnapshot = Read-Resource 'resource://vsoul/snapshot' | ConvertTo-Json -Depth 100 -Compress
        if (($after.viewport.background | ConvertTo-Json -Compress) -ne ($background | ConvertTo-Json -Compress) `
            -or ($after.document | ConvertTo-Json -Depth 20 -Compress) -ne ($state.document | ConvertTo-Json -Depth 20 -Compress) `
            -or $afterSnapshot -cne $snapshot) { throw 'Paint observation failed to restore view state or changed the document.' }
        $Observation.paint.restored = $true
    }
}

function Observe-Console($Editor, [string]$Kind) {
    $path = Join-Path $evidence "$Kind-console.json"
    $helper = Join-Path $PSScriptRoot 'observe-windows-console.ps1'
    $observer = Start-Process pwsh -PassThru -NoNewWindow -ArgumentList @('-NoProfile', '-File', "`"$helper`"", `
        '-EditorPid', $Editor.Id, '-OutputPath', "`"$path`"")
    if (-not $observer.WaitForExit(15000)) {
        $observer.Kill(); $observer.WaitForExit()
        throw 'Console observer timed out.'
    }
    if ($observer.ExitCode -ne 0) { throw 'Console observation failed.' }
    return (Get-Content -Raw -LiteralPath $path | ConvertFrom-Json -AsHashtable)
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
        'MCP.Host' = '127.0.0.1'; 'MCP.Port' = $port; 'MCP.authEnabled' = $true; AutosaveEnabled = $false;
        WinW = 900; WinH = 600; WinMax = $false }
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
    $observation.pid = $editor.Id
    $observation.executablePath = $editor.Path
    $observation.listenerOwnerPid = $connections[0].OwningProcess
    $observation.hwnd = $editor.MainWindowHandle.ToInt64()
    $observation.windowVisibleStyle = $true
    $observation.editorState = $state
    $observation.modelName = $snapshot.model.meta.name
    if ($SoftwareGlArchive) {
        $modules = @($editor.Modules | Where-Object { $_.ModuleName -ieq 'opengl32.dll' })
        if ($modules.Count -ne 1 -or $modules[0].FileName -ine $stagedDll `
            -or (Get-FileHash -LiteralPath $modules[0].FileName).Hash -ine $result.graphicsFixture.dllSha256) {
            throw 'Installed editor did not load the pinned application-local software GL module.'
        }
        $observation.softwareModule = @{ verified = $true; path = $modules[0].FileName;
            sha256 = (Get-FileHash -LiteralPath $modules[0].FileName).Hash }
    }
    $observation.console = Observe-Console $editor $Kind
    Observe-Paint $editor $Kind $observation
    $editor = Get-ObservedProcess $editor.Id
    try {
        $reply = Invoke-EditorTool 'editor_quit' @{}
        if ($reply.quit.state -ne 'accepted') { throw 'Installed editor did not acknowledge clean Quit.' }
        if (-not $editor.WaitForExit(15000)) { throw 'Installed editor did not exit after clean Quit.' }
        $observation.exitCode = $editor.get_ExitCode()
        if ($observation.exitCode -ne 0) { throw 'Installed editor exited unsuccessfully.' }
    } finally { $editor.Dispose() }
    $observation.accepted = $true
}

try {
    $env:SOUL2D_MCP_BEARER = $null
    if ($SoftwareGlArchive) {
        $pin = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'windows-software-gl.json') | ConvertFrom-Json -AsHashtable
        $archive = (Resolve-Path -LiteralPath $SoftwareGlArchive).Path
        if ((Get-FileHash -LiteralPath $archive).Hash -ine $pin.archiveSha256) { throw 'Software GL archive hash mismatch.' }
        $temporary = Join-Path $env:RUNNER_TEMP "soul2d-software-gl-$([guid]::NewGuid())"
        New-Item -ItemType Directory $temporary | Out-Null
        $ownedConfigs.Add($temporary)
        & 7z x $archive "-o$temporary" opengl32.dll -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Cannot extract pinned software GL fixture.' }
        $softwareDll = Join-Path $temporary 'opengl32.dll'
        $bytes = [IO.File]::ReadAllBytes($softwareDll)
        $pe = [BitConverter]::ToInt32($bytes, 60)
        if ((Get-FileHash -LiteralPath $softwareDll).Hash -ine $pin.dllSha256 `
            -or [BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664) { throw 'Software GL DLL identity mismatch.' }
        $result.graphicsFixture = $pin
        $result.graphicsFixture.verified = $true
    }
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
    $originalInventory = Get-InstalledInventory
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
    if ($SoftwareGlArchive) {
        $destination = Join-Path $installDirectory 'opengl32.dll'
        if (Test-Path -LiteralPath $destination) { throw 'Refusing to replace an installed GL module.' }
        $stagedDll = $destination
        Copy-Item -LiteralPath $softwareDll -Destination $stagedDll
        if ((Get-FileHash -LiteralPath $stagedDll).Hash -ine $result.graphicsFixture.dllSha256) { throw 'Staged GL fixture hash mismatch.' }
    }
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
    $config = Join-Path $env:RUNNER_TEMP "soul2d-installed-stdio-$([guid]::NewGuid())"
    New-Item -ItemType Directory $config | Out-Null
    $ownedConfigs.Add($config)
    $env:INOCHI_CONFIG_PATH = $config
    $env:SOUL2D_MCP_BEARER = $null
    @{ firstrun_complete = $true; hasDoneQuickSetup = $true; 'MCP.Enabled' = $false; AutosaveEnabled = $false;
        WinW = 900; WinH = 600; WinMax = $false } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $config 'settings.json') -Encoding utf8NoBOM
    python tools/verify-mcp-initialize.py --editor $editorPath --mcp > (Join-Path $evidence 'editor-stdio.log') 2>&1
    if ($LASTEXITCODE -ne 0) { throw 'Installed editor stdio initialize/state/quit failed.' }
    $result.editorStdio = 'PASS'
} catch {
    $result.failure = $_.Exception.Message
} finally {
    foreach ($editorId in $ownedEditors) {
        try {
            $process = Get-Process -Id $editorId -ErrorAction SilentlyContinue
            if ($null -ne $process -and $process.Path -eq $editorPath) { $process.Kill(); $process.WaitForExit() }
        } catch { $cleanupFailures.Add($_.Exception.Message) }
    }
    try {
        if ($null -ne $stagedDll -and (Test-Path -LiteralPath $stagedDll)) { Remove-Item -LiteralPath $stagedDll -Force }
        $result.cleanup.fixtureRemoved = $null -eq $stagedDll -or -not (Test-Path -LiteralPath $stagedDll)
        if ($null -ne $originalInventory) {
            $result.cleanup.inventoryRestored = (Get-InstalledInventory) -ceq $originalInventory
            if (-not $result.cleanup.inventoryRestored) { throw 'Installed file inventory changed before uninstall.' }
        }
    } catch { $cleanupFailures.Add($_.Exception.Message) }
    $env:SOUL2D_MCP_BEARER = $null
    if ($installationAttempted) {
        try {
            $result.cleanup.uninstallExitCode = Invoke-Msi @('/x', "`"$msiPath`"") (Join-Path $evidence 'uninstall.log')
            $result.cleanup.productAbsent = [InstalledSoul2D]::MsiQueryProductState($productCode) -eq -1
            $result.cleanup.installDirectoryAbsent = -not (Test-Path -LiteralPath $installDirectory)
            $result.cleanup.shortcutAbsent = @($knownShortcuts | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0
            $result.cleanup.registrationsAbsent = @($ownedRegistryKeys | Where-Object { Test-Path -LiteralPath $_ }).Count -eq 0 `
                -and -not (Test-OwnedRegistryValues)
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
    if ($SoftwareGlArchive) {
        $result.softwareGl = $(if ($result.exitCode -eq 0) { 'PASS' } else { 'FAIL' })
    } else {
        $result.native = switch ($result.exitCode) { 0 { 'PASS' }; 2 { 'UNSUPPORTED' }; default { 'FAIL' } }
    }
    $result | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath (Join-Path $evidence 'result.json') -Encoding utf8NoBOM
}
if ($result.exitCode -eq 2) {
    Write-Output 'UNSUPPORTED: hosted Windows OpenGL is below 2.0. Installation checks passed; native acceptance remains unmet.'
    exit 2
}
if ($result.exitCode -ne 0) { throw 'Installed Windows acceptance failed; see result.json and MSI logs.' }
Write-Output 'PASS: installed x64 resources, advertised shortcut, INX association and uninstall.'
