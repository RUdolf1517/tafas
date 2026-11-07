# tafas — integration test for packaging/windows/install.ps1.
#
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\packaging\windows\tests\Install.Integration.ps1
#
# The installer is dot-sourced, so every case drives the real `Invoke-TafasInstallCore`
# with only its three seams replaced: the release-URL resolver, the downloader and
# the directory move. Nothing here reaches the network: the "release" is a ZIP and a
# `SHA256SUMS` built in the temp directory from a stub executable that prints the
# version it was compiled with. That is deliberate — an integration test that needs
# a published release cannot guard the installer before the first release exists,
# and a test that needs the network cannot tell a broken installer from a bad day
# at GitHub.
#
# The stub is not tafas: the installer only ever asks an exe for `--version`, and the
# case `real-binary-version-format` separately proves the real release binary answers
# in the shape the installer parses.
#
# Everything happens inside a throwaway profile in the temp directory. The user PATH
# of the real account *is* written (that is the feature under test), so it is
# snapshotted first and restored in `finally` even when a case fails.

[CmdletBinding()]
param(
    [string]$SourceExecutable,
    [string]$EvidencePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:RepoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)))
$script:InstallScript = Join-Path $script:RepoRoot 'packaging\windows\install.ps1'
$script:ArchiveName = 'tafas-x86_64-pc-windows-msvc.zip'
$script:ChecksumsName = 'SHA256SUMS'
$script:Passed = 0
$sandbox = $null
$script:Failed = New-Object 'System.Collections.Generic.List[string]'
$script:CaseLog = New-Object 'System.Collections.Generic.List[string]'
$script:BaselineUserPath = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
$script:OriginalProfile = $env:USERPROFILE
$script:OriginalLocalAppData = $env:LOCALAPPDATA

function Write-CaseLine {
    param([string]$Line)
    $script:CaseLog.Add($Line)
    Write-Host $Line
}

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "assertion failed: $Message" }
}

function Assert-Equal {
    param($Expected, $Actual, [string]$Message)
    if ("$Expected" -cne "$Actual") {
        throw "assertion failed: $Message (expected '$Expected', got '$Actual')"
    }
}

function Assert-Match {
    param([string]$Text, [string]$Pattern, [string]$Message)
    if (-not ($Text -match $Pattern)) {
        throw "assertion failed: $Message (no match for '$Pattern' in '$Text')"
    }
}

function Assert-Throws {
    param([scriptblock]$Action, [string]$Pattern, [string]$Message)
    try {
        & $Action
    }
    catch {
        $text = $_.Exception.Message
        if (-not ($text -match $Pattern)) {
            throw "assertion failed: $Message (refused, but for another reason: $text)"
        }
        Write-CaseLine "    refused as expected: $text"
        return
    }
    throw "assertion failed: $Message (no error was raised)"
}

function New-StubExecutable {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Version)

    $className = 'TafasStub' + ($Version -replace '[^0-9]', '') + ([guid]::NewGuid().ToString('N').Substring(0, 8))
    $source = @"
using System;
public static class $className
{
    public static int Main(string[] args)
    {
        if (args.Length >= 1 && args[0] == "--version")
        {
            Console.WriteLine("tafas $Version");
            return 0;
        }
        Console.Error.WriteLine("stub: only --version is implemented");
        return 1;
    }
}
"@
    Add-Type -TypeDefinition $source -OutputAssembly $Path -OutputType ConsoleApplication
    Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) "stub executable was compiled to $Path"
}

function New-ReleaseFixture {
    param(
        [Parameter(Mandatory)][string]$Directory,
        [Parameter(Mandatory)][string]$Tag,
        [Parameter(Mandatory)][string]$Version,
        [string[]]$ExtraEntries = @(),
        [string]$ChecksumsContent
    )

    $stage = Join-Path $Directory ("stage-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $exe = Join-Path $stage 'tafas.exe'
    New-StubExecutable -Path $exe -Version $Version
    [System.IO.File]::WriteAllText((Join-Path $stage 'LICENSE'), "MIT License`n`nCopyright (c) tafas`n")
    foreach ($entry in $ExtraEntries) {
        [System.IO.File]::WriteAllText((Join-Path $stage $entry), "extra`n")
    }

    $tagDirectory = Join-Path $Directory $Tag
    New-Item -ItemType Directory -Force -Path $tagDirectory | Out-Null
    $archivePath = Join-Path $tagDirectory $script:ArchiveName
    if (Test-Path -LiteralPath $archivePath) { Remove-Item -LiteralPath $archivePath -Force }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $archivePath)
    Remove-Item -LiteralPath $stage -Recurse -Force

    $hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $checksumsPath = Join-Path $tagDirectory $script:ChecksumsName
    if ($PSBoundParameters.ContainsKey('ChecksumsContent')) {
        [System.IO.File]::WriteAllText($checksumsPath, $ChecksumsContent)
    }
    else {
        [System.IO.File]::WriteAllText($checksumsPath, "$hash  ./$script:ArchiveName`n")
    }
    return [pscustomobject]@{
        Tag = $Tag
        Version = $Version
        Directory = $tagDirectory
        ArchivePath = $archivePath
        ChecksumsPath = $checksumsPath
        Hash = $hash
    }
}

function New-DownloadStub {
    param([Parameter(Mandatory)][hashtable]$Releases)

    # Replaces the network download: the URI must be the release URL this tag
    # would really use, and the bytes come from the fixture directory.
    return {
        param($Uri, $Destination)
        $match = [regex]::Match([string]$Uri, '/releases/download/(?<tag>v[0-9.]+)/(?<name>[^/]+)$')
        Assert-True $match.Success "download URI has the release shape: $Uri"
        $tag = $match.Groups['tag'].Value
        $name = $match.Groups['name'].Value
        Assert-True ($Releases.ContainsKey($tag)) "tag $tag is a known fixture"
        $source = Join-Path $Releases[$tag].Directory $name
        Assert-True (Test-Path -LiteralPath $source -PathType Leaf) "fixture provides $name for $tag"
        Copy-Item -LiteralPath $source -Destination $Destination -Force
    }.GetNewClosure()
}

function Get-UserPathEntries {
    $raw = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
    return @(([string]$raw).Split([char[]]@(';')) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Reset-Environment {
    [Environment]::SetEnvironmentVariable('Path', $script:BaselineUserPath, [EnvironmentVariableTarget]::User)
    $env:Path = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::Machine)
}

function Invoke-Case {
    param([string]$Name, [scriptblock]$Body)

    Write-CaseLine "case: $Name"
    Reset-Environment
    try {
        & $Body
        $script:Passed++
        Write-CaseLine "  ok"
    }
    catch {
        $script:Failed.Add("$Name`: $($_.Exception.Message)")
        Write-CaseLine "  FAILED: $($_.Exception.Message)"
        Write-CaseLine "  at: $($_.InvocationInfo.PositionMessage -replace "`r?`n", ' ')"
    }
}

try {
    Assert-True (Test-Path -LiteralPath $script:InstallScript -PathType Leaf) "installer exists at $script:InstallScript"

    if ([string]::IsNullOrWhiteSpace($SourceExecutable)) {
        $SourceExecutable = Join-Path $script:RepoRoot 'target\release\tafas.exe'
    }

    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ('tafas-installer-' + [guid]::NewGuid().ToString('N'))
    $sandboxProfile = Join-Path $sandbox 'profile'
    $localAppData = Join-Path $sandboxProfile 'AppData\Local'
    $fixtures = Join-Path $sandbox 'fixtures'
    New-Item -ItemType Directory -Force -Path $sandboxProfile, $localAppData, $fixtures | Out-Null
    $env:USERPROFILE = $sandboxProfile
    $env:LOCALAPPDATA = $localAppData
    $defaultInstallPath = Join-Path $localAppData 'Programs\tafas'
    Write-CaseLine "sandbox: $sandbox"

    Write-CaseLine "case: real-binary-version-format"
    Assert-True (Test-Path -LiteralPath $SourceExecutable -PathType Leaf) "the release binary exists at $SourceExecutable (build it first)"
    $realVersionLine = (& $SourceExecutable --version | Out-String).Trim()
    Assert-Match $realVersionLine '^tafas [0-9]+\.[0-9]+\.[0-9]+$' 'install.ps1 parses `tafas <x.y.z>` from --version'
    Write-CaseLine "  ok: $realVersionLine"
    $script:Passed++

    # Two releases: one to install, one to upgrade to, plus the hostile ones.
    $release1 = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.0' -Version '0.1.0'
    $release2 = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.1' -Version '0.1.1'
    $wrongVersion = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.2' -Version '0.1.0'
    $extraEntry = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.3' -Version '0.1.3' -ExtraEntries @('tafas.pdb')
    $badChecksums = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.4' -Version '0.1.4' `
        -ChecksumsContent "not-a-hash  ./$script:ArchiveName`n"
    $missingEntry = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.5' -Version '0.1.5' `
        -ChecksumsContent "$($release1.Hash)  ./tafas-aarch64-pc-windows-msvc.zip`n"
    $duplicateEntry = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.6' -Version '0.1.6' `
        -ChecksumsContent ("$($release1.Hash)  ./$script:ArchiveName`n$($release1.Hash)  ./$script:ArchiveName`n")
    $corruptArchive = New-ReleaseFixture -Directory $fixtures -Tag 'v0.1.7' -Version '0.1.7'

    $releases = @{}
    foreach ($release in @($release1, $release2, $wrongVersion, $extraEntry, $badChecksums, $missingEntry, $duplicateEntry, $corruptArchive)) {
        $releases[$release.Tag] = $release
    }
    $download = New-DownloadStub -Releases $releases

    . $script:InstallScript

    function Install-Fixture {
        param(
            [string]$Tag = 'latest',
            [string]$InstallRoot = $defaultInstallPath,
            [scriptblock]$ResolveLatest = { 'v0.1.1' }
        )
        Invoke-TafasInstallCore -Version $Tag -InstallRoot $InstallRoot -ResolveLatest $ResolveLatest -DownloadAsset $download
    }

    Invoke-Case 'install into the default per-user location' {
        Install-Fixture -Tag 'v0.1.0'
        Assert-True (Test-Path -LiteralPath (Join-Path $defaultInstallPath 'tafas.exe')) 'tafas.exe was installed'
        Assert-True (Test-Path -LiteralPath (Join-Path $defaultInstallPath 'LICENSE')) 'LICENSE was installed'
        $marker = Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw | ConvertFrom-Json
        Assert-Equal 1 $marker.marker_schema 'marker schema'
        Assert-Equal 'tafas-per-user' $marker.installer 'marker owner'
        Assert-Equal '0.1.0' $marker.version 'marker version'
        Assert-Equal 'v0.1.0' $marker.tag 'marker tag'
        Assert-Equal $true $marker.path_entry_owned 'marker records the PATH entry as owned'
        Assert-Equal 3 @($marker.managed_files).Count 'marker lists exactly the managed files'
        $entries = Get-UserPathEntries
        Assert-True (@($entries | Where-Object { $_ -ieq $defaultInstallPath }).Count -eq 1) 'exactly one user PATH entry was added'
        # A brand-new process reads the *persisted* user PATH, not this one.
        $freshRun = (& powershell.exe -NoProfile -Command `
            "`$env:Path = [Environment]::GetEnvironmentVariable('Path','User') + ';' + `$env:SystemRoot + '\System32'; tafas --version" |
            Out-String).Trim()
        Assert-Match $freshRun '^tafas 0\.1\.0$' 'a fresh process finds tafas on the persisted user PATH'
        $staging = @(Get-ChildItem -LiteralPath (Split-Path -Parent $defaultInstallPath) -Force)
        Assert-True (@($staging | Where-Object { $_.Name -like '.tafas-work-*' }).Count -eq 0) 'no staging directory is left behind'
    }

    Invoke-Case 'latest resolves through the release API seam' {
        Install-Fixture -Tag 'latest'
        $marker = Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw | ConvertFrom-Json
        Assert-Equal 'v0.1.1' $marker.tag 'latest installed the tag the resolver returned'
        $env:Path = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::Machine)
        $version = (& (Join-Path $defaultInstallPath 'tafas.exe') --version | Out-String).Trim()
        Assert-Equal 'tafas 0.1.1' $version 'the upgraded executable runs'
        Assert-True (@(Get-ChildItem -LiteralPath (Split-Path -Parent $defaultInstallPath) -Force -Filter '.tafas-backup-*').Count -eq 0) 'the backup directory was cleaned up'
    }

    Invoke-Case 'upgrade keeps exactly one PATH entry' {
        $entries = @(Get-UserPathEntries | Where-Object { $_ -ieq $defaultInstallPath })
        Assert-True ($entries.Count -eq 1) 'reinstalling did not duplicate the PATH entry'
        $marker = Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw | ConvertFrom-Json
        Assert-Equal '0.1.1' $marker.version 'marker follows the upgrade'
    }

    Invoke-Case 'checksum mismatch is refused and the old install survives' {
        $before = Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw
        # Corrupt the archive but keep a valid checksum line for it.
        $corruptTag = 'v0.1.7'
        Add-Content -LiteralPath $corruptArchive.ArchivePath -Value 'tampered'
        Assert-Throws { Install-Fixture -Tag $corruptTag } 'SHA-256 mismatch' 'a tampered archive is refused'
        $after = Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw
        Assert-Equal $before $after 'the installed version was left untouched'
        $env:Path = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::Machine)
        Assert-Equal 'tafas 0.1.1' ((& (Join-Path $defaultInstallPath 'tafas.exe') --version | Out-String).Trim()) 'the old executable still runs'
    }

    Invoke-Case 'a malformed SHA256SUMS is refused' {
        Assert-Throws { Install-Fixture -Tag $badChecksums.Tag } 'Malformed SHA256SUMS' 'an unparseable checksum file is refused'
    }

    Invoke-Case 'a SHA256SUMS without the exact asset is refused' {
        Assert-Throws { Install-Fixture -Tag $missingEntry.Tag } 'no exact entry' 'the checksum must name this archive'
    }

    Invoke-Case 'duplicate SHA256SUMS entries are refused' {
        Assert-Throws { Install-Fixture -Tag $duplicateEntry.Tag } 'duplicate entries' 'ambiguous checksums are refused'
    }

    Invoke-Case 'an archive with unexpected entries is refused' {
        Assert-Throws { Install-Fixture -Tag $extraEntry.Tag } 'Unexpected Windows release archive entries' 'only tafas.exe and LICENSE are expected'
        Assert-Equal '0.1.1' ((Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw | ConvertFrom-Json).version) 'the old install survived'
    }

    Invoke-Case 'an archive whose executable reports another version is refused' {
        Assert-Throws { Install-Fixture -Tag $wrongVersion.Tag } 'Version check failed' 'the tag and the binary must agree'
        Assert-Equal '0.1.1' ((Get-Content -LiteralPath (Join-Path $defaultInstallPath '.tafas-installer.json') -Raw | ConvertFrom-Json).version) 'the old install survived'
    }

    Invoke-Case 'a directory the installer does not own is refused' {
        $foreign = Join-Path $sandboxProfile 'Programs\foreign'
        New-Item -ItemType Directory -Force -Path $foreign | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $foreign 'notes.txt'), "mine`n")
        Assert-Throws { Install-Fixture -Tag 'v0.1.1' -InstallRoot $foreign } 'not owned by this installer' 'an unmarked directory is not adopted'
        Assert-True (Test-Path -LiteralPath (Join-Path $foreign 'notes.txt')) 'the foreign file is untouched'
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $foreign 'tafas.exe'))) 'nothing was installed into it'
    }

    Invoke-Case 'a marked directory with unmanaged files is refused' {
        $mixed = Join-Path $sandboxProfile 'Programs\mixed'
        Install-Fixture -Tag 'v0.1.1' -InstallRoot $mixed
        [System.IO.File]::WriteAllText((Join-Path $mixed 'notes.txt'), "mine`n")
        Assert-Throws { Install-Fixture -Tag 'v0.1.0' -InstallRoot $mixed } 'Unmanaged item' 'unmanaged files block a replacement'
        Assert-True (Test-Path -LiteralPath (Join-Path $mixed 'notes.txt')) 'the unrelated file is untouched'
        Remove-Item -LiteralPath (Join-Path $mixed 'notes.txt') -Force
        Invoke-TafasUninstallCore -InstallRoot $mixed
    }

    Invoke-Case 'an install root outside the profile is refused' {
        $outside = Join-Path ([System.IO.Path]::GetTempPath()) ('tafas-outside-' + [guid]::NewGuid().ToString('N'))
        Assert-Throws { Install-Fixture -Tag 'v0.1.1' -InstallRoot $outside } 'must be inside the current user profile' 'machine-wide roots are refused'
    }

    Invoke-Case 'an invalid version string is refused' {
        Assert-Throws { Install-Fixture -Tag 'v1.2' } 'Invalid tafas version' 'only stable x.y.z versions are accepted'
        Assert-Throws { Install-Fixture -Tag 'latest' -ResolveLatest { 'v0.1.0-rc1' } } 'Invalid tafas version' 'pre-releases are refused'
    }

    Invoke-Case 'uninstall removes only managed files and the owned PATH entry' {
        $custom = Join-Path $sandboxProfile 'Programs\custom'
        Install-Fixture -Tag 'v0.1.1' -InstallRoot $custom
        Assert-True (@(Get-UserPathEntries | Where-Object { $_ -ieq $custom }).Count -eq 1) 'the custom root is on the user PATH'
        Invoke-TafasUninstallCore -InstallRoot $custom
        Assert-True (-not (Test-Path -LiteralPath $custom)) 'the directory is gone once it is empty'
        Assert-True (@(Get-UserPathEntries | Where-Object { $_ -ieq $custom }).Count -eq 0) 'the owned PATH entry was removed'
        Assert-True (@(Get-UserPathEntries | Where-Object { $_ -ieq $defaultInstallPath }).Count -eq 1) 'other installs keep their PATH entry'
    }

    Invoke-Case 'uninstall keeps unrelated files in a shared directory' {
        $shared = Join-Path $sandboxProfile 'Programs\shared'
        Install-Fixture -Tag 'v0.1.1' -InstallRoot $shared
        [System.IO.File]::WriteAllText((Join-Path $shared 'notes.txt'), "mine`n")
        Invoke-TafasUninstallCore -InstallRoot $shared
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $shared 'tafas.exe'))) 'the executable was removed'
        Assert-True (-not (Test-Path -LiteralPath (Join-Path $shared '.tafas-installer.json'))) 'the marker was removed'
        Assert-True (Test-Path -LiteralPath (Join-Path $shared 'notes.txt')) 'the unrelated file stayed'
        Remove-Item -LiteralPath $shared -Recurse -Force
    }

    Invoke-Case 'uninstall refuses a directory it does not own' {
        $foreign = Join-Path $sandboxProfile 'Programs\foreign-uninstall'
        New-Item -ItemType Directory -Force -Path $foreign | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $foreign 'tafas.exe'), "not ours`n")
        Assert-Throws { Invoke-TafasUninstallCore -InstallRoot $foreign } 'not owned by this installer' 'an unmarked directory is not removed'
        Assert-True (Test-Path -LiteralPath (Join-Path $foreign 'tafas.exe')) 'the file stayed'
    }

    Invoke-Case 'a locked executable blocks replacement instead of half-replacing it' {
        $locked = Join-Path $sandboxProfile 'Programs\locked'
        Install-Fixture -Tag 'v0.1.0' -InstallRoot $locked
        $handle = [System.IO.File]::Open(
            (Join-Path $locked 'tafas.exe'),
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::None
        )
        try {
            Assert-Throws { Install-Fixture -Tag 'v0.1.1' -InstallRoot $locked } 'open, locked, or unreadable' 'a busy executable is reported, not overwritten'
        }
        finally {
            $handle.Dispose()
        }
        Assert-Equal '0.1.0' ((Get-Content -LiteralPath (Join-Path $locked '.tafas-installer.json') -Raw | ConvertFrom-Json).version) 'the previous version is intact'
        Invoke-TafasUninstallCore -InstallRoot $locked
    }

    Invoke-Case 'a revision three install cleans up after itself' {
        Invoke-TafasUninstallCore -InstallRoot $defaultInstallPath
        $leftovers = @(Get-ChildItem -LiteralPath (Split-Path -Parent $defaultInstallPath) -Force -Filter '.tafas-*')
        Assert-True ($leftovers.Count -eq 0) "no staging directories survived: $($leftovers.Name -join ', ')"
        Assert-True (@(Get-UserPathEntries | Where-Object { $_ -ieq $defaultInstallPath }).Count -eq 0) 'the default PATH entry is gone'
    }
}
catch {
    $script:Failed.Add("harness: $($_.Exception.Message)")
    Write-CaseLine "harness error: $($_.Exception.Message)"
    Write-CaseLine $_.ScriptStackTrace
}
finally {
    [Environment]::SetEnvironmentVariable('Path', $script:BaselineUserPath, [EnvironmentVariableTarget]::User)
    $env:USERPROFILE = $script:OriginalProfile
    $env:LOCALAPPDATA = $script:OriginalLocalAppData
    if ($null -ne $sandbox -and (Test-Path -LiteralPath $sandbox)) {
        Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$summary = @(
    "passed: $($script:Passed)"
    "failed: $($script:Failed.Count)"
) + $script:Failed
$summaryText = $summary -join [Environment]::NewLine
Write-Host $summaryText
if (-not [string]::IsNullOrWhiteSpace($EvidencePath)) {
    $parent = Split-Path -Parent $EvidencePath
    if (-not [string]::IsNullOrWhiteSpace($parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    [System.IO.File]::WriteAllText($EvidencePath, ($script:CaseLog -join [Environment]::NewLine) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
}
if ($script:Failed.Count -gt 0) {
    throw "$($script:Failed.Count) installer integration case(s) failed."
}
