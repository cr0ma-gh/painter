# Setup GitHub Project V2 Board for Painter Antigravity Workflow (PowerShell)
param(
    [string]$Owner = "cr0ma-gh",
    [string]$Repo = "painter",
    [string]$Title = "Painter Development & Antigravity Board"
)

Write-Host "=== GitHub Projects V2 Setup for Painter ===" -ForegroundColor Cyan
Write-Host "Note: GitHub Projects V2 CLI commands require the 'project' scope."
Write-Host "Run 'gh auth refresh -s project,repo,workflow' if not done yet.`n"

try {
    Write-Host "Creating Project V2 board: '$Title'..."
    $projJson = gh project create --owner $Owner --title $Title --format json | Out-String | ConvertFrom-Json
    $projNum = $projJson.number

    if (-not $projNum) {
        throw "Failed to extract project number from gh project create."
    }

    Write-Host "Created Project #$projNum" -ForegroundColor Green

    Write-Host "Linking repository $Owner/$Repo to project #$projNum..."
    gh project link $projNum --owner $Owner --repo "$Owner/$Repo"

    Write-Host "Configuring 'Agent Status' field..."
    gh project field-create $projNum --owner $Owner --name "Agent Status" --data-type "SINGLE_SELECT" --single-select-options "Backlog,Ready for Agent,Agent Running,Ready for Review,Done"

    Write-Host "`nProject Setup Complete!" -ForegroundColor Green
    Write-Host "Project URL: https://github.com/users/$Owner/projects/$projNum" -ForegroundColor Yellow
}
catch {
    Write-Error "Failed to configure project: $_"
}
