#!/usr/bin/env bash
# ==============================================================================
# Setup GitHub Labels for Painter & Antigravity Workflow
# ==============================================================================
set -euo pipefail

create_or_update_label() {
    local name="$1"
    local color="$2"
    local desc="$3"
    echo "Creating/updating label: ${name}..."
    gh label create "${name}" --color "${color}" --description "${desc}" --force
}

echo "Configuring GitHub labels for repository..."

# Agent Workflow Statuses
create_or_update_label "agent:ready" "0E8A16" "Ready for autonomous agent processing"
create_or_update_label "agent:running" "FBCA04" "Autonomous agent is currently executing task"
create_or_update_label "agent:review" "1D76DB" "Task completed by agent, PR ready for review"
create_or_update_label "agent:failed" "D93F0B" "Agent encountered errors or test failures"
create_or_update_label "status:backlog" "6E5494" "Task planned in backlog, not ready for agent"
create_or_update_label "status:done" "0052CC" "Task verified, merged and closed"

# Crate & Architectural Scopes
create_or_update_label "scope:core" "5319E7" "painter-core: TileStore, CPU engine, raster, document"
create_or_update_label "scope:render" "006B75" "painter-render: wgpu, WGSL shaders, tile atlas"
create_or_update_label "scope:app" "BFD4F2" "painter-app: eframe/egui UI, docking, tablet bridges"
create_or_update_label "scope:brush-io" "D4C5F9" "brush-io: Photoshop .abr, Clip Studio .sut/.todb, .rustbrush"
create_or_update_label "scope:transform" "C2E0C6" "transform-tools: mesh warp, perspective, affine transform"
create_or_update_label "scope:comic" "FEF2C0" "comic-layout: panels, frames, speech balloons"
create_or_update_label "scope:scene-3d" "F9D0C4" "scene-3d: 3D model reference layer and wgpu renderer"
create_or_update_label "scope:vector" "C5DEF5" "vector-export: SVG export, centerline vector extraction"
create_or_update_label "scope:ci" "E99695" "CI/CD, automation, DevOps, GitHub Actions"

# Task Types
create_or_update_label "type:antigravity" "7057FF" "Autonomous high-density specification task"
create_or_update_label "type:feature" "A2EEEF" "New feature or module extension"
create_or_update_label "type:bug" "D73A4A" "Defect, crash, rendering glitch or race condition"
create_or_update_label "type:perf" "FFC107" "Performance optimization or memory reduction"
create_or_update_label "type:refactor" "BDBDBD" "Internal refactoring without API breakage"

# AI Engine Routing
create_or_update_label "engine:antigravity" "4285F4" "Execute task using Google Antigravity (agy / SDK)"
create_or_update_label "engine:claude" "D97706" "Execute task using Anthropic Claude Code"

# Priorities
create_or_update_label "priority:critical" "B60205" "Immediate blocker / critical defect"
create_or_update_label "priority:high" "D93F0B" "High priority task for agent queue"
create_or_update_label "priority:medium" "FBCA04" "Normal development priority"
create_or_update_label "priority:low" "0E8A16" "Low priority / polish task"

echo "All labels successfully configured!"
