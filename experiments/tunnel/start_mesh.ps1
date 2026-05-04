# Run from experiments\tunnel\ — working directory matters for cert paths.
# Usage: powershell -ExecutionPolicy Bypass -File start_mesh.ps1

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# Ensure Rust's cargo is on PATH (default install location)
$cargoBin = "$env:USERPROFILE\.cargo\bin"
if (Test-Path $cargoBin) {
    $env:PATH = "$cargoBin;$env:PATH"
} else {
    throw "cargo not found. Install Rust from https://rustup.rs and re-open PowerShell."
}

# Kill any stray processes from a previous run
Write-Host "Cleaning up old processes..."
Get-Process -Name "python"        -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process -Name "main-server"   -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process -Name "volunteer-agent" -ErrorAction SilentlyContinue | Stop-Process -Force

Start-Sleep -Seconds 1

# Start the mock target service
Write-Host "Starting Python web server on port 9000..."
$python = Start-Process -FilePath "python" -ArgumentList "-m http.server 9000" -PassThru -WindowStyle Hidden

Start-Sleep -Seconds 1

# Start the main server
Write-Host "Starting main-server (gRPC :50051, HTTP :8080)..."
$server = Start-Process -FilePath "cargo" -ArgumentList "run --bin main-server" `
    -RedirectStandardOutput "main.log" -RedirectStandardError "main.err.log" `
    -PassThru -WindowStyle Hidden

Start-Sleep -Seconds 4

# Start two volunteer agents
Write-Host "Starting volunteer agents..."
$vol1 = Start-Process -FilePath "cargo" -ArgumentList "run --bin volunteer-agent" `
    -RedirectStandardOutput "vol1.log" -RedirectStandardError "vol1.err.log" `
    -PassThru -WindowStyle Hidden

$vol2 = Start-Process -FilePath "cargo" -ArgumentList "run --bin volunteer-agent" `
    -RedirectStandardOutput "vol2.log" -RedirectStandardError "vol2.err.log" `
    -PassThru -WindowStyle Hidden

Write-Host ""
Write-Host "=========================================================="
Write-Host "All components running. Try:"
Write-Host ""
Write-Host "  curl http://127.0.0.1:8080/"
Write-Host "  start http://127.0.0.1:8080/dashboard"
Write-Host ""
Write-Host "Follow logs:"
Write-Host "  Get-Content main.log -Wait"
Write-Host "  Get-Content vol1.log -Wait"
Write-Host "=========================================================="
Write-Host ""
Write-Host "Press Ctrl+C to stop all processes..."

try {
    Wait-Process -Id $server.Id
} finally {
    Write-Host "Shutting down..."
    $python, $server, $vol1, $vol2 | ForEach-Object {
        if ($_ -and !$_.HasExited) { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
    }
}
