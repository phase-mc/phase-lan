# Builds the installer: dist\Phase-Setup-<version>.exe and dist\release.json.
#
#   powershell -ExecutionPolicy Bypass -File tools\build-setup.ps1
#   ...                                -AppExe <path>   use a ready release Phase.exe
#
# The installer is not packed with UPX and is never run here against the real system: the only
# check of the finished file is --verify-payload, which reads nothing but the file itself.
[CmdletBinding()]
param(
    [string]$AppExe = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$sizeLimit = 25MB   # Cloudflare Pages limit for a single file

function Invoke-Checked([string]$what, [scriptblock]$action) {
    Write-Host "== $what"
    & $action
    if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" }
}

$cargoToml = Get-Content -Raw -Encoding UTF8 (Join-Path $root 'Cargo.toml')
if ($cargoToml -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    throw 'No version in Cargo.toml'
}
$version = $Matches[1]

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }

if (-not $AppExe) {
    $gui = Join-Path $root 'crates\gui'
    Push-Location $gui
    try {
        if (-not (Test-Path 'node_modules')) {
            Invoke-Checked 'npm ci' { npm ci --no-audit --no-fund }
        }
        # Phase LAN only: no `client` cargo feature and no Client page in the front end.
        $savedClientBuild = $env:PHASE_CLIENT_BUILD
        Remove-Item Env:PHASE_CLIENT_BUILD -ErrorAction SilentlyContinue
        try {
            Invoke-Checked 'Release build of Phase' { npx tauri build --no-bundle }
        } finally {
            if ($null -ne $savedClientBuild) { $env:PHASE_CLIENT_BUILD = $savedClientBuild }
        }
    } finally {
        Pop-Location
    }
    $AppExe = Join-Path $targetDir 'release\Phase.exe'
}
$AppExe = (Resolve-Path $AppExe).Path

# Read by crates\setup\build.rs: the app is embedded compressed.
$env:PHASE_SETUP_PAYLOAD = $AppExe
Remove-Item Env:PHASE_SETUP_BROTLI_QUALITY -ErrorAction SilentlyContinue
Push-Location $root
try {
    Invoke-Checked 'Installer build' { cargo build -p phase-setup --profile setup --locked }
} finally {
    Pop-Location
    Remove-Item Env:PHASE_SETUP_PAYLOAD -ErrorAction SilentlyContinue
}

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$fileName = "Phase-Setup-$version.exe"
$output = Join-Path $dist $fileName
Copy-Item -Force (Join-Path $targetDir 'setup\phase-setup.exe') $output

$check = Start-Process -FilePath $output -ArgumentList '--verify-payload' -Wait -PassThru -WindowStyle Hidden
if ($check.ExitCode -ne 0) { throw "Embedded app check: exit code $($check.ExitCode)" }

$bytes = [IO.File]::ReadAllBytes($output)
foreach ($forbidden in 'PHASE_SETUP_SANDBOX', 'PHASE_SETUP_FAULT') {
    $text = [Text.Encoding]::ASCII.GetString($bytes)
    if ($text.Contains($forbidden)) { throw "The release file contains the debug setting $forbidden" }
}

$size = (Get-Item $output).Length
if ($size -ge $sizeLimit) {
    throw ('Installer is {0:N1} MiB, over the {1} MiB limit' -f ($size / 1MB), ($sizeLimit / 1MB))
}
$hash = (Get-FileHash -Algorithm SHA256 $output).Hash.ToLowerInvariant()

# The website reads this schema.
$release = [ordered]@{
    version    = $version
    fileName   = $fileName
    sizeBytes  = $size
    installedBytes = (Get-Item $AppExe).Length
    sha256     = $hash
    url        = "/download/$fileName"
    releasedAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
    virusTotal = $null
}
$json = $release | ConvertTo-Json
[IO.File]::WriteAllText((Join-Path $dist 'release.json'), $json + "`n", (New-Object Text.UTF8Encoding($false)))

Write-Host ('Done: {0} ({1:N1} MiB)' -f $output, ($size / 1MB))
Write-Host "SHA-256: $hash"

# Optional local post-build step (not part of the public repository). A failure here does not
# undo the finished installer.
$postBuild = Join-Path $PSScriptRoot 'sync-phase-folder.ps1'
if (Test-Path $postBuild) {
    try {
        & $postBuild
    } catch {
        Write-Warning "Post-build step failed: $($_.Exception.Message)"
    }
}
