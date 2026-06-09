# sandbox/build.ps1
Write-Host "Adding wasm32-wasip1 target..."
rustup target add wasm32-wasip1

Write-Host "Creating sandbox/tools output directory..."
New-Item -ItemType Directory -Force -Path "sandbox/tools"

Write-Host "Building echo-wasm..."
Set-Location -Path "sandbox/echo"
cargo build --target wasm32-wasip1 --release
Copy-Item "target/wasm32-wasip1/release/echo_wasm.wasm" "../tools/echo.wasm" -Force
Set-Location -Path "../.."

Write-Host "Building file-read-wasm..."
Set-Location -Path "sandbox/file_read"
cargo build --target wasm32-wasip1 --release
Copy-Item "target/wasm32-wasip1/release/file_read_wasm.wasm" "../tools/file_read.wasm" -Force
Set-Location -Path "../.."

Write-Host "WASM Build complete!"
