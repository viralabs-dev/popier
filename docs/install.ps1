# popier installer for Windows: downloads the binary from the latest GitHub
# release, checks its SHA-256, installs it and puts it on your PATH.
#
#   irm https://raw.githubusercontent.com/viralabs-dev/popier/main/docs/install.ps1 | iex
#
# Options (environment variables):
#   POPIER_VERSION      a release tag such as v0.3.0 (default: latest)
#   POPIER_INSTALL_DIR  where to put popier.exe (default: %LOCALAPPDATA%\Programs\popier)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Yellow }
function Die($msg) { Write-Host "error: $msg" -ForegroundColor Red; throw $msg }

$repo = 'viralabs-dev/popier'
$version = if ($env:POPIER_VERSION) { $env:POPIER_VERSION } else { 'latest' }
$dir = if ($env:POPIER_INSTALL_DIR) { $env:POPIER_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\popier' }

$arch = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $arch = $env:PROCESSOR_ARCHITEW6432 }
if ($arch -ne 'AMD64') {
    Die "no prebuilt popier for $arch yet. Build it with: cargo install --git https://github.com/$repo"
}
$target = 'x86_64-pc-windows-msvc'
$asset = "popier-$target.tar.gz"
$base = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}

if (-not (Get-Command tar.exe -ErrorAction SilentlyContinue)) {
    Die 'tar.exe is needed to unpack popier (it ships with Windows 10 1803 and later)'
}


# Windows PowerShell 5.1 on older systems may not offer TLS 1.2 by default.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("popier-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Say "downloading $asset ($version)"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    } catch {
        Die "download failed: $base/$asset"
    }

    $sumFile = Join-Path $tmp "$asset.sha256"
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile $sumFile
    } catch {
        Die "checksum download failed: $base/$asset.sha256"
    }
    $expected = ((Get-Content $sumFile -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($expected -ne $actual) { Die 'checksum mismatch, not installing' }
    Say 'checksum ok'

    tar.exe -xzf (Join-Path $tmp $asset) -C $tmp
    $bin = Join-Path $tmp 'popier.exe'
    if (-not (Test-Path $bin)) { Die 'the archive has no popier.exe' }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $exe = Join-Path $dir 'popier.exe'
    # A running popier.exe can't be overwritten, but it can be renamed.
    if (Test-Path $exe) {
        Remove-Item "$exe.old" -Force -ErrorAction SilentlyContinue
        Move-Item $exe "$exe.old" -Force
    }
    Move-Item $bin $exe -Force
    Remove-Item "$exe.old" -Force -ErrorAction SilentlyContinue
    Say "installed $(& $exe --version) to $exe"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($userPath -split ';') -contains $dir)) {
    $newPath = if ($userPath) { "$userPath;$dir" } else { $dir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    $env:Path = "$env:Path;$dir"
    Say "added $dir to your PATH (open a new terminal to use it everywhere)"
}

Write-Host ''
Write-Host 'Next:'
Write-Host '  popier init --discover   # find your repos (uses your gh login)'
Write-Host '  popier                   # watch'
Write-Host '  popier --update          # get new releases later'
