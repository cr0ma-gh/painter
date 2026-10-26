#!/usr/bin/env bash
# ==============================================================================
# Antigravity Bash Watcher Daemon for Painter
# ==============================================================================
# Lightweight, native bash loop designed for systemd, tmux, or dedicated terminal.
# Continuously queries GitHub for issues labeled 'agent:ready', delegates to
# the Antigravity runner engine or executes Google Antigravity / Claude Code directly.
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Automatically load .env if present
if [ -f "${REPO_ROOT}/.env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "${REPO_ROOT}/.env"
    set +a
fi

POLL_INTERVAL="${ANTIGRAVITY_INTERVAL:-60}"
ENGINE="${ANTIGRAVITY_ENGINE:-auto}"
MODEL="${ANTIGRAVITY_MODEL:-gemini-3.8-flash}"
LOCK_FILE="${REPO_ROOT}/.antigravity/watcher.lock"
LOG_DIR="${REPO_ROOT}/.antigravity/logs"

# Parse CLI flag overrides
while [[ $# -gt 0 ]]; do
    case $1 in
        --engine)
            ENGINE="$2"
            shift 2
            ;;
        --model)
            MODEL="$2"
            shift 2
            ;;
        --interval)
            POLL_INTERVAL="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

mkdir -p "${LOG_DIR}"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*"
}

cleanup() {
    log "Stopping Antigravity Watcher..."
    rm -f "${LOCK_FILE}"
    exit 0
}

trap cleanup SIGINT SIGTERM

if [ -f "${LOCK_FILE}" ]; then
    OLD_PID=$(cat "${LOCK_FILE}" || true)
    if [ -n "${OLD_PID}" ] && kill -0 "${OLD_PID}" 2>/dev/null; then
        log "Error: Antigravity watcher is already running (PID: ${OLD_PID})."
        exit 1
    fi
fi

echo $$ > "${LOCK_FILE}"

log "================================================================"
log "🚀 Antigravity Bash Watcher initialized"
log "Repository: ${REPO_ROOT}"
log "Polling interval: ${POLL_INTERVAL} seconds"
log "AI Engine preference: ${ENGINE}"
log "================================================================"

# Verify gh CLI
if ! command -v gh &>/dev/null; then
    log "Error: GitHub CLI (gh) not found in PATH. Please install gh."
    exit 1
fi

while true; do
    log "Checking for issues with label 'agent:ready'..."

    READY_ISSUES=$(gh issue list --repo cr0ma-gh/painter --label "agent:ready" --state open --json number,title,labels --limit 5)

    ISSUE_COUNT=$(echo "${READY_ISSUES}" | grep -o '"number":' | wc -l || true)

    if [ "${ISSUE_COUNT}" -gt 0 ]; then
        log "Detected ${ISSUE_COUNT} pending issue(s) ready for agent processing."

        # Prefer executing Python orchestrator for robust atomic transactions and test gates
        if command -v python3 &>/dev/null; then
            log "Delegating execution to antigravity_runner.py (model: ${MODEL})..."
            python3 "${SCRIPT_DIR}/antigravity_runner.py" --engine "${ENGINE}" --model "${MODEL}" --once || log "Runner returned non-zero status."
        elif command -v python &>/dev/null; then
            log "Delegating execution to antigravity_runner.py (model: ${MODEL})..."
            python "${SCRIPT_DIR}/antigravity_runner.py" --engine "${ENGINE}" --model "${MODEL}" --once || log "Runner returned non-zero status."
        else
            # Native bash fallback if python is unavailable
            TARGET_NUM=$(echo "${READY_ISSUES}" | grep -o '"number":[0-9]*' | head -n1 | cut -d: -f2)
            TARGET_TITLE=$(echo "${READY_ISSUES}" | grep -o '"title":"[^"]*"' | head -n1 | cut -d: -f2 | tr -d '"')

            # Determine agent binary
            EXEC_BIN=""
            if [[ "${ENGINE}" == "antigravity" || "${ENGINE}" == "agy" ]] && command -v agy &>/dev/null; then
                EXEC_BIN="agy"
            elif command -v agy &>/dev/null; then
                EXEC_BIN="agy"
            elif command -v claude &>/dev/null; then
                EXEC_BIN="claude"
            fi

            log "Running native bash runner on Issue #${TARGET_NUM}: ${TARGET_TITLE} (Agent: ${EXEC_BIN:-none})"

            # Claim issue
            gh issue edit "${TARGET_NUM}" --remove-label "agent:ready" --add-label "agent:running"
            gh issue comment "${TARGET_NUM}" --body "🤖 **Antigravity Bash Watcher**: Inizio elaborazione sul nodo local `$(hostname)` (Engine: ${EXEC_BIN:-auto})."

            BRANCH="feature/issue-${TARGET_NUM}-auto"
            git checkout main
            git pull origin main || true
            git checkout -b "${BRANCH}"

            PROMPT="Implement task #${TARGET_NUM}: ${TARGET_TITLE}. Follow PROJECT_CONTEXT.md. Verify with cargo test --workspace."

            if [ "${EXEC_BIN}" = "agy" ]; then
                agy -p "${PROMPT}" || true
            elif [ "${EXEC_BIN}" = "claude" ]; then
                claude -p "${PROMPT}" --dangerously-skip-permissions || true
            else
                log "Warning: Neither 'agy' nor 'claude' CLI found. Skipping agent invocation."
            fi

            # Verification
            if cargo test --workspace; then
                git add -A
                git commit -m "feat(issue-${TARGET_NUM}): ${TARGET_TITLE}" || true
                git push -u origin "${BRANCH}"
                PR_URL=$(gh pr create --draft --head "${BRANCH}" --title "feat(#${TARGET_NUM}): ${TARGET_TITLE}" --body "Closes #${TARGET_NUM}")
                gh issue edit "${TARGET_NUM}" --remove-label "agent:running" --add-label "agent:review"
                gh issue comment "${TARGET_NUM}" --body "✅ Task completato. PR: ${PR_URL}"
            else
                gh issue edit "${TARGET_NUM}" --remove-label "agent:running" --add-label "agent:failed"
                gh issue comment "${TARGET_NUM}" --body "❌ Test falliti durante la verifica locale."
            fi
            git checkout main
        fi
    else
        log "Queue idle. No tasks labeled 'agent:ready'."
    fi

    log "Sleeping ${POLL_INTERVAL}s..."
    sleep "${POLL_INTERVAL}"
done
