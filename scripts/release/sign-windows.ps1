<#
.SYNOPSIS
Signs vanillite.exe in the Windows archives of a published GitHub release.

.DESCRIPTION
The release workflow publishes the Windows build unsigned. This script runs on a Windows PC
with the code signing certificate in Certum SimplySign (SimplySign Desktop, logged in) and,
for one release tag:

  1. downloads the Windows zip archives and SHA256SUMS of the release (gh) and checks them
     against SHA256SUMS,
  2. signs vanillite.exe in each archive with signtool (SHA-256 file digest, RFC 3161
     timestamp from Certum) and verifies the signature (signtool verify /pa),
  3. packs each archive again with the same layout and updates SHA256SUMS,
  4. uploads the archives and SHA256SUMS to the release, replacing the old ones.

An archive whose vanillite.exe already has a valid signature is left as it is, so the script
can run again after a failure. -DryRun does everything but the upload.

Steps and prerequisites: docs/development.md, "Signing the Windows build".

.PARAMETER Tag
The release tag, e.g. v0.1.0-beta.1+mc26.3.

.PARAMETER CertificateSubject
Part of the certificate's subject (e.g. the owner's name); exactly one matching code signing
certificate with a private key must be in the user's certificate store.

.PARAMETER Thumbprint
The certificate's SHA-1 thumbprint, instead of -CertificateSubject.

.PARAMETER Repository
The GitHub repository (owner/name).

.PARAMETER TimestampUrl
The RFC 3161 timestamp server.

.PARAMETER SignTool
Path to signtool.exe; by default it is searched on PATH and in the Windows SDK.

.PARAMETER WorkDir
Where the archives are downloaded and packed; by default a new folder in %TEMP%.

.PARAMETER DryRun
Download, sign, verify and pack, but upload nothing.

.EXAMPLE
.\scripts\release\sign-windows.ps1 -Tag 'v0.1.0-beta.1+mc26.3' -CertificateSubject 'Hannes' -DryRun
#>
# Write-Host on purpose: an interactive script with coloured messages, no pipeline output.
[Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '')]
[CmdletBinding(DefaultParameterSetName = 'Subject')]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string] $Tag,

    [Parameter(Mandatory = $true, ParameterSetName = 'Subject')]
    [string] $CertificateSubject,

    [Parameter(Mandatory = $true, ParameterSetName = 'Thumbprint')]
    [string] $Thumbprint,

    [string] $Repository = 'hthoene/vanillite',

    [string] $TimestampUrl = 'http://time.certum.pl',

    [string] $SignTool,

    [string] $WorkDir,

    [switch] $DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-Step([string] $Message) {
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Exit-WithFix([string] $Problem, [string] $Fix) {
    Write-Host "ERROR: $Problem" -ForegroundColor Red
    Write-Host "  Fix: $Fix" -ForegroundColor Yellow
    exit 1
}

# Runs a native program and fails on a non-zero exit code.
function Invoke-Native([string] $Program, [string[]] $Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$([IO.Path]::GetFileName($Program)) failed (exit code $LASTEXITCODE): $($Arguments -join ' ')"
    }
}

function Get-Sha256([string] $Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Find-SignTool([string] $SignTool) {
    if ($SignTool) {
        if (Test-Path -LiteralPath $SignTool) { return (Resolve-Path -LiteralPath $SignTool).Path }
        Exit-WithFix "signtool not found at $SignTool." 'Pass the full path to signtool.exe with -SignTool, or leave it out to search the Windows SDK.'
    }
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    $arch = if ([Environment]::Is64BitOperatingSystem) { 'x64' } else { 'x86' }
    $kits = @(${env:ProgramFiles(x86)}, $env:ProgramFiles) | Where-Object { $_ } |
        ForEach-Object { Join-Path $_ 'Windows Kits\10\bin' } | Where-Object { Test-Path -LiteralPath $_ }
    $found = $kits | ForEach-Object { Get-ChildItem -LiteralPath $_ -Directory -ErrorAction SilentlyContinue } |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version] $_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName "$arch\signtool.exe" } |
        Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if ($found) { return $found }
    Exit-WithFix 'signtool.exe (Windows SDK) not found.' 'Install the Windows SDK with "Windows SDK Signing Tools for Desktop Apps" (winget install Microsoft.WindowsSDK.10.0.26100, or the Visual Studio Installer), or pass -SignTool <path>.'
}

function Find-Certificate([string] $CertificateSubject, [string] $Thumbprint) {
    $codeSigning = '1.3.6.1.5.5.7.3.3'
    $all = @(Get-ChildItem Cert:\CurrentUser\My) | Where-Object {
        $_.HasPrivateKey -and $_.NotAfter -gt (Get-Date) -and
        ($_.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq $codeSigning })
    }
    if ($Thumbprint) {
        $wanted = ($Thumbprint -replace '\s', '').ToUpperInvariant()
        $match = @($all | Where-Object { $_.Thumbprint -eq $wanted })
        $what = "with the thumbprint $wanted"
    }
    else {
        $match = @($all | Where-Object { $_.Subject -like "*$CertificateSubject*" })
        $what = "whose subject contains '$CertificateSubject'"
    }
    if ($match.Count -eq 0) {
        $known = ($all | ForEach-Object { "    $($_.Thumbprint)  $($_.Subject)" }) -join "`n"
        if (-not $known) { $known = '    (none)' }
        Write-Host "Valid code signing certificates in Cert:\CurrentUser\My:`n$known"
        Exit-WithFix "No valid code signing certificate $what." 'Start SimplySign Desktop and log in (token code from the SimplySign app); the certificate then appears in the store. Otherwise check -CertificateSubject / -Thumbprint against the list above.'
    }
    if ($match.Count -gt 1) {
        $known = ($match | ForEach-Object { "    $($_.Thumbprint)  $($_.Subject)  (valid until $($_.NotAfter.ToString('yyyy-MM-dd')))" }) -join "`n"
        Write-Host "Matching certificates:`n$known"
        Exit-WithFix "More than one code signing certificate $what." 'Pick one with -Thumbprint <thumbprint> from the list above.'
    }
    $match[0]
}

# Reads SHA256SUMS ("<hash>  <file>" per line, as sha256sum writes it) into an ordered map.
function Read-Checksum([string] $Path) {
    $sums = [ordered]@{}
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        if ($line -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2]] = $Matches[1].ToLowerInvariant() }
    }
    $sums
}

function Write-Checksum([string] $Path, $Sums) {
    $text = ($Sums.Keys | ForEach-Object { "$($Sums[$_])  $_`n" }) -join ''
    [IO.File]::WriteAllText($Path, $text, (New-Object Text.UTF8Encoding $false))
}

# --- Prerequisites ---------------------------------------------------------------------------

# Windows PowerShell 5.1 has no $IsWindows (and runs only on Windows).
if ($PSVersionTable.PSEdition -eq 'Core' -and -not $IsWindows) {
    Exit-WithFix 'This script runs only on Windows.' 'Run it in PowerShell on the Windows PC with SimplySign Desktop.'
}

Write-Step 'Checking prerequisites'
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Exit-WithFix 'GitHub CLI (gh) not found.' 'Install it (winget install GitHub.cli), open a new PowerShell window, then run: gh auth login'
}
& gh auth status --hostname github.com *> $null
if ($LASTEXITCODE -ne 0) {
    Exit-WithFix 'GitHub CLI is not logged in.' 'Run: gh auth login   (GitHub.com, HTTPS, log in with the browser)'
}
$signtoolExe = Find-SignTool $SignTool
Write-Host "  signtool:    $signtoolExe"
if (-not (Get-Process -Name 'SimplySign*' -ErrorAction SilentlyContinue)) {
    Exit-WithFix 'SimplySign Desktop is not running.' 'Start SimplySign Desktop and log in with your e-mail and the token code from the SimplySign app.'
}
$certificate = Find-Certificate $CertificateSubject $Thumbprint
Write-Host "  certificate: $($certificate.Subject)"
Write-Host "               thumbprint $($certificate.Thumbprint), valid until $($certificate.NotAfter.ToString('yyyy-MM-dd'))"

& gh release view $Tag --repo $Repository --json tagName *> $null
if ($LASTEXITCODE -ne 0) {
    Exit-WithFix "Release $Tag not found in $Repository." 'Check the tag (gh release list --repo hthoene/vanillite) and that the release workflow has finished.'
}

# --- Download --------------------------------------------------------------------------------

if (-not $WorkDir) {
    $WorkDir = Join-Path ([IO.Path]::GetTempPath()) ("vanillite-sign-" + ($Tag -replace '[^0-9A-Za-z.+-]', '_'))
}
if (Test-Path -LiteralPath $WorkDir) { Remove-Item -LiteralPath $WorkDir -Recurse -Force }
$download = Join-Path $WorkDir 'download'
$unpack = Join-Path $WorkDir 'unpack'
$out = Join-Path $WorkDir 'signed'
New-Item -ItemType Directory -Path $download, $unpack, $out -Force | Out-Null

Write-Step "Downloading the Windows archives of $Tag"
Invoke-Native gh @('release', 'download', $Tag, '--repo', $Repository, '--dir', $download,
    '--pattern', 'vanillite-*-windows-*.zip', '--pattern', 'SHA256SUMS')
$zips = @(Get-ChildItem -LiteralPath $download -Filter 'vanillite-*-windows-*.zip')
$sumsPath = Join-Path $download 'SHA256SUMS'
if ($zips.Count -eq 0) { Exit-WithFix "Release $Tag has no Windows zip archive." 'Check the release assets on GitHub; the release workflow builds them.' }
if (-not (Test-Path -LiteralPath $sumsPath)) { Exit-WithFix "Release $Tag has no SHA256SUMS." 'Check the release assets on GitHub; the release workflow creates it.' }
$sums = Read-Checksum $sumsPath
foreach ($zip in $zips) {
    if (-not $sums.Contains($zip.Name)) { throw "SHA256SUMS has no entry for $($zip.Name)." }
    if ((Get-Sha256 $zip.FullName) -ne $sums[$zip.Name]) { throw "$($zip.Name) does not match SHA256SUMS: download again." }
    Write-Host "  $($zip.Name): checksum OK"
}

# --- Sign and pack ---------------------------------------------------------------------------

Add-Type -AssemblyName System.IO.Compression.FileSystem
$changed = @()
foreach ($zip in $zips) {
    Write-Step "Signing $($zip.Name)"
    $dir = Join-Path $unpack $zip.BaseName
    [IO.Compression.ZipFile]::ExtractToDirectory($zip.FullName, $dir)
    # The layout of package.sh: one folder named like the archive holding vanillite.exe.
    $roots = @(Get-ChildItem -LiteralPath $dir -Force)
    if ($roots.Count -ne 1 -or -not $roots[0].PSIsContainer) { throw "$($zip.Name): expected one top-level folder." }
    $exe = Join-Path $roots[0].FullName 'vanillite.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "$($zip.Name): $($roots[0].Name)\vanillite.exe not found." }

    if ((Get-AuthenticodeSignature -LiteralPath $exe).Status -eq 'Valid') {
        Write-Host '  vanillite.exe is already signed; archive left as it is.'
        continue
    }
    Invoke-Native $signtoolExe @('sign', '/sha1', $certificate.Thumbprint, '/fd', 'sha256',
        '/tr', $TimestampUrl, '/td', 'sha256', '/d', 'Vanillite', '/du', "https://github.com/$Repository", $exe)
    Invoke-Native $signtoolExe @('verify', '/pa', '/v', $exe)

    $signed = Join-Path $out $zip.Name
    [IO.Compression.ZipFile]::CreateFromDirectory($roots[0].FullName, $signed,
        [IO.Compression.CompressionLevel]::Optimal, $true)
    $sums[$zip.Name] = Get-Sha256 $signed
    $changed += $signed
    Write-Host "  signed, verified and packed: $signed"
}

if ($changed.Count -eq 0) {
    Write-Step 'Nothing to do: every Windows archive is already signed.'
    exit 0
}
$newSums = Join-Path $out 'SHA256SUMS'
Write-Checksum $newSums $sums

# --- Upload ----------------------------------------------------------------------------------

$assets = $changed + $newSums
if ($DryRun) {
    Write-Step 'Dry run: nothing uploaded. Would replace these release assets:'
    $assets | ForEach-Object { Write-Host "  $_" }
    exit 0
}
Write-Step "Uploading to release $Tag (replacing the unsigned archives)"
Invoke-Native gh (@('release', 'upload', $Tag, '--repo', $Repository, '--clobber') + $assets)
Write-Step "Done: vanillite.exe of $Tag is signed. Files kept in $WorkDir"
