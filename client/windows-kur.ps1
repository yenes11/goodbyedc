# goodbyedc - Windows icin tek dosyalik exe olusturucu (elle derleme)
# Not: Normalde exe GitHub Actions'ta otomatik derlenir. Bu betikle derlenen
# exe kendini guncelleyemez; sadece deneme icin kullan.
# Calistirmak icin: windows-kur.bat dosyasina cift tikla.
$ErrorActionPreference = "Continue"
Set-Location -Path (Join-Path $PSScriptRoot "src-tauri")

function Adim($m) { Write-Host ""; Write-Host "==> $m" -ForegroundColor Cyan }
function Var($cmd) { [bool](Get-Command $cmd -ErrorAction SilentlyContinue) }

$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

# 1) Visual Studio C++ derleme araclari
Adim "Visual Studio C++ derleme araclari kontrol ediliyor"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vc = $null
if (Test-Path $vswhere) {
  $vc = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
}
if (-not $vc) {
  Write-Host "Bulunamadi, kuruluyor (birkac GB, 10-20 dakika surebilir)..."
  winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements `
    --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
} else { Write-Host "Tamam: $vc" }

# 2) Rust
Adim "Rust kontrol ediliyor"
if (-not (Var cargo)) {
  Write-Host "Bulunamadi, kuruluyor..."
  winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
  $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
rustup default stable-msvc
cargo --version

# 3) Tauri CLI
Adim "Tauri CLI kontrol ediliyor"
$null = cmd /c "cargo tauri --version 2>nul"
if ($LASTEXITCODE -ne 0) {
  Write-Host "Kuruluyor (birkac dakika)..."
  cargo install tauri-cli --version "^2" --locked
}

# 3.5) WebRTC kutuphanesini kendimiz indir (derleme sirasindaki indirme zaman asimina ugrayabiliyor)
Adim "WebRTC kutuphanesi kontrol ediliyor"
$wrtcRoot = Join-Path $env:LOCALAPPDATA "goodbyedc-webrtc\webrtc-a545de4"
$ninja = Get-ChildItem $wrtcRoot -Recurse -Filter webrtc.ninja -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $ninja) {
  New-Item -ItemType Directory -Force $wrtcRoot | Out-Null
  $zip = Join-Path $wrtcRoot "webrtc.zip"
  $url = "https://github.com/livekit/rust-sdks/releases/download/webrtc-a545de4/webrtc-win-x64-release.zip"
  $ok = $false
  # Tarayiciyla indirilmis dosya varsa onu kullan.
  $elle = Join-Path $env:USERPROFILE "Downloads\webrtc-win-x64-release.zip"
  if (Test-Path $elle) {
    Write-Host "Indirilenler klasorundeki dosya kullaniliyor: $elle"
    Copy-Item $elle $zip -Force
    $ok = $true
  } else {
    Write-Host "Indiriliyor (birkac yuz MB). Kesilirse kaldigi yerden devam eder..."
  }
  for ($i = 1; $i -le 10 -and -not $ok; $i++) {
    curl.exe -L --fail --connect-timeout 30 --retry 3 --retry-delay 5 -C - -o $zip $url
    if ($LASTEXITCODE -eq 0) { $ok = $true; break }
    Write-Host "Indirme kesildi, tekrar deneniyor ($i/10)..."
    Start-Sleep -Seconds 5
  }
  if (-not $ok) { throw "WebRTC indirilemedi. Internet baglantini kontrol edip tekrar dene." }
  Write-Host "Arsivden cikariliyor..."
  tar.exe -xf $zip -C $wrtcRoot
  if ($LASTEXITCODE -ne 0) { throw "WebRTC arsivi acilamadi." }
  Remove-Item $zip -Force
  $ninja = Get-ChildItem $wrtcRoot -Recurse -Filter webrtc.ninja | Select-Object -First 1
  if (-not $ninja) { throw "WebRTC arsivi beklenen icerige sahip degil." }
}
$env:LK_CUSTOM_WEBRTC = $ninja.DirectoryName
Write-Host "Tamam: $env:LK_CUSTOM_WEBRTC"

# 4) Derle (kurulum dosyasi yok, tek basina calisan exe)
Adim "Uygulama derleniyor (ilk seferde 10-20 dakika surebilir)"
cargo tauri build --no-bundle
if ($LASTEXITCODE -ne 0) { throw "Derleme basarisiz oldu." }

# 5) Exe'yi disari kopyala
$exe = "target\release\goodbyedc.exe"
if (-not (Test-Path $exe)) { throw "goodbyedc.exe bulunamadi." }
$hedef = Join-Path $PSScriptRoot "goodbyedc.exe"
Copy-Item $exe $hedef -Force
Adim "Hazir: $hedef"
Write-Host "Bu dosyayi arkadaslarina gonderebilirsin, kurulum gerektirmez."

explorer.exe /select,"$hedef"
