# ==============================================================================
# Antigravity PowerShell Watcher Daemon for Painter (Windows Native)
# ==============================================================================
param(
    [int]$Interval = 60,
    [string]$Repo = "cr0ma-gh/painter",
    [ValidateSet("auto", "antigravity", "claude", "agy")]
    [string]$Engine = "auto",
    [string]$Model = "gemini-3.8-flash"
)

$ErrorActionPreference = "Continue"
$RepoRoot = (Get-Item $PSScriptRoot).Parent.FullName

# Automatically load .env if present
$envFile = Join-Path $RepoRoot ".env"
if (Test-Path $envFile) {
    Get-Content $envFile | ForEach-Object {
        $line = $_.Trim()
        if ($line -and -not $line.StartsWith("#") -and $line.Contains("=")) {
            $parts = $line.Split("=", 2)
            $k = $parts[0].Trim()
            $v = $parts[1].Trim().Trim("'`"")
            if (-not [System.Environment]::GetEnvironmentVariable($k)) {
                [System.Environment]::SetEnvironmentVariable($k, $v, "Process")
            }
        }
    }
}

$LockFile = Join-Path $RepoRoot ".antigravity\watcher.lock"
$LogDir = Join-Path $RepoRoot ".antigravity\logs"

if (-not (Test-Path $LogDir)) {
    New-Item -ItemType Directory -Path $LogDir -Force | Out-Null
}

function Log-Message {
    param([string]$Text)
    $ts = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    Write-Host "[$ts] $Text"
}

Log-Message "================================================================"
Log-Message "🚀 Antigravity PowerShell Watcher Daemon Initialized"
Log-Message "Repository: $RepoRoot"
Log-Message "Polling Interval: $Interval seconds"
Log-Message "AI Engine Preference: $Engine"
Log-Message "================================================================"

# Verify gh
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Log-Message "Error: GitHub CLI (gh) not found in PATH."
    exit 1
}

$PID | Out-File -FilePath $LockFile -Encoding ascii -Force

try {
    while ($true) {
        Log-Message "Scanning GitHub for issues labeled 'agent:ready'..."
        
        $jsonStr = gh issue list --repo $Repo --label "agent:ready" --state open --json number,title | Out-String
        if ($jsonStr -and $jsonStr.Trim() -ne "[]") {
            $issues = $jsonStr | ConvertFrom-Json
            Log-Message "Detected $($issues.Count) pending issue(s) ready for agent processing."
            
            # Delegate to antigravity_runner.py with engine and model flags
            $runnerPath = Join-Path $PSScriptRoot "antigravity_runner.py"
            python $runnerPath --engine $Engine --model $Model --once
        }
        else {
            Log-Message "Queue idle. No tasks labeled 'agent:ready'."
        }

        Start-Sleep -Seconds $Interval
    }
}
finally {
    if (Test-Path $LockFile) {
        Remove-Item $LockFile -Force -ErrorAction SilentlyContinue
    }
    Log-Message "Antigravity Watcher stopped."
}
