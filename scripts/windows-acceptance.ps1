[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$MsiPath,
    [Parameter(Mandatory = $true)]
    [string]$ZipPath,
    [string]$PreviousMsiPath,
    [string]$ReleaseEvidencePath,
    [string]$OutputPath = "windows-acceptance.json",
    [switch]$SkipRendererSmoke,
    [switch]$SkipMsiLifecycle,
    [switch]$RequireStandardUser,
    [switch]$RequireMesaRuntime,
    [switch]$RequireAngleRuntime,
    [switch]$RequireRdpSession,
    [switch]$RequireSignature
)

$ErrorActionPreference = "Stop"
$version = "0.15.0"
$expectedMesaReleaseTag = "26.0.8"
$expectedMesaAssetName = "mesa3d-26.0.8-release-msvc.7z"
$expectedMesaAssetSha256 = "a438c26c2752726916e455f9ad121f8a7e3cfecf8626251abf8e5b3e129d8497"
$evidence = [ordered]@{
    schemaVersion = 1
    packageVersion = $version
    os = [Environment]::OSVersion.Version.ToString()
    session = $env:SESSIONNAME
    tests = @()
}

function Add-Evidence {
    param(
        [string]$Name,
        [string]$Status,
        [string]$Detail
    )
    $script:evidence.tests += [ordered]@{
        name = $Name
        status = $Status
        detail = $Detail
    }
    if ($Status -eq "failed") {
        throw "$Name failed: $Detail"
    }
}

function Invoke-Msi {
    param(
        [ValidateSet("install", "uninstall")]
        [string]$Action,
        [string]$Path
    )
    $arguments = if ($Action -eq "install") {
        "/i `"$Path`" /qn /norestart"
    } else {
        "/x `"$Path`" /qn /norestart"
    }
    $process = Start-Process -FilePath "msiexec.exe" -ArgumentList $arguments -Wait -PassThru
    if ($process.ExitCode -notin @(0, 3010)) {
        throw "msiexec $Action returned $($process.ExitCode)"
    }
}

function Get-InstalledExe {
    $candidate = Join-Path ${env:ProgramFiles} "Scope Analyzer\ScopeAnalyzer.exe"
    if (Test-Path $candidate) {
        return $candidate
    }
    return $null
}

function Get-InstalledCli {
    $candidate = Join-Path ${env:ProgramFiles} "Scope Analyzer\scope-cli.exe"
    if (Test-Path $candidate) {
        return $candidate
    }
    return $null
}

function Assert-AuthenticodeSignature {
    param(
        [string]$Path,
        [string]$Description
    )
    $signature = Get-AuthenticodeSignature -FilePath $Path
    if ($signature.Status -ne "Valid") {
        throw "$Description is not Authenticode-signed with a valid signature: $($signature.Status)"
    }
}

function Assert-BuildProvenance {
    param(
        [string]$PackageRoot,
        [object]$ReleaseEvidence
    )
    $provenancePath = Join-Path $PackageRoot "build-provenance.json"
    if (-not (Test-Path $provenancePath)) {
        throw "ZIP is missing build-provenance.json"
    }
    $provenance = Get-Content -Raw $provenancePath | ConvertFrom-Json
    if ($provenance.schemaVersion -ne 1) {
        throw "Unsupported build provenance schema: $($provenance.schemaVersion)"
    }
    if ($provenance.packageVersion -ne $version) {
        throw "Build provenance package version mismatch: $($provenance.packageVersion)"
    }
    foreach ($field in @("cargoLockSha256", "toolchainSha256")) {
        if ([string]$provenance.$field -notmatch '^[0-9a-fA-F]{64}$') {
            throw "Build provenance $field is missing or is not a SHA256 value"
        }
    }
    if ($RequireSignature -and
        ([string]::IsNullOrWhiteSpace([string]$provenance.sourceCommit) -or
         [string]$provenance.sourceCommit -eq "unknown")) {
        throw "Signed release provenance must bind to a known source commit"
    }
    if ($RequireSignature -and $provenance.sourceDirty -eq $true) {
        throw "Signed release provenance must be produced from a clean source tree"
    }
    if ($null -ne $ReleaseEvidence -and
        $ReleaseEvidence.sourceCommit -and
        $provenance.sourceCommit -ne $ReleaseEvidence.sourceCommit) {
        throw "Build provenance sourceCommit does not match release-evidence.json"
    }
    if ($RequireSignature -and $ReleaseEvidence -and $ReleaseEvidence.sourceDirty -eq $true) {
        throw "Signed release evidence must be produced from a clean source tree"
    }
    if (-not $provenance.fileHashes) {
        throw "Build provenance does not contain fileHashes"
    }
    $expectedNames = @($provenance.fileHashes.PSObject.Properties.Name)
    foreach ($entry in $provenance.fileHashes.PSObject.Properties) {
        $relative = [string]$entry.Name
        if ([IO.Path]::IsPathRooted($relative) -or $relative -match '(^|/)\.\.(\/|$)') {
            throw "Build provenance contains an unsafe relative path: $relative"
        }
        $path = Join-Path $PackageRoot ($relative -replace '/', '\')
        if (-not (Test-Path $path)) {
            throw "Build provenance references missing staged file: $relative"
        }
        $actual = (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne ([string]$entry.Value).ToLowerInvariant()) {
            throw "Build provenance hash mismatch for $relative"
        }
    }
    $actualNames = @(Get-ChildItem -Path $PackageRoot -Recurse -File |
        ForEach-Object {
            $_.FullName.Substring($PackageRoot.Length).TrimStart('\', '/') -replace '\\', '/'
        } |
        Where-Object { $_ -ne "build-provenance.json" })
    $differences = Compare-Object -ReferenceObject ($expectedNames | Sort-Object) -DifferenceObject ($actualNames | Sort-Object)
    if ($differences) {
        throw "Build provenance file list does not match ZIP stage contents"
    }
}

function Assert-RuntimeManifestFiles {
    param(
        [string]$RuntimeRoot,
        [object]$Manifest,
        [string]$Description
    )
    if (-not $Manifest.fileHashes) {
        throw "$Description manifest does not contain fileHashes"
    }
    foreach ($entry in $Manifest.fileHashes.PSObject.Properties) {
        $relative = [string]$entry.Name
        if ([IO.Path]::IsPathRooted($relative) -or $relative -match '(^|/)\.\.(\/|$)') {
            throw "$Description manifest contains an unsafe relative path: $relative"
        }
        $path = Join-Path $RuntimeRoot ($relative -replace '/', '\')
        if (-not (Test-Path $path)) {
            throw "$Description manifest references missing file: $relative"
        }
        $actual = (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne ([string]$entry.Value).ToLowerInvariant()) {
            throw "$Description manifest hash mismatch for $relative"
        }
    }
}

function Assert-ManifestContainsFiles {
    param(
        [object]$Manifest,
        [string[]]$RequiredFiles,
        [string]$Description
    )
    if (-not $Manifest.fileHashes) {
        throw "$Description manifest does not contain fileHashes"
    }
    foreach ($name in $RequiredFiles) {
        if (-not $Manifest.fileHashes.PSObject.Properties[$name]) {
            throw "$Description manifest does not hash required file: $name"
        }
    }
}

function Invoke-BridgeCapabilities {
    param([string]$Executable)
    $json = & $Executable --vscode-capabilities 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "Bridge capability command failed with exit code $LASTEXITCODE"
    }
    $capabilities = $json | ConvertFrom-Json
    if ($capabilities.protocolVersion -ne 1) {
        throw "Bridge protocol version mismatch: $($capabilities.protocolVersion)"
    }
    if (-not ($capabilities.commands -contains "dataset") -or
        -not ($capabilities.commands -contains "fft")) {
        throw "Bridge did not advertise dataset and fft commands"
    }
}

function Invoke-RendererSmoke {
    param(
        [string]$Executable,
        [string]$Renderer,
        [string]$Api
    )
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.WorkingDirectory = Split-Path -Parent $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.EnvironmentVariables["SCOPE_RENDERER"] = $Renderer
    $startInfo.EnvironmentVariables["SCOPE_GL_API"] = $Api
    $smokeHome = Join-Path $env:TEMP "scope-renderer-smoke-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $smokeHome -Force | Out-Null
    $startInfo.EnvironmentVariables["SCOPE_APP_HOME"] = $smokeHome
    $logPath = Join-Path $smokeHome "ScopeAnalyzer-startup.log"
    $process = $null
    try {
        $process = [System.Diagnostics.Process]::Start($startInfo)
        $timedOut = -not $process.WaitForExit(8000)
        if ($timedOut) {
            $process.Kill()
            $process.WaitForExit(2000)
        }
        # A GUI smoke process is expected to remain alive; terminate it after
        # startup has been observed. Only an early, self-terminating process
        # must report a clean exit code.
        if (-not $timedOut -and $process.ExitCode -ne 0) {
            throw "$Renderer renderer exited with $($process.ExitCode)"
        }
        $expectedLog = switch ($Renderer) {
            "wgpu-software" { "starting renderer: wgpu/DX12 software/WARP" }
            "mesa" { "starting renderer: Mesa/llvmpipe software OpenGL" }
            "glow" { "starting renderer: glow/OpenGL hardware" }
            default { throw "Unsupported renderer smoke mode: $Renderer" }
        }
        if (-not (Test-Path $logPath)) {
            throw "$Renderer renderer did not produce ScopeAnalyzer-startup.log"
        }
        $log = Get-Content -Raw $logPath
        if ($log -notmatch [regex]::Escape($expectedLog)) {
            throw "$Renderer renderer did not record the expected selection in ScopeAnalyzer-startup.log"
        }
    } finally {
        if ($process -and -not $process.HasExited) {
            $process.Kill()
            $process.WaitForExit(2000)
        }
        if (Test-Path $smokeHome) {
            Remove-Item -Recurse -Force $smokeHome
        }
    }
}

$zipTemp = Join-Path $env:TEMP "scope-acceptance-$([guid]::NewGuid())"
try {
    if (-not (Test-Path $MsiPath) -or -not (Test-Path $ZipPath)) {
        throw "Both -MsiPath and -ZipPath must point to existing artifacts."
    }
    if ($RequireSignature -and -not $ReleaseEvidencePath) {
        throw "-RequireSignature requires -ReleaseEvidencePath so the signed release evidence can be verified."
    }
    if ($SkipRendererSmoke -and ($RequireMesaRuntime -or $RequireAngleRuntime)) {
        throw "-SkipRendererSmoke cannot be combined with -RequireMesaRuntime or -RequireAngleRuntime."
    }
    if ($RequireStandardUser -and -not $SkipMsiLifecycle) {
        throw "-RequireStandardUser requires -SkipMsiLifecycle; install/upgrade/uninstall must be performed by an elevated acceptance run."
    }
    $osVersion = [Environment]::OSVersion.Version
    if ($osVersion.Major -lt 10) {
        throw "Windows 10 or newer is required for the x64 acceptance matrix."
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw "The x64 package requires a 64-bit Windows host."
    }
    Add-Evidence "host" "passed" "Windows $osVersion x64 (Windows 10/11 family)"

    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if ($RequireStandardUser -and $isAdministrator) {
        throw "The standard-user acceptance run is elevated; start it from a non-administrator account."
    }
    if (-not $SkipMsiLifecycle -and -not $isAdministrator) {
        throw "The MSI lifecycle acceptance run must be started from an elevated administrator session."
    }
    $userDetail = if ($isAdministrator) { "elevated administrator token" } else { "standard user token" }
    Add-Evidence "user-context" "passed" $userDetail

    if ($ReleaseEvidencePath) {
        if (-not (Test-Path $ReleaseEvidencePath)) {
            throw "Release evidence file was not found: $ReleaseEvidencePath"
        }
        $releaseEvidence = Get-Content -Raw $ReleaseEvidencePath | ConvertFrom-Json
        foreach ($artifactPath in @($MsiPath, $ZipPath)) {
            $artifactName = Split-Path -Leaf $artifactPath
            $artifactEntry = $releaseEvidence.artifacts.PSObject.Properties[$artifactName].Value
            $expected = $artifactEntry.sha256
            $actual = (Get-FileHash -Path $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
            if (-not $expected -or $expected.ToLowerInvariant() -ne $actual) {
                throw "Release evidence hash mismatch for $artifactName"
            }
        }
        if ($RequireSignature -and $releaseEvidence.signatureStatus -ne "msi-and-stage-binaries-valid") {
            throw "Release evidence does not declare a signed MSI and signed stage binaries."
        }
        Add-Evidence "release-evidence" "passed" "Final ZIP/MSI SHA256 values match release-evidence.json"
    } else {
        Add-Evidence "release-evidence" "skipped" "-ReleaseEvidencePath was not supplied"
    }

    Expand-Archive -Path $ZipPath -DestinationPath $zipTemp -Force
    # package-windows.ps1 currently archives the stage contents directly.  A
    # flat layout can still contain subdirectories such as mesa/, so do not
    # select an arbitrary first directory as the package root.  Also accept a
    # conventional single top-level folder when a future packager uses one.
    $flatRoot = Get-Item -Path $zipTemp
    if (Test-Path (Join-Path $flatRoot.FullName "ScopeAnalyzer.exe")) {
        $zipRoot = $flatRoot
    } else {
        $zipRoot = Get-ChildItem -Path $zipTemp -Directory |
            Where-Object { Test-Path (Join-Path $_.FullName "ScopeAnalyzer.exe") } |
            Select-Object -First 1
        if (-not $zipRoot) {
            throw "ZIP does not contain ScopeAnalyzer.exe in a flat or top-level package layout"
        }
    }
    foreach ($required in @(
        "ScopeAnalyzer.exe",
        "scope-cli.exe",
        "build-provenance.json",
        "sbom.cdx.json",
        "THIRD-PARTY-NOTICES.txt",
        "Start-ScopeAnalyzer.bat",
        "Start-ScopeAnalyzer-Mesa.bat"
    )) {
        if (-not (Test-Path (Join-Path $zipRoot.FullName $required))) {
            throw "ZIP is missing $required"
        }
    }
    Add-Evidence "zip-content" "passed" "Required executables, scripts, provenance and SBOM present"
    Assert-BuildProvenance -PackageRoot $zipRoot.FullName -ReleaseEvidence $releaseEvidence
    Add-Evidence "provenance" "passed" "ZIP file hashes and source commit match build-provenance.json"

    if ($RequireSignature) {
        Assert-AuthenticodeSignature -Path $MsiPath -Description "MSI artifact"
        Assert-AuthenticodeSignature -Path (Join-Path $zipRoot.FullName "ScopeAnalyzer.exe") -Description "ZIP ScopeAnalyzer.exe"
        Assert-AuthenticodeSignature -Path (Join-Path $zipRoot.FullName "scope-cli.exe") -Description "ZIP scope-cli.exe"
        $zipMesaExecutable = Join-Path $zipRoot.FullName "mesa\ScopeAnalyzerMesa.exe"
        if (Test-Path $zipMesaExecutable) {
            Assert-AuthenticodeSignature -Path $zipMesaExecutable -Description "ZIP Mesa ScopeAnalyzerMesa.exe"
        }
        Add-Evidence "signatures" "passed" "MSI and packaged executables have valid Authenticode signatures"
    }

    $angleManifest = Join-Path $zipRoot.FullName "angle-runtime-manifest.json"
    if (Test-Path (Join-Path $zipRoot.FullName "libEGL.dll")) {
        if (-not (Test-Path $angleManifest)) {
            throw "ZIP contains ANGLE DLLs but no angle-runtime-manifest.json"
        }
        $angleManifestData = Get-Content -Raw $angleManifest | ConvertFrom-Json
        if ($RequireAngleRuntime -and
            [string]::IsNullOrWhiteSpace($angleManifestData.sourceArchiveSha256)) {
            throw "ANGLE manifest does not pin sourceArchiveSha256"
        }
        if ($RequireAngleRuntime -and
            $angleManifestData.sourceArchiveSha256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw "ANGLE manifest sourceArchiveSha256 is not a 64-character hexadecimal SHA256 value"
        }
        if ($RequireAngleRuntime -and
            $angleManifestData.preloadManifestSha256 -notmatch '^[0-9a-fA-F]{64}$') {
            throw "ANGLE manifest preloadManifestSha256 is not a 64-character hexadecimal SHA256 value"
        }
        if ($angleManifestData.schemaVersion -ne 1) {
            throw "ANGLE manifest schemaVersion must be 1"
        }
        Assert-ManifestContainsFiles -Manifest $angleManifestData -RequiredFiles @("libEGL.dll", "libGLESv2.dll") -Description "ANGLE runtime"
        Assert-RuntimeManifestFiles -RuntimeRoot $zipRoot.FullName -Manifest $angleManifestData -Description "ANGLE runtime"
        Add-Evidence "angle-manifest" "passed" "Bundled ANGLE DLLs have a manifest"
    } else {
        Add-Evidence "angle-manifest" "skipped" "ANGLE runtime was not bundled"
    }
    $mesaManifest = Join-Path $zipRoot.FullName "mesa\mesa-runtime-manifest.json"
    if (Test-Path (Join-Path $zipRoot.FullName "mesa\ScopeAnalyzerMesa.exe")) {
        if (-not (Test-Path $mesaManifest)) {
            throw "ZIP contains Mesa helper but no mesa-runtime-manifest.json"
        }
        $mesaManifestData = Get-Content -Raw $mesaManifest | ConvertFrom-Json
        if ($mesaManifestData.schemaVersion -ne 1) {
            throw "Mesa manifest schemaVersion must be 1"
        }
        if ($RequireMesaRuntime -and
            ($mesaManifestData.mesaReleaseTag -ne $expectedMesaReleaseTag -or
             $mesaManifestData.mesaAssetName -ne $expectedMesaAssetName -or
             $mesaManifestData.mesaAssetSha256 -ne $expectedMesaAssetSha256 -or
             $mesaManifestData.sourceArchiveSha256 -notmatch '^[0-9a-fA-F]{64}$')) {
            throw "Mesa manifest does not match the pinned release asset"
        }
        Assert-ManifestContainsFiles -Manifest $mesaManifestData -RequiredFiles @("opengl32.dll", "libgallium_wgl.dll", "libEGL.dll", "libGLESv2.dll", "libGLESv1_CM.dll") -Description "Mesa runtime"
        Assert-RuntimeManifestFiles -RuntimeRoot (Join-Path $zipRoot.FullName "mesa") -Manifest $mesaManifestData -Description "Mesa runtime"
        Add-Evidence "mesa-manifest" "passed" "Bundled Mesa DLLs have a manifest"
    } else {
        Add-Evidence "mesa-manifest" "skipped" "Mesa runtime was not bundled"
    }

    if ($SkipMsiLifecycle) {
        Add-Evidence "msi-lifecycle" "skipped" "-SkipMsiLifecycle was supplied; using an already deployed installation"
    } elseif ($PreviousMsiPath) {
        Invoke-Msi "install" $PreviousMsiPath
        if (-not (Get-InstalledExe)) {
            throw "Previous MSI did not install ScopeAnalyzer.exe"
        }
        Invoke-Msi "install" $MsiPath
        Add-Evidence "upgrade" "passed" "Previous MSI upgraded to $version"
    } else {
        Invoke-Msi "install" $MsiPath
        Add-Evidence "install" "passed" "MSI installed for upgrade/uninstall verification"
    }

    $installedExe = Get-InstalledExe
    if (-not $installedExe) {
        throw "Installed ScopeAnalyzer.exe was not found"
    }
    $installedCli = Get-InstalledCli
    if (-not $installedCli) {
        throw "Installed scope-cli.exe was not found"
    }
    $installedMesaExecutable = Join-Path (Split-Path -Parent $installedExe) "mesa\ScopeAnalyzerMesa.exe"
    $packagedMesaExecutable = Join-Path $zipRoot.FullName "mesa\ScopeAnalyzerMesa.exe"
    $artifactPairs = @(
        @{ installed = $installedExe; packaged = (Join-Path $zipRoot.FullName "ScopeAnalyzer.exe"); name = "ScopeAnalyzer.exe" },
        @{ installed = $installedCli; packaged = (Join-Path $zipRoot.FullName "scope-cli.exe"); name = "scope-cli.exe" }
    )
    if (Test-Path $packagedMesaExecutable) {
        if (-not (Test-Path $installedMesaExecutable)) {
            throw "Installed Mesa ScopeAnalyzerMesa.exe was not found"
        }
        $artifactPairs += @{ installed = $installedMesaExecutable; packaged = $packagedMesaExecutable; name = "mesa\ScopeAnalyzerMesa.exe" }
    } elseif (Test-Path $installedMesaExecutable) {
        throw "Installed Mesa ScopeAnalyzerMesa.exe was not present in the packaged ZIP"
    }
    foreach ($pair in $artifactPairs) {
        $installedHash = (Get-FileHash -Path $pair.installed -Algorithm SHA256).Hash.ToLowerInvariant()
        $packagedHash = (Get-FileHash -Path $pair.packaged -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($installedHash -ne $packagedHash) {
            throw "Installed $($pair.name) does not match the packaged ZIP executable"
        }
    }
    Add-Evidence "installed-artifact-match" "passed" "Installed executables match the signed ZIP payload"
    if ($RequireSignature) {
        Assert-AuthenticodeSignature -Path $installedExe -Description "Installed ScopeAnalyzer.exe"
        Assert-AuthenticodeSignature -Path $installedCli -Description "Installed scope-cli.exe"
    }
    if ($RequireSignature -and (Test-Path $installedMesaExecutable)) {
        Assert-AuthenticodeSignature -Path $installedMesaExecutable -Description "Installed Mesa ScopeAnalyzerMesa.exe"
    }
    Invoke-BridgeCapabilities $installedExe
    Add-Evidence "bridge-capabilities" "passed" "Protocol 1 and dataset/fft capabilities negotiated"

    if ($SkipRendererSmoke) {
        Add-Evidence "renderer-smoke" "skipped" "-SkipRendererSmoke was supplied"
    } else {
        Invoke-RendererSmoke $installedExe "wgpu-software" ""
        Add-Evidence "warp-renderer" "passed" "WARP/software renderer started and recorded renderer selection"
        if (Test-Path $installedMesaExecutable) {
            Invoke-RendererSmoke $installedMesaExecutable "mesa" "wgl"
            Add-Evidence "mesa-renderer" "passed" "Bundled Mesa process started and recorded renderer selection"
        } elseif ($RequireMesaRuntime) {
            Add-Evidence "mesa-renderer" "failed" "Mesa runtime was required but was not bundled"
        } else {
            Add-Evidence "mesa-renderer" "skipped" "Mesa runtime was not bundled"
        }
        $installRoot = Split-Path -Parent $installedExe
        $angleDlls = @(
            (Join-Path $installRoot "libEGL.dll"),
            (Join-Path $installRoot "libGLESv2.dll")
        )
        if (($angleDlls | Where-Object { -not (Test-Path $_) }).Count -eq 0) {
            Invoke-RendererSmoke $installedExe "glow" ""
            Add-Evidence "angle-renderer" "passed" "Bundled ANGLE EGL/GLES DLLs started the Glow/EGL path and renderer selection was logged"
        } elseif ($RequireAngleRuntime) {
            Add-Evidence "angle-renderer" "failed" "ANGLE runtime was required but was not bundled"
        } else {
            Add-Evidence "angle-renderer" "skipped" "ANGLE runtime was not bundled; target-machine EGL fallback was not asserted"
        }
    }

    if ($SkipMsiLifecycle) {
        Add-Evidence "uninstall" "skipped" "-SkipMsiLifecycle was supplied; cleanup is owned by the elevated lifecycle run"
    } else {
        Invoke-Msi "uninstall" $MsiPath
        if (Get-InstalledExe) {
            throw "Scope Analyzer executable remains after uninstall"
        }
        Add-Evidence "uninstall" "passed" "MSI removed installed executable"
    }
    if ($env:SESSIONNAME -like "RDP-*") {
        Add-Evidence "rdp-session" "passed" "Acceptance ran inside an RDP session"
    } elseif ($RequireRdpSession) {
        Add-Evidence "rdp-session" "failed" "RDP session was required for this acceptance run"
    } else {
        Add-Evidence "rdp-session" "skipped" "Host session was not RDP"
    }
} catch {
    $evidence.tests += [ordered]@{
        name = "acceptance"
        status = "failed"
        detail = $_.Exception.Message
    }
    throw
} finally {
    $evidence | ConvertTo-Json -Depth 8 | Set-Content -Path $OutputPath -Encoding UTF8
    if (Test-Path $zipTemp) {
        Remove-Item -Recurse -Force $zipTemp
    }
}

Write-Host "Windows acceptance evidence written to $OutputPath"
