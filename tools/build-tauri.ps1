#requires -Version 7.0
<#
.SYNOPSIS
Build a committed, clean Windows Tauri release without installing or launching the panel.
.EXAMPLE
pwsh -File tools/build-tauri.ps1 -OutputRoot D:/Releases/ClashTrafficMonitor -TargetDir D:/BuildCache/clash-target -SkipDependencyInstall
.NOTES
SkipDependencyInstall preserves an existing node_modules junction. Otherwise npm ci
recreates node_modules, so junctions require the explicit skip switch. Rust's target
directory is a reusable build cache; every deliverable is copied into a fresh release
directory under OutputRoot. No directory is deleted and no installed app is changed.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$OutputRoot,
    [string]$TargetDir,
    [switch]$SkipDependencyInstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$ProjectDir = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$Utf8 = [Text.UTF8Encoding]::new($false)
$LogPath = $null
$ManifestPath = $null
$Manifest = $null
$SavedEnvironment = @{}

function Invoke-Text([string]$Command, [Parameter(ValueFromRemainingArguments)][string[]]$Arguments) {
    $result = @(& $Command @Arguments 2>&1)
    $exitCode = $LASTEXITCODE
    # PowerShell 7 preserves redirected native stderr as ErrorRecord objects.
    # Keep those diagnostics visible and logged without polluting machine stdout.
    foreach ($diagnostic in $result | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] }) {
        $line = $diagnostic.ToString()
        [Console]::Error.WriteLine($line)
        if ($LogPath) { [IO.File]::AppendAllText($LogPath, "[stderr] $line`n", $Utf8) }
    }
    if ($exitCode -ne 0) { throw "Command failed ($exitCode): $Command $($Arguments -join ' ')`n$($result -join "`n")" }
    $stdout = @($result | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] })
    return ($stdout -join "`n").Trim()
}
function Invoke-Logged([string]$Command, [Parameter(ValueFromRemainingArguments)][string[]]$Arguments) {
    $description = "$Command $($Arguments -join ' ')"
    [IO.File]::AppendAllText($LogPath, "`n> $description`n", $Utf8)
    Write-Host "> $description"
    & $Command @Arguments 2>&1 | ForEach-Object {
        $line = $_.ToString()
        [IO.File]::AppendAllText($LogPath, "$line`n", $Utf8)
        Write-Host $line
    }
    if ($LASTEXITCODE -ne 0) { throw "Command failed ($LASTEXITCODE): $description" }
}
function Invoke-Git([Parameter(ValueFromRemainingArguments)][string[]]$Arguments) {
    return Invoke-Text $Git (@('-C', $ProjectDir) + $Arguments)
}
function Assert-Clean {
    $state = Invoke-Git @('status', '--porcelain=v1', '--untracked-files=all')
    if ($state) { throw "Release requires a committed, clean checkout (ignored build output is allowed):`n$state" }
}
function Assert-OutputLocation([string]$Path) {
    $relative = [IO.Path]::GetRelativePath($ProjectDir, $Path)
    $inside = $relative -eq '.' -or (-not [IO.Path]::IsPathRooted($relative) -and $relative -ne '..' -and -not $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)"))
    if ($inside) {
        & $Git -C $ProjectDir check-ignore --quiet -- $Path
        if ($LASTEXITCODE -ne 0) { throw "Output/cache inside the source checkout must be ignored by Git: $Path" }
    }
}
function Write-Json([string]$Path, $Value) {
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), $Utf8)
}
function File-Record([string]$Path) {
    $item = Get-Item -LiteralPath $Path
    return [ordered]@{ file = $item.Name; bytes = $item.Length; sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
}
function Set-BuildEnvironment([string]$Name, [string]$Value) {
    $SavedEnvironment[$Name] = [Environment]::GetEnvironmentVariable($Name, 'Process')
    [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
}
function Read-BuildInfo([string]$Executable, [string]$DataDirectory) {
    # Redirected output lets the Windows-subsystem binary report diagnostics
    # without opening its panel, contacting Mihomo, or touching the real database.
    $info = [Diagnostics.ProcessStartInfo]::new($Executable)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $Utf8
    $info.StandardErrorEncoding = $Utf8
    $info.WorkingDirectory = $ProjectDir
    foreach ($arg in @('--data-dir', $DataDirectory, '--diagnostics')) { $info.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    try {
        if (-not $process.Start()) { throw 'Unable to start the build diagnostics.' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw 'Build diagnostics timed out; only this newly started diagnostic process was stopped.'
        }
        $text = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw "Build diagnostics failed: $errorText" }
        [IO.File]::WriteAllText((Join-Path $ReleaseDir 'build-info.json'), $text, $Utf8)
        return $text | ConvertFrom-Json
    } finally { $process.Dispose() }
}

Push-Location -LiteralPath $ProjectDir
try {
    $Git = (Get-Command git -CommandType Application -ErrorAction Stop).Source
    $Npm = (Get-Command npm.cmd -CommandType Application -ErrorAction Stop).Source
    $Node = (Get-Command node -CommandType Application -ErrorAction Stop).Source
    $Cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
    $Rustc = (Get-Command rustc -CommandType Application -ErrorAction Stop).Source
    $root = [IO.Path]::GetFullPath((Invoke-Git @('rev-parse', '--show-toplevel')))
    if (-not $root.Equals($ProjectDir, [StringComparison]::OrdinalIgnoreCase)) { throw "Unexpected repository root: $root" }
    $Branch = Invoke-Git @('symbolic-ref', '--quiet', '--short', 'HEAD')
    $Commit = Invoke-Git @('rev-parse', '--verify', 'HEAD^{commit}')
    Assert-Clean
    $null = Invoke-Git @('ls-files', '--error-unmatch', 'package-lock.json', 'Cargo.lock')
    $OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
    if (-not $TargetDir) { $TargetDir = Join-Path $OutputRoot 'cargo-target' }
    $TargetDir = [IO.Path]::GetFullPath($TargetDir)
    Assert-OutputLocation $OutputRoot
    Assert-OutputLocation $TargetDir
    $modules = Get-Item -LiteralPath (Join-Path $ProjectDir 'node_modules') -ErrorAction SilentlyContinue
    if (-not $SkipDependencyInstall -and $modules -and ($modules.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'node_modules is a junction/symlink. Use -SkipDependencyInstall after verifying its locked dependencies; npm ci would replace this link.'
    }
    New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
    $ReleaseDir = Join-Path $OutputRoot ("{0}-{1}-{2}" -f [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfffZ'), $Commit.Substring(0, 12), [Guid]::NewGuid().ToString('N').Substring(0, 8))
    # A collision must fail, never reuse or empty an earlier release directory.
    New-Item -ItemType Directory -Path $ReleaseDir -ErrorAction Stop | Out-Null
    $WebDir = Join-Path $ReleaseDir 'web'
    New-Item -ItemType Directory -Path $WebDir -ErrorAction Stop | Out-Null
    $LogPath = Join-Path $ReleaseDir 'build.log'
    $ManifestPath = Join-Path $ReleaseDir 'release.json'
    [IO.File]::WriteAllText($LogPath, '', $Utf8)
    $PackageLockHash = (Get-FileHash -LiteralPath package-lock.json -Algorithm SHA256).Hash.ToLowerInvariant()
    $CargoLockHash = (Get-FileHash -LiteralPath Cargo.lock -Algorithm SHA256).Hash.ToLowerInvariant()
    $Manifest = [ordered]@{
        schemaVersion = 1; state = 'building'; startedAtUtc = [DateTime]::UtcNow.ToString('o')
        gitCommit = $Commit; gitBranch = $Branch; gitDirty = $false; repositoryRoot = $ProjectDir
        packageLockSha256 = $PackageLockHash; cargoLockSha256 = $CargoLockHash
        dependencyInstallSkipped = [bool]$SkipDependencyInstall; runtime = 'rust-tauri'
        releaseDirectory = $ReleaseDir; frontendDist = $WebDir; cargoTargetDir = $TargetDir
        sourceArchiveMethod = 'git archive --format=zip HEAD'; artifacts = @()
    }
    Write-Json $ManifestPath $Manifest
    $TemporaryDir = Join-Path $ReleaseDir 'tmp'
    New-Item -ItemType Directory -Path $TemporaryDir -ErrorAction Stop | Out-Null
    Set-BuildEnvironment 'TEMP' $TemporaryDir
    Set-BuildEnvironment 'TMP' $TemporaryDir
    Set-BuildEnvironment 'npm_config_cache' (Join-Path $OutputRoot 'npm-cache')
    Set-BuildEnvironment 'CLASH_FRONTEND_DIST' $WebDir
    Set-BuildEnvironment 'CLASH_BUILD_COMMIT' $Commit
    Set-BuildEnvironment 'CARGO_TARGET_DIR' $TargetDir
    Set-BuildEnvironment 'CARGO_INCREMENTAL' '0'
    if (-not $SkipDependencyInstall) { Invoke-Logged $Npm @('ci', '--include=dev', '--no-audit', '--no-fund') }
    Invoke-Logged $Npm @('ls', '--all', '--json')
    # npm ls accepts semver-compatible transitive versions. Verify the physical
    # package locations and exact lockfile versions even when reusing a junction.
    Invoke-Logged $Node @((Join-Path $PSScriptRoot 'verify_locked_dependencies.mjs'), $ProjectDir)
    $Tauri = Join-Path $ProjectDir 'node_modules/.bin/tauri.cmd'
    if (-not (Test-Path -LiteralPath $Tauri -PathType Leaf)) { throw 'The locked local Tauri CLI is missing.' }
    $RustVersion = Invoke-Text $Rustc @('-vV')
    $HostTarget = [regex]::Match($RustVersion, '(?m)^host:\s*(\S+)').Groups[1].Value
    if (-not $HostTarget.EndsWith('-windows-msvc')) { throw "A Windows MSVC Rust toolchain is required: $HostTarget" }
    $Manifest.effectiveBuildTarget = if ($env:CARGO_BUILD_TARGET) { $env:CARGO_BUILD_TARGET } else { $HostTarget }
    if (-not $Manifest.effectiveBuildTarget.EndsWith('-windows-msvc')) { throw 'The effective Cargo build target must be Windows MSVC.' }
    $Manifest.versions = [ordered]@{
        git = Invoke-Text $Git @('--version'); node = Invoke-Text $Node @('--version')
        npm = Invoke-Text $Npm @('--version'); cargo = Invoke-Text $Cargo @('--version')
        rustc = $RustVersion; tauri = Invoke-Text $Tauri @('--version')
    }
    $Manifest.tools = [ordered]@{ git = $Git; node = $Node; npm = $Npm; cargo = $Cargo; rustc = $Rustc; tauri = $Tauri }
    $OverridePath = Join-Path $ReleaseDir 'tauri.build.json'
    # A drive-letter string is parsed as a URL by Tauri's untagged config.
    # The array selects Files and recursively embeds the directory instead.
    Write-Json $OverridePath ([ordered]@{ build = [ordered]@{ frontendDist = @($WebDir) } })
    $Manifest.configSources = @(
        [ordered]@{ path = 'src-tauri/tauri.conf.json'; sha256 = (Get-FileHash -LiteralPath src-tauri/tauri.conf.json -Algorithm SHA256).Hash.ToLowerInvariant() },
        [ordered]@{ path = $OverridePath; sha256 = (Get-FileHash -LiteralPath $OverridePath -Algorithm SHA256).Hash.ToLowerInvariant() }
    )
    $WindowsConfig = Join-Path $ProjectDir 'src-tauri/tauri.windows.conf.json'
    if (Test-Path -LiteralPath $WindowsConfig -PathType Leaf) {
        $Manifest.configSources += [ordered]@{ path = 'src-tauri/tauri.windows.conf.json'; sha256 = (Get-FileHash -LiteralPath $WindowsConfig -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    # The single NSIS bundle has no updater. Preserve identical executable bytes
    # in Cargo output, the standalone delivery, and the NSIS-installed program.
    $BuildArguments = @('build', '--ci', '--bundles', 'nsis', '--no-binary-patching', '--config', $OverridePath, '--', '--locked')
    $Manifest.binaryPatching = $false
    $Manifest.buildCommand = [ordered]@{ executable = $Tauri; arguments = $BuildArguments }
    Write-Json $ManifestPath $Manifest
    Assert-Clean
    $BuildStarted = [DateTime]::UtcNow
    Invoke-Logged $Tauri $BuildArguments
    if (-not (Test-Path -LiteralPath (Join-Path $WebDir 'index.html') -PathType Leaf)) { throw 'The fresh frontend build did not produce index.html.' }
    $ReleaseTarget = if ($env:CARGO_BUILD_TARGET) { Join-Path $TargetDir "$($env:CARGO_BUILD_TARGET)/release" } else { Join-Path $TargetDir 'release' }
    $Binary = Join-Path $ReleaseTarget 'ClashTrafficMonitor.exe'
    if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) { throw "Expected release binary is missing: $Binary" }
    $Installers = @(Get-ChildItem -LiteralPath (Join-Path $ReleaseTarget 'bundle/nsis') -File -Filter '*.exe' | Where-Object { $_.LastWriteTimeUtc -ge $BuildStarted.AddSeconds(-2) })
    if (-not $Installers.Count) { throw 'No freshly generated NSIS installer was found; refusing a stale bundle.' }
    $DeliveryDir = Join-Path $ReleaseDir 'windows'
    New-Item -ItemType Directory -Path $DeliveryDir -ErrorAction Stop | Out-Null
    $DeliveryBinary = Join-Path $DeliveryDir 'ClashTrafficMonitor.exe'
    Copy-Item -LiteralPath $Binary -Destination $DeliveryBinary
    foreach ($installer in $Installers) { Copy-Item -LiteralPath $installer.FullName -Destination (Join-Path $DeliveryDir $installer.Name) }
    if ((Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $DeliveryBinary -Algorithm SHA256).Hash) { throw 'The delivered executable hash differs from the Cargo release binary.' }
    $BuildInfo = Read-BuildInfo $DeliveryBinary (Join-Path $ReleaseDir 'diagnostic-no-database')
    if ($BuildInfo.build_commit -ne $Commit -or $BuildInfo.build_dirty -ne $false -or $BuildInfo.runtime -ne 'rust-tauri' -or $BuildInfo.frontend_mode -ne 'embedded' -or $BuildInfo.embedded_index -ne $true -or $BuildInfo.embedded_script -ne $true) {
        throw 'The built executable did not report this clean HEAD commit, the Rust Tauri runtime and embedded frontend assets.'
    }
    if ((Invoke-Git @('rev-parse', 'HEAD')) -ne $Commit) { throw 'HEAD changed during the build.' }
    Assert-Clean
    if ((Get-FileHash -LiteralPath package-lock.json -Algorithm SHA256).Hash.ToLowerInvariant() -ne $PackageLockHash -or (Get-FileHash -LiteralPath Cargo.lock -Algorithm SHA256).Hash.ToLowerInvariant() -ne $CargoLockHash) { throw 'A lockfile changed during the build.' }
    $SourceZip = Join-Path $ReleaseDir 'clash-traffic-monitor-source.zip'
    Invoke-Logged $Git @('-C', $ProjectDir, 'archive', '--format=zip', "--output=$SourceZip", 'HEAD')
    if ((Invoke-Git @('rev-parse', 'HEAD')) -ne $Commit) { throw 'HEAD changed while creating the source archive.' }
    $ArtifactFiles = @($DeliveryBinary) + @($Installers | ForEach-Object { Join-Path $DeliveryDir $_.Name }) + @($SourceZip, (Join-Path $ReleaseDir 'build-info.json'))
    $Manifest.artifacts = @($ArtifactFiles | ForEach-Object { File-Record $_ })
    $Manifest.buildInfoSource = 'windows/ClashTrafficMonitor.exe --data-dir <isolated missing directory> --diagnostics'
    $Manifest.buildInfo = $BuildInfo
    $Manifest.state = 'complete'
    $Manifest.completedAtUtc = [DateTime]::UtcNow.ToString('o')
    Write-Json $ManifestPath $Manifest
    Write-Host "Release verified: $ReleaseDir"
    Write-Host "Manifest: $ManifestPath"
} catch {
    if ($Manifest -and $ManifestPath) {
        $Manifest.state = 'failed'
        $Manifest.error = $_.Exception.Message
        Write-Json $ManifestPath $Manifest
    }
    throw
} finally {
    foreach ($entry in $SavedEnvironment.GetEnumerator()) { [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process') }
    Pop-Location
}
