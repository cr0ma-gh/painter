#!/usr/bin/env bash
# ==============================================================================
# Setup GitHub Project V2 Board for Painter Antigravity Workflow
# ==============================================================================
set -euo pipefail

OWNER="cr0ma-gh"
REPO="painter"
TITLE="Painter Development & Antigravity Board"

echo "=== GitHub Projects V2 Setup for Painter ==="
echo "Note: GitHub Projects V2 CLI commands require the 'project' scope."
echo "If you haven't granted it yet, run: gh auth refresh -s project,repo,workflow"
echo ""

# Check auth scope
if ! gh auth status 2>&1 | grep -q "project"; then
    echo "Warning: 'project' scope might be missing. Attempting refresh..."
    gh auth refresh -s project,repo,workflow || true
fi

echo "Creating Project V2 board: '${TITLE}'..."
PROJECT_JSON=$(gh project create --owner "${OWNER}" --title "${TITLE}" --format json)
PROJECT_NUM=$(echo "${PROJECT_JSON}" | grep -o '"number":[0-9]*' | cut -d: -f2)

if [ -z "${PROJECT_NUM}" ]; then
    echo "Error: Failed to retrieve project number."
    exit 1
fi

echo "Created Project #${PROJECT_NUM}"

echo "Linking repository ${OWNER}/${REPO} to project #${PROJECT_NUM}..."
gh project link "${PROJECT_NUM}" --owner "${OWNER}" --repo "${OWNER}/${REPO}" || true

echo "Configuring Agent Workflow field..."
gh project field-create "${PROJECT_NUM}" --owner "${OWNER}" \
    --name "Agent Status" \
    --data-type "SINGLE_SELECT" \
    --single-select-options "Backlog,Ready for Agent,Agent Running,Ready for Review,Done" || true

echo "=== Project Setup Complete! ==="
echo "Project URL: https://github.com/users/${OWNER}/projects/${PROJECT_NUM}"
