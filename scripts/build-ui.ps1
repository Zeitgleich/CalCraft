# Builds the web UI (crates/calcraft-ui) into crates/calcraft-ui/dist/.
# Windows counterpart of scripts/build-ui.sh; see there for details.
$ErrorActionPreference = 'Stop'

$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$ui = Join-Path $root 'crates/calcraft-ui'
$dist = Join-Path $ui 'dist'
$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
$wasmBindgen = if ($env:WASM_BINDGEN) { $env:WASM_BINDGEN } else { 'wasm-bindgen' }

$pinned = (Select-String -Path (Join-Path $ui 'Cargo.toml') -Pattern '^wasm-bindgen = "=([0-9.]+)"').Matches[0].Groups[1].Value
$installed = ((& $wasmBindgen --version) -split ' ')[1]
if ($pinned -ne $installed) {
    throw "wasm-bindgen CLI $installed does not match the pinned crate version $pinned (cargo install wasm-bindgen-cli --version $pinned --locked)"
}

cargo build --manifest-path (Join-Path $root 'Cargo.toml') --locked -p calcraft-ui `
    --target wasm32-unknown-unknown --profile wasm-release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Path $dist | Out-Null
& $wasmBindgen --target web --no-typescript --out-dir $dist --out-name app `
    (Join-Path $targetDir 'wasm32-unknown-unknown/wasm-release/calcraft-ui.wasm')
if ($LASTEXITCODE -ne 0) { throw 'wasm-bindgen failed' }
Copy-Item (Join-Path $ui 'style.css') (Join-Path $dist 'style.css')
# Icons and favicon (sources and exports: assets/brand/, see docs/brand.md).
$brand = Join-Path $root 'assets/brand'
foreach ($file in 'favicon.svg', 'favicon.ico', 'apple-touch-icon.png') {
    Copy-Item (Join-Path $brand $file) (Join-Path $dist $file)
}
Copy-Item (Join-Path $brand 'logo-192.png') (Join-Path $dist 'icon-192.png')
Copy-Item (Join-Path $brand 'logo-512.png') (Join-Path $dist 'icon-512.png')

Write-Output "web UI assets written to ${dist}:"
Get-ChildItem $dist
