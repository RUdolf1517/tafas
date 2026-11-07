# tafas — per-user installer for Windows x64.
#
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version latest
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall
#
# What this is: the Windows channel the tarball cannot be. There is no MSI and
# no machine-wide write; everything lands in the current user's profile
# (`%LOCALAPPDATA%\Programs\tafas` by default), and `-Uninstall` removes exactly
# what this script put there and nothing else.
#
# What it refuses to do is the point of the file. The archive is checked against
# the release's `SHA256SUMS` before it is unpacked, the unpacked archive must
# contain exactly `tafas.exe` and `LICENSE`, and the extracted executable must
# report the version the tag names. An existing directory is only replaced when
# a marker this installer wrote says it owns the place; anything else is left
# alone and reported. A failed install rolls the previous one back.
#
# The behaviour below is exercised against a real Windows filesystem by
# `packaging/windows/tests/Install.Integration.ps1` in `windows-installer.yml`:
# install, upgrade, every refusal, rollback and uninstall, offline.
#
# Environment:
#   TAFAS_GITHUB_TOKEN   optional read-only token for the GitHub API, to avoid
#                        anonymous rate limits in CI. End-user installs are
#                        anonymous unless the user sets this.

[CmdletBinding()]
param(
    [string]$Version = 'latest',
    [switch]$Uninstall,
    [string]$InstallRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12

$script:TafasRepository = 'RUdolf1517/tafas'
$script:TafasArchiveName = 'tafas-x86_64-pc-windows-msvc.zip'
$script:TafasChecksumsName = 'SHA256SUMS'
$script:TafasMarkerName = '.tafas-installer.json'
$script:TafasManagedFiles = @('tafas.exe', 'LICENSE', '.tafas-installer.json')
$script:TafasRedirectHosts = @(
    'github.com',
    'api.github.com',
    'release-assets.githubusercontent.com',
    'objects.githubusercontent.com',
    'github-releases.githubusercontent.com'
)

function ConvertTo-TafasTag {
    param([Parameter(Mandatory)][string]$Value)

    $tag = $Value.Trim()
    if ($tag.StartsWith('v', [System.StringComparison]::OrdinalIgnoreCase)) {
        $tag = 'v' + $tag.Substring(1)
    }
    else {
        $tag = 'v' + $tag
    }
    if ($tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
        throw "Invalid tafas version '$Value'. Expected a stable version such as 0.1.0 or v0.1.0."
    }
    return $tag
}

function Assert-TafasX64Architecture {
    $architecture = [string]$env:PROCESSOR_ARCHITEW6432
    if ([string]::IsNullOrWhiteSpace($architecture)) {
        $architecture = [string]$env:PROCESSOR_ARCHITECTURE
    }
    if ($architecture -notmatch '^(AMD64|x64)$') {
        throw "Unsupported Windows architecture '$architecture'. This release provides only the x64 (AMD64) binary."
    }
}

function Get-TafasLatestReleaseTag {
    $uri = 'https://api.github.com/repos/' + $script:TafasRepository + '/releases/latest'
    $headers = @{
        Accept = 'application/vnd.github+json'
        'X-GitHub-Api-Version' = '2022-11-28'
        'User-Agent' = 'tafas-install.ps1'
    }
    $token = [Environment]::GetEnvironmentVariable('TAFAS_GITHUB_TOKEN')
    if (-not [string]::IsNullOrWhiteSpace($token)) {
        $headers.Authorization = "Bearer $token"
    }
    $release = Invoke-RestMethod -Method Get -Uri $uri -Headers $headers
    if ($null -eq $release -or [string]::IsNullOrWhiteSpace([string]$release.tag_name)) {
        throw 'GitHub latest-release response did not contain tag_name.'
    }
    return (ConvertTo-TafasTag -Value ([string]$release.tag_name))
}

function Save-TafasUrl {
    param(
        [Parameter(Mandatory)][string]$Uri,
        [Parameter(Mandatory)][string]$Destination
    )

    $parsed = [System.Uri]$Uri
    if ($parsed.Scheme -ne 'https' -or $script:TafasRedirectHosts -cnotcontains $parsed.Host) {
        throw "Refusing an unexpected download URL: $Uri"
    }

    $headers = @{
        Accept = 'application/octet-stream'
        'User-Agent' = 'tafas-install.ps1'
    }
    $response = Invoke-WebRequest -Method Get -Uri $parsed.AbsoluteUri -Headers $headers `
        -OutFile $Destination -MaximumRedirection 5 -UseBasicParsing

    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        throw "GitHub returned no file for $Uri"
    }
    if ((Get-Item -LiteralPath $Destination).Length -le 0) {
        throw "GitHub returned an empty file for $Uri"
    }

    # Release downloads normally redirect to GitHub's release-asset CDN. Check
    # the final URI where the PowerShell version exposes it; never let an
    # unexpected redirect host silently become a new download source.
    $finalUri = $null
    if ($null -ne $response) {
        $baseProperty = $response.PSObject.Properties['BaseResponse']
        if ($null -ne $baseProperty -and $null -ne $baseProperty.Value) {
            $base = $baseProperty.Value
            $responseUriProperty = $base.PSObject.Properties['ResponseUri']
            if ($null -ne $responseUriProperty -and $null -ne $responseUriProperty.Value) {
                $finalUri = [System.Uri]$responseUriProperty.Value
            }
            if ($null -eq $finalUri) {
                $requestMessageProperty = $base.PSObject.Properties['RequestMessage']
                if ($null -ne $requestMessageProperty -and $null -ne $requestMessageProperty.Value) {
                    $requestUriProperty = $requestMessageProperty.Value.PSObject.Properties['RequestUri']
                    if ($null -ne $requestUriProperty -and $null -ne $requestUriProperty.Value) {
                        $finalUri = [System.Uri]$requestUriProperty.Value
                    }
                }
            }
        }
    }
    if ($null -ne $finalUri -and $script:TafasRedirectHosts -cnotcontains $finalUri.Host) {
        Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue
        throw "Refusing a GitHub redirect to an unexpected host: $($finalUri.Host)"
    }
}

function Get-TafasReleaseChecksum {
    param(
        [Parameter(Mandatory)][string]$ChecksumPath,
        [Parameter(Mandatory)][string]$AssetName
    )

    $lines = [System.IO.File]::ReadAllLines($ChecksumPath)
    if ($lines.Length -eq 0) {
        throw 'SHA256SUMS is empty.'
    }

    $entries = [System.Collections.Generic.Dictionary[string, object]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($line in $lines) {
        # `sha256sum ./*` writes `<hash>  ./<name>`, which is the shape the
        # release job produces. Anything else is refused rather than skipped:
        # a checksum file this script cannot read is a checksum file it cannot
        # check.
        $match = [System.Text.RegularExpressions.Regex]::Match(
            $line,
            '^(?<hash>[0-9a-fA-F]{64})  \.(?<name>/[A-Za-z0-9._-]+)$'
        )
        if (-not $match.Success) {
            throw "Malformed SHA256SUMS line: '$line'"
        }
        $name = $match.Groups['name'].Value.TrimStart('/')
        if ($entries.ContainsKey($name)) {
            throw "SHA256SUMS contains duplicate entries for '$name'."
        }
        $entries[$name] = [pscustomobject]@{
            Name = $name
            Hash = $match.Groups['hash'].Value.ToLowerInvariant()
        }
    }

    if (-not $entries.ContainsKey($AssetName) -or $entries[$AssetName].Name -cne $AssetName) {
        throw "SHA256SUMS has no exact entry for '$AssetName'."
    }
    return [string]$entries[$AssetName].Hash
}

function Get-TafasArchiveVersion {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
        throw "The release archive is missing tafas.exe: $ExecutablePath"
    }
    $output = @(& $ExecutablePath --version 2>&1)
    $exitCode = $LASTEXITCODE
    $text = ($output | ForEach-Object { [string]$_ }) -join "`n"
    if ($exitCode -ne 0) {
        throw "tafas.exe --version exited with ${exitCode}: $text"
    }
    $versionMatch = [System.Text.RegularExpressions.Regex]::Match(
        $text.Trim(),
        '^tafas (?<version>[0-9]+\.[0-9]+\.[0-9]+)$'
    )
    if (-not $versionMatch.Success) {
        throw "Unexpected tafas.exe --version output: $text"
    }
    return $versionMatch.Groups['version'].Value
}

function Get-TafasInstallRoot {
    param([string]$RequestedPath)

    $userProfile = [Environment]::GetEnvironmentVariable('USERPROFILE')
    $localAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA')
    if ([string]::IsNullOrWhiteSpace($userProfile) -or [string]::IsNullOrWhiteSpace($localAppData)) {
        throw 'USERPROFILE and LOCALAPPDATA must be set for a per-user installation.'
    }

    if ([string]::IsNullOrWhiteSpace($RequestedPath)) {
        $RequestedPath = Join-Path $localAppData 'Programs\tafas'
    }
    if (-not [System.IO.Path]::IsPathRooted($RequestedPath)) {
        throw 'InstallRoot must be an absolute path inside the current user profile.'
    }

    $fullPath = [System.IO.Path]::GetFullPath($RequestedPath).TrimEnd([char[]]@('\', '/'))
    $fullProfile = [System.IO.Path]::GetFullPath($userProfile).TrimEnd([char[]]@('\', '/'))
    $prefix = $fullProfile + [System.IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "InstallRoot must be inside the current user profile '$fullProfile'."
    }
    return $fullPath
}

function ConvertTo-TafasComparablePath {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return ''
    }
    $value = [Environment]::ExpandEnvironmentVariables($Path.Trim().Trim('"'))
    try {
        return [System.IO.Path]::GetFullPath($value).TrimEnd([char[]]@('\', '/'))
    }
    catch {
        return $value.TrimEnd([char[]]@('\', '/'))
    }
}

function Test-TafasPathEntry {
    param(
        [string]$Entry,
        [string]$InstallPath
    )

    $left = ConvertTo-TafasComparablePath -Path $Entry
    $right = ConvertTo-TafasComparablePath -Path $InstallPath
    return [string]::Equals($left, $right, [System.StringComparison]::OrdinalIgnoreCase)
}

function Add-TafasProcessPathEntry {
    param([string]$InstallPath)

    $parts = @(([string]$env:Path).Split([char]';'))
    foreach ($part in $parts) {
        if (Test-TafasPathEntry -Entry $part -InstallPath $InstallPath) {
            return
        }
    }
    if ([string]::IsNullOrEmpty($env:Path)) {
        $env:Path = $InstallPath
    }
    else {
        $env:Path = $env:Path + ';' + $InstallPath
    }
}

function Add-TafasUserPathEntry {
    param(
        [Parameter(Mandatory)][string]$InstallPath,
        [bool]$PreviouslyOwned
    )

    $rawPath = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
    $parts = @(([string]$rawPath).Split([char]';'))
    foreach ($part in $parts) {
        if (Test-TafasPathEntry -Entry $part -InstallPath $InstallPath) {
            Add-TafasProcessPathEntry -InstallPath $InstallPath
            return $PreviouslyOwned
        }
    }

    if ([string]::IsNullOrEmpty($rawPath)) {
        $updatedPath = $InstallPath
    }
    else {
        $updatedPath = $rawPath + ';' + $InstallPath
    }
    [Environment]::SetEnvironmentVariable('Path', $updatedPath, [EnvironmentVariableTarget]::User)
    Add-TafasProcessPathEntry -InstallPath $InstallPath
    return $true
}

function Remove-TafasProcessPathEntry {
    param([string]$InstallPath)

    $parts = @(([string]$env:Path).Split([char]';'))
    $kept = New-Object 'System.Collections.Generic.List[string]'
    $removed = $false
    foreach ($part in $parts) {
        if (-not $removed -and (Test-TafasPathEntry -Entry $part -InstallPath $InstallPath)) {
            $removed = $true
            continue
        }
        $kept.Add($part)
    }
    if ($removed) {
        $env:Path = [string]::Join(';', $kept.ToArray())
    }
}

function Remove-TafasUserPathEntry {
    param([Parameter(Mandatory)][string]$InstallPath)

    $rawPath = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
    $parts = @(([string]$rawPath).Split([char]';'))
    $kept = New-Object 'System.Collections.Generic.List[string]'
    $removed = $false
    foreach ($part in $parts) {
        if (-not $removed -and (Test-TafasPathEntry -Entry $part -InstallPath $InstallPath)) {
            $removed = $true
            continue
        }
        $kept.Add($part)
    }
    if ($removed) {
        [Environment]::SetEnvironmentVariable('Path', [string]::Join(';', $kept.ToArray()), [EnvironmentVariableTarget]::User)
    }
    Remove-TafasProcessPathEntry -InstallPath $InstallPath
}

function Write-TafasInstallerMarker {
    param(
        [Parameter(Mandatory)][string]$InstallPath,
        [Parameter(Mandatory)][string]$Tag,
        [Parameter(Mandatory)][bool]$PathEntryOwned
    )

    # The marker is what makes `-Uninstall` safe. It records the version, the
    # path and the exact list of files this installer is allowed to touch, so a
    # directory that happens to look like an install is still not one.
    $marker = [ordered]@{
        marker_schema = 1
        installer = 'tafas-per-user'
        version = $Tag.Substring(1)
        tag = $Tag
        install_root = $InstallPath
        path_entry_owned = $PathEntryOwned
        managed_files = @($script:TafasManagedFiles)
    }
    $json = ConvertTo-Json -InputObject $marker -Depth 4
    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText((Join-Path $InstallPath $script:TafasMarkerName), $json, $encoding)
}

function Read-TafasInstallerMarker {
    param([Parameter(Mandatory)][string]$InstallPath)

    $markerPath = Join-Path $InstallPath $script:TafasMarkerName
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw "'$InstallPath' is not owned by this installer; no files were changed."
    }
    try {
        $markerJson = [System.IO.File]::ReadAllText($markerPath, [System.Text.Encoding]::UTF8)
        $marker = ConvertFrom-Json -InputObject $markerJson
    }
    catch {
        throw "Installer ownership marker is malformed; no files were changed: $($_.Exception.Message)"
    }
    if ($marker.marker_schema -ne 1 -or $marker.installer -ne 'tafas-per-user') {
        throw 'Installer ownership marker has an unsupported schema or owner; no files were changed.'
    }
    if (-not [string]::Equals(
        (ConvertTo-TafasComparablePath -Path ([string]$marker.install_root)),
        (ConvertTo-TafasComparablePath -Path $InstallPath),
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'Installer ownership marker names a different install path; no files were changed.'
    }
    $managed = @($marker.managed_files)
    if ($managed.Count -ne $script:TafasManagedFiles.Count) {
        throw 'Installer ownership marker has an unexpected managed-file list; no files were changed.'
    }
    foreach ($name in $script:TafasManagedFiles) {
        if ($managed -cnotcontains $name) {
            throw 'Installer ownership marker has an unexpected managed-file list; no files were changed.'
        }
    }
    if ($marker.path_entry_owned -isnot [bool]) {
        throw 'Installer ownership marker has invalid PATH ownership metadata; no files were changed.'
    }
    return $marker
}

function Assert-TafasOwnedDirectory {
    param([Parameter(Mandatory)][string]$InstallPath)

    foreach ($item in Get-ChildItem -LiteralPath $InstallPath -Force) {
        if ($script:TafasManagedFiles -cnotcontains $item.Name) {
            throw "Unmanaged item '$($item.Name)' exists in '$InstallPath'; refusing to replace or remove it."
        }
        if ($item.PSIsContainer) {
            throw "Managed path '$($item.Name)' is a directory; refusing to replace or remove it."
        }
    }
}

function Assert-TafasExecutableAvailable {
    param([Parameter(Mandatory)][string]$ExecutablePath)

    if (-not (Test-Path -LiteralPath $ExecutablePath -PathType Leaf)) {
        return
    }
    $lastError = $null
    for ($attempt = 0; $attempt -lt 5; $attempt++) {
        $stream = $null
        try {
            # A read handle with no sharing detects an executable held open by
            # another process before any directory rename is attempted.
            $stream = [System.IO.File]::Open(
                $ExecutablePath,
                [System.IO.FileMode]::Open,
                [System.IO.FileAccess]::Read,
                [System.IO.FileShare]::None
            )
            return
        }
        catch {
            $lastError = $_.Exception
            if ($attempt -lt 4) {
                Start-Sleep -Milliseconds 200
            }
        }
        finally {
            if ($null -ne $stream) {
                $stream.Dispose()
            }
        }
    }
    throw "Cannot replace '$ExecutablePath': it is open, locked, or unreadable. Close programs using tafas and retry. $lastError"
}

function Test-TafasArchiveEntries {
    param([Parameter(Mandatory)][string]$ArchivePath)

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        $names = @($archive.Entries | ForEach-Object { $_.FullName })
        if ($names.Count -ne 2 -or $names -cnotcontains 'tafas.exe' -or $names -cnotcontains 'LICENSE') {
            throw "Unexpected Windows release archive entries: $($names -join ', ')"
        }
    }
    finally {
        $archive.Dispose()
    }
}

function Invoke-TafasInstallCore {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$InstallRoot,
        [scriptblock]$ResolveLatest = { Get-TafasLatestReleaseTag },
        [scriptblock]$DownloadAsset = {
            param($Uri, $Destination)
            Save-TafasUrl -Uri $Uri -Destination $Destination
        },
        [scriptblock]$MoveDirectory = {
            param($Source, $Destination)
            [System.IO.Directory]::Move($Source, $Destination)
        }
    )

    Assert-TafasX64Architecture
    $installPath = Get-TafasInstallRoot -RequestedPath $InstallRoot
    if ($Version -ieq 'latest') {
        $tag = ConvertTo-TafasTag -Value ([string](& $ResolveLatest))
    }
    else {
        $tag = ConvertTo-TafasTag -Value $Version
    }
    $releaseVersion = $tag.Substring(1)

    $parent = [System.IO.Directory]::GetParent($installPath).FullName
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    $id = [guid]::NewGuid().ToString('N')
    $workPath = Join-Path $parent ('.tafas-work-' + $id)
    $backupPath = Join-Path $parent ('.tafas-backup-' + $id)
    $downloadPath = Join-Path $workPath 'download'
    $extractPath = Join-Path $workPath 'extract'
    $stagedInstallPath = Join-Path $workPath 'install'

    $oldMoved = $false
    $newPromoted = $false
    $pathAttempted = $false
    $previousUserPath = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
    $previousProcessPath = $env:Path
    $oldMarker = $null

    try {
        [System.IO.Directory]::CreateDirectory($downloadPath) | Out-Null
        [System.IO.Directory]::CreateDirectory($extractPath) | Out-Null
        [System.IO.Directory]::CreateDirectory($stagedInstallPath) | Out-Null

        $baseUri = 'https://github.com/' + $script:TafasRepository + '/releases/download/' + $tag + '/'
        $archivePath = Join-Path $downloadPath $script:TafasArchiveName
        $checksumsPath = Join-Path $downloadPath $script:TafasChecksumsName
        & $DownloadAsset ($baseUri + $script:TafasArchiveName) $archivePath
        & $DownloadAsset ($baseUri + $script:TafasChecksumsName) $checksumsPath

        $expectedHash = Get-TafasReleaseChecksum -ChecksumPath $checksumsPath -AssetName $script:TafasArchiveName
        $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if (-not [string]::Equals($actualHash, $expectedHash, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "SHA-256 mismatch for $script:TafasArchiveName (expected $expectedHash, received $actualHash)."
        }

        Test-TafasArchiveEntries -ArchivePath $archivePath
        Expand-Archive -LiteralPath $archivePath -DestinationPath $extractPath -Force
        $extractedVersion = Get-TafasArchiveVersion -ExecutablePath (Join-Path $extractPath 'tafas.exe')
        if ($extractedVersion -ne $releaseVersion) {
            throw "Version check failed: tag $tag contains tafas $extractedVersion, not $releaseVersion."
        }
        if (-not (Test-Path -LiteralPath (Join-Path $extractPath 'LICENSE') -PathType Leaf)) {
            throw 'The release archive is missing the exact LICENSE file.'
        }

        Copy-Item -LiteralPath (Join-Path $extractPath 'tafas.exe') -Destination $stagedInstallPath
        Copy-Item -LiteralPath (Join-Path $extractPath 'LICENSE') -Destination $stagedInstallPath

        $targetExists = Test-Path -LiteralPath $installPath
        if ($targetExists) {
            if (-not [System.IO.Directory]::Exists($installPath)) {
                throw "Install path exists and is not a directory: $installPath"
            }
            $oldMarker = Read-TafasInstallerMarker -InstallPath $installPath
            Assert-TafasOwnedDirectory -InstallPath $installPath
            Assert-TafasExecutableAvailable -ExecutablePath (Join-Path $installPath 'tafas.exe')
        }

        # The candidate directory lives beside the destination, so these are
        # same-volume directory renames. If promotion fails, the previous
        # directory is moved back before the original error is returned.
        if ($targetExists) {
            & $MoveDirectory $installPath $backupPath
            $oldMoved = $true
        }
        & $MoveDirectory $stagedInstallPath $installPath
        $newPromoted = $true

        $wasPathOwned = $false
        if ($null -ne $oldMarker) {
            $wasPathOwned = [bool]$oldMarker.path_entry_owned
        }
        $pathAttempted = $true
        $pathOwned = Add-TafasUserPathEntry -InstallPath $installPath -PreviouslyOwned $wasPathOwned

        $installedVersion = Get-TafasArchiveVersion -ExecutablePath (Join-Path $installPath 'tafas.exe')
        if ($installedVersion -ne $releaseVersion) {
            throw "Post-install version check failed: expected $releaseVersion, got $installedVersion."
        }
        Write-TafasInstallerMarker -InstallPath $installPath -Tag $tag -PathEntryOwned $pathOwned

        if ($oldMoved) {
            try {
                Remove-Item -LiteralPath $backupPath -Recurse -Force
                $oldMoved = $false
            }
            catch {
                Write-Warning "New installation works, but the owned backup remains at '$backupPath': $($_.Exception.Message)"
            }
        }
        Write-Host "Installed tafas $releaseVersion for the current user at '$installPath'."
        Write-Host 'Open a new terminal to pick up the persisted user PATH entry; the current PowerShell process was updated too.'
    }
    catch {
        $installFailure = $_
        $rollbackErrors = New-Object 'System.Collections.Generic.List[string]'

        if ($pathAttempted) {
            try {
                [Environment]::SetEnvironmentVariable('Path', $previousUserPath, [EnvironmentVariableTarget]::User)
                $env:Path = $previousProcessPath
            }
            catch {
                $rollbackErrors.Add("could not restore PATH: $($_.Exception.Message)")
            }
        }
        if ($newPromoted -and [System.IO.Directory]::Exists($installPath)) {
            try {
                Remove-Item -LiteralPath $installPath -Recurse -Force
                $newPromoted = $false
            }
            catch {
                $rollbackErrors.Add("could not remove the staged replacement at '$installPath': $($_.Exception.Message)")
            }
        }
        if ($oldMoved -and [System.IO.Directory]::Exists($backupPath)) {
            try {
                [System.IO.Directory]::Move($backupPath, $installPath)
                $oldMoved = $false
            }
            catch {
                $rollbackErrors.Add("the previous installation is preserved at '$backupPath' but could not be moved back: $($_.Exception.Message)")
            }
        }

        if ($rollbackErrors.Count -gt 0) {
            throw "Installation failed: $($installFailure.Exception.Message) Rollback details: $($rollbackErrors -join '; ')"
        }
        throw $installFailure
    }
    finally {
        if ([System.IO.Directory]::Exists($workPath)) {
            Remove-Item -LiteralPath $workPath -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

function Invoke-TafasUninstallCore {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$InstallRoot)

    $installPath = Get-TafasInstallRoot -RequestedPath $InstallRoot
    if (-not [System.IO.Directory]::Exists($installPath)) {
        Write-Host "No tafas installer-owned installation was found at '$installPath'."
        return
    }

    $marker = Read-TafasInstallerMarker -InstallPath $installPath
    $backupPath = Join-Path ([System.IO.Directory]::GetParent($installPath).FullName) ('.tafas-uninstall-' + [guid]::NewGuid().ToString('N'))
    $movedFiles = New-Object 'System.Collections.Generic.List[string]'
    $previousUserPath = [Environment]::GetEnvironmentVariable('Path', [EnvironmentVariableTarget]::User)
    $previousProcessPath = $env:Path
    $pathAttempted = $false

    try {
        foreach ($name in $script:TafasManagedFiles) {
            $filePath = Join-Path $installPath $name
            if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) {
                continue
            }
            if (-not [System.IO.Directory]::Exists($backupPath)) {
                [System.IO.Directory]::CreateDirectory($backupPath) | Out-Null
            }
            [System.IO.File]::Move($filePath, (Join-Path $backupPath $name))
            $movedFiles.Add($name)
        }

        if ([bool]$marker.path_entry_owned) {
            $pathAttempted = $true
            Remove-TafasUserPathEntry -InstallPath $installPath
        }

        if ((Get-ChildItem -LiteralPath $installPath -Force | Measure-Object).Count -eq 0) {
            [System.IO.Directory]::Delete($installPath)
        }
        try {
            if ([System.IO.Directory]::Exists($backupPath)) {
                Remove-Item -LiteralPath $backupPath -Recurse -Force
            }
        }
        catch {
            Write-Warning "Owned uninstall staging files remain at '$backupPath': $($_.Exception.Message)"
        }
        Write-Host "Uninstalled installer-owned tafas files from '$installPath'. Unrelated files and PATH entries were left alone."
    }
    catch {
        $uninstallFailure = $_
        $rollbackErrors = New-Object 'System.Collections.Generic.List[string]'
        if ($pathAttempted) {
            try {
                [Environment]::SetEnvironmentVariable('Path', $previousUserPath, [EnvironmentVariableTarget]::User)
                $env:Path = $previousProcessPath
            }
            catch {
                $rollbackErrors.Add("could not restore PATH: $($_.Exception.Message)")
            }
        }
        $movedNames = $movedFiles.ToArray()
        for ($index = $movedNames.Length - 1; $index -ge 0; $index--) {
            $name = $movedNames[$index]
            $savedFile = Join-Path $backupPath $name
            $originalFile = Join-Path $installPath $name
            if (Test-Path -LiteralPath $savedFile -PathType Leaf) {
                try {
                    [System.IO.File]::Move($savedFile, $originalFile)
                }
                catch {
                    $rollbackErrors.Add("could not restore '$name': $($_.Exception.Message)")
                }
            }
        }
        if ($rollbackErrors.Count -gt 0) {
            throw "Uninstall failed: $($uninstallFailure.Exception.Message) Rollback details: $($rollbackErrors -join '; ')"
        }
        throw $uninstallFailure
    }
}

function Invoke-TafasInstaller {
    param(
        [string]$RequestedVersion = 'latest',
        [string]$RequestedInstallRoot
    )

    $installPath = Get-TafasInstallRoot -RequestedPath $RequestedInstallRoot
    Invoke-TafasInstallCore -Version $RequestedVersion -InstallRoot $installPath
}

function Invoke-TafasUninstaller {
    param([string]$RequestedInstallRoot)

    $installPath = Get-TafasInstallRoot -RequestedPath $RequestedInstallRoot
    Invoke-TafasUninstallCore -InstallRoot $installPath
}

# Dot-sourcing exposes the core functions to the integration test, which needs
# them to inject a local archive instead of a network download. The normal
# download-and-run interface accepts no alternate URLs and no checksum bypass.
if ($MyInvocation.InvocationName -ne '.') {
    if ($Uninstall) {
        Invoke-TafasUninstaller -RequestedInstallRoot $InstallRoot
    }
    else {
        Invoke-TafasInstaller -RequestedVersion $Version -RequestedInstallRoot $InstallRoot
    }
}
