# Setup GitHub Labels for Painter & Antigravity Workflow
$labels = @(
    # Agent Workflow Statuses
    @{ name = "agent:ready"; color = "0E8A16"; desc = "Ready for autonomous agent processing" },
    @{ name = "agent:running"; color = "FBCA04"; desc = "Autonomous agent is currently executing task" },
    @{ name = "agent:review"; color = "1D76DB"; desc = "Task completed by agent, PR ready for review" },
    @{ name = "agent:failed"; color = "D93F0B"; desc = "Agent encountered errors or test failures" },
    @{ name = "status:backlog"; color = "6E5494"; desc = "Task planned in backlog, not ready for agent" },
    @{ name = "status:done"; color = "0052CC"; desc = "Task verified, merged and closed" },

    # Crate & Architectural Scopes
    @{ name = "scope:core"; color = "5319E7"; desc = "painter-core: TileStore, CPU engine, raster, document" },
    @{ name = "scope:render"; color = "006B75"; desc = "painter-render: wgpu, WGSL shaders, tile atlas" },
    @{ name = "scope:app"; color = "BFD4F2"; desc = "painter-app: eframe/egui UI, docking, tablet bridges" },
    @{ name = "scope:brush-io"; color = "D4C5F9"; desc = "brush-io: Photoshop .abr, Clip Studio .sut/.todb, .rustbrush" },
    @{ name = "scope:transform"; color = "C2E0C6"; desc = "transform-tools: mesh warp, perspective, affine transform" },
    @{ name = "scope:comic"; color = "FEF2C0"; desc = "comic-layout: panels, frames, speech balloons" },
    @{ name = "scope:scene-3d"; color = "F9D0C4"; desc = "scene-3d: 3D model reference layer and wgpu renderer" },
    @{ name = "scope:vector"; color = "C5DEF5"; desc = "vector-export: SVG export, centerline vector extraction" },
    @{ name = "scope:ci"; color = "E99695"; desc = "CI/CD, automation, DevOps, GitHub Actions" },

    # Task Types
    @{ name = "type:antigravity"; color = "7057FF"; desc = "Autonomous high-density specification task" },
    @{ name = "type:feature"; color = "A2EEEF"; desc = "New feature or module extension" },
    @{ name = "type:bug"; color = "D73A4A"; desc = "Defect, crash, rendering glitch or race condition" },
    @{ name = "type:perf"; color = "FFC107"; desc = "Performance optimization or memory reduction" },
    @{ name = "type:refactor"; color = "BDBDBD"; desc = "Internal refactoring without API breakage" },

    # AI Engine Routing
    @{ name = "engine:antigravity"; color = "4285F4"; desc = "Execute task using Google Antigravity (agy / SDK)" },
    @{ name = "engine:claude"; color = "D97706"; desc = "Execute task using Anthropic Claude Code" },

    # Priorities
    @{ name = "priority:critical"; color = "B60205"; desc = "Immediate blocker / critical defect" },
    @{ name = "priority:high"; color = "D93F0B"; desc = "High priority task for agent queue" },
    @{ name = "priority:medium"; color = "FBCA04"; desc = "Normal development priority" },
    @{ name = "priority:low"; color = "0E8A16"; desc = "Low priority / polish task" }
)

Write-Host "Configuring GitHub labels for repository..." -ForegroundColor Cyan
foreach ($label in $labels) {
    Write-Host "Creating/updating label: $($label.name)..."
    gh label create $label.name --color $label.color --description $label.desc --force
}
Write-Host "All labels successfully configured!" -ForegroundColor Green
