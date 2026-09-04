param(
    [string]$Version = "0.1.0-dev-27ca5507",
    [string]$ArtifactBaseUrl = "https://downloads.acentric.dev",
    [string]$InstallDirectory = "$env:LOCALAPPDATA\Programs\execution-host"
)

$ErrorActionPreference = "Stop"
if (-not [Environment]::Is64BitOperatingSystem) {
    throw "execution-host currently requires 64-bit Windows"
}

$target = "x86_64-pc-windows-gnu"
$artifactUrl = "$ArtifactBaseUrl/execution-host/$Version/$target/execution-host.exe"
$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null

try {
    $binary = Join-Path $temporaryDirectory "execution-host.exe"
    $checksum = Join-Path $temporaryDirectory "execution-host.exe.sha256"
    Invoke-WebRequest -UseBasicParsing -Uri $artifactUrl -OutFile $binary
    Invoke-WebRequest -UseBasicParsing -Uri "$artifactUrl.sha256" -OutFile $checksum

    $expected = ((Get-Content -Raw $checksum).Trim() -split "\s+")[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $binary).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "execution-host checksum verification failed"
    }

    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    $destination = Join-Path $InstallDirectory "execution-host.exe"
    Copy-Item -Force $binary $destination
    & $destination --version
    Write-Output "Installed $destination"
} finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $temporaryDirectory
}
