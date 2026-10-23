# documentum installer for Windows (PowerShell 5+):
#   irm https://documentum.kerliane.eu/install.ps1 | iex
#
# Downloads the latest GitHub release (x86_64), verifies SHA256SUMS, installs
# to %LOCALAPPDATA%\documentum\bin and adds it to the user PATH.
#   $env:DOCUMENTUM_VERSION = "v3.0.1"   pin a release tag
#   $env:GH_TOKEN                        only for private forks, or to lift API rate limits
$ErrorActionPreference = "Stop"

$Repo    = if ($env:DOCUMENTUM_REPO) { $env:DOCUMENTUM_REPO } else { "kerbart/documentum" }
$Version = if ($env:DOCUMENTUM_VERSION) { $env:DOCUMENTUM_VERSION } else { "latest" }
$Token   = if ($env:GH_TOKEN) { $env:GH_TOKEN } elseif ($env:GITHUB_TOKEN) { $env:GITHUB_TOKEN } else { $null }
$BinDir  = Join-Path $env:LOCALAPPDATA "documentum\bin"
$Asset   = "documentum-windows-x86_64.zip"

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne "X64") {
  throw "documentum: only x86_64 Windows builds are published"
}
Write-Host "▍ documentum installer" -ForegroundColor Yellow

if ($Version -eq "latest") {
  $Base = "https://github.com/$Repo/releases/latest/download"
  $Api  = "https://api.github.com/repos/$Repo/releases/latest"
} else {
  $Base = "https://github.com/$Repo/releases/download/$Version"
  $Api  = "https://api.github.com/repos/$Repo/releases/tags/$Version"
}
$Tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("documentum-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $Tmp | Out-Null

function Fetch($Name, $Dest) {
  if (-not $Token) {
    Invoke-WebRequest -Uri "$Base/$Name" -OutFile $Dest -UseBasicParsing
  } else {
    $h = @{ Authorization = "Bearer $Token"; Accept = "application/vnd.github+json" }
    $rel = Invoke-RestMethod -Uri $Api -Headers $h
    $a = $rel.assets | Where-Object { $_.name -eq $Name } | Select-Object -First 1
    if (-not $a) { throw "no asset $Name in release" }
    $h.Accept = "application/octet-stream"
    Invoke-WebRequest -Uri $a.url -Headers $h -OutFile $Dest -UseBasicParsing
  }
}

Write-Host "downloading $Asset ($Version)" -ForegroundColor DarkGray
try { Fetch $Asset (Join-Path $Tmp $Asset) }
catch {
  if (-not $Token) { Write-Host "private fork or rate-limited? set `$env:GH_TOKEN and retry" -ForegroundColor DarkGray }
  throw
}
Fetch "SHA256SUMS" (Join-Path $Tmp "SHA256SUMS")

$expected = (Get-Content (Join-Path $Tmp "SHA256SUMS") | Where-Object { $_ -match "\s\*?$([regex]::Escape($Asset))$" } | Select-Object -First 1) -split "\s+" | Select-Object -First 1
if (-not $expected) { throw "SHA256SUMS has no entry for $Asset" }
$actual = (Get-FileHash (Join-Path $Tmp $Asset) -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected.ToLower()) { throw "checksum mismatch for $Asset" }
Write-Host "✓ sha256 verified" -ForegroundColor Green

New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
Expand-Archive -Path (Join-Path $Tmp $Asset) -DestinationPath $Tmp -Force
Move-Item -Force (Join-Path $Tmp "documentum.exe") (Join-Path $BinDir "documentum.exe")
Remove-Item -Recurse -Force $Tmp
Write-Host "✓ installed: $BinDir\documentum.exe" -ForegroundColor Green

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $BinDir) {
  [Environment]::SetEnvironmentVariable("Path", "$userPath;$BinDir", "User")
  $env:Path += ";$BinDir"
  Write-Host "added $BinDir to the user PATH (open a new terminal)" -ForegroundColor DarkGray
}
Write-Host ""
Write-Host "Next:"
Write-Host "  cd my-workspace; documentum setup" -ForegroundColor DarkGray
Write-Host "  documentum search `"how do I deploy the api?`"" -ForegroundColor DarkGray
