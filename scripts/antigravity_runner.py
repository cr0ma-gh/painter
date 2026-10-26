#!/usr/bin/env python3
"""
Antigravity Runner: Frictionless Autonomous Development Engine for Painter
==========================================================================
Monitors GitHub Issues labeled 'agent:ready', claims tasks atomically,
isolates Git workspaces, prompts the chosen AI Engine (Google Antigravity or Claude Code),
runs local test/clippy verification gates, and publishes Draft PRs for review.

Supported AI Engines:
  - 'antigravity' (Google Antigravity via Python SDK or `agy` CLI)
  - 'claude' (Anthropic Claude Code CLI)
  - 'auto' (Auto-detects available engine or per-issue labels)

Usage:
    python scripts/antigravity_runner.py --daemon --interval 60 --engine antigravity
    python scripts/antigravity_runner.py --daemon --interval 60 --engine claude
    python scripts/antigravity_runner.py --once
    python scripts/antigravity_runner.py --interactive 42 --engine antigravity
    python scripts/antigravity_runner.py --issue 42
"""

import argparse
import asyncio
import datetime
import json
import logging
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional, Tuple

# Ensure stdout/stderr on Windows handles UTF-8 without charmap encoding crashes
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

# ------------------------------------------------------------------------------
# Configuration & Defaults
# ------------------------------------------------------------------------------
REPO_ROOT = Path(__file__).resolve().parent.parent

def load_dotenv(env_path: Path):
    """Load key-value pairs from .env into os.environ if not already present."""
    if not env_path.exists():
        return
    try:
        for line in env_path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, val = line.split("=", 1)
            key = key.strip()
            val = val.strip().strip("'\"")
            if key and key not in os.environ:
                os.environ[key] = val
    except Exception as e:
        pass

load_dotenv(REPO_ROOT / ".env")

ANTIGRAVITY_DIR = REPO_ROOT / ".antigravity"
LOCK_FILE = ANTIGRAVITY_DIR / "runner.lock"
LOGS_DIR = ANTIGRAVITY_DIR / "logs"
PROMPTS_DIR = ANTIGRAVITY_DIR / "prompts"

DEFAULT_TEST_COMMAND = "cargo clippy --workspace --all-targets && cargo test --workspace"
DEFAULT_ENGINE = os.environ.get("ANTIGRAVITY_ENGINE", "auto")

PRIORITY_WEIGHTS = {
    "priority:critical": 100,
    "priority:high": 75,
    "priority:medium": 50,
    "priority:low": 25,
}

logging.basicConfig(
    level=logging.INFO,
    format="[%(asctime)s] [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger("antigravity")


# ------------------------------------------------------------------------------
# Process & Lock Management
# ------------------------------------------------------------------------------
class LockManager:
    """Manages PID-based lockfile to avoid concurrent overlapping runners."""

    def __init__(self, lock_path: Path):
        self.lock_path = lock_path

    def acquire(self) -> bool:
        self.lock_path.parent.mkdir(parents=True, exist_ok=True)
        if self.lock_path.exists():
            try:
                content = self.lock_path.read_text().strip()
                old_pid = int(content.split()[0])
                if self._pid_is_active(old_pid):
                    logger.warning(f"Another Antigravity runner is currently active (PID {old_pid}).")
                    return False
                else:
                    logger.info(f"Removing stale lock from defunct PID {old_pid}.")
            except Exception:
                pass
        self.lock_path.write_text(f"{os.getpid()} {datetime.datetime.now().isoformat()}\n")
        return True

    def release(self):
        try:
            if self.lock_path.exists():
                self.lock_path.unlink()
        except Exception as e:
            logger.error(f"Failed to release lockfile: {e}")

    @staticmethod
    def _pid_is_active(pid: int) -> bool:
        if platform.system() == "Windows":
            cmd = f'tasklist /FI "PID eq {pid}" /NH'
            res = subprocess.run(cmd, capture_output=True, text=True, shell=True)
            return str(pid) in res.stdout
        else:
            try:
                os.kill(pid, 0)
                return True
            except (OSError, ProcessLookupError):
                return False


# ------------------------------------------------------------------------------
# GitHub CLI Interface
# ------------------------------------------------------------------------------
class GitHubClient:
    """Wrapper around GitHub CLI (gh) for issues, PRs, and labeling."""

    @staticmethod
    def run_gh(args: List[str], check: bool = True) -> subprocess.CompletedProcess:
        cmd = ["gh"] + args
        try:
            return subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=check,
                cwd=REPO_ROOT,
                encoding="utf-8",
                errors="replace",
            )
        except subprocess.CalledProcessError as e:
            logger.error(f"GitHub CLI command failed: {' '.join(cmd)}\nStderr: {e.stderr}")
            raise

    def fetch_ready_issues(self) -> List[Dict]:
        """Fetch all open issues labeled 'agent:ready' with priority sorting."""
        fields = "number,title,body,labels,author,createdAt,url"
        res = self.run_gh(["issue", "list", "--label", "agent:ready", "--state", "open", "--json", fields])
        if not res.stdout.strip():
            return []

        try:
            issues = json.loads(res.stdout)
        except json.JSONDecodeError as e:
            logger.error(f"Failed to decode issues JSON: {e}")
            return []

        def sort_key(issue: Dict):
            label_names = [lbl["name"] for lbl in issue.get("labels", [])]
            priority = 50  # Default medium
            for name in label_names:
                if name in PRIORITY_WEIGHTS:
                    priority = PRIORITY_WEIGHTS[name]
                    break
            created_at = issue.get("createdAt", "")
            # High priority first (negative weight), then oldest first (FIFO)
            return (-priority, created_at)

        issues.sort(key=sort_key)
        return issues

    def claim_issue(self, issue_number: int, branch_name: str, engine_name: str) -> bool:
        """Atomically remove 'agent:ready' and add 'agent:running'."""
        logger.info(f"Claiming Issue #{issue_number} using engine '{engine_name}'...")
        self.run_gh([
            "issue", "edit", str(issue_number),
            "--remove-label", "agent:ready",
            "--add-label", "agent:running",
        ])
        hostname = platform.node()
        comment = (
            f"🤖 **Antigravity Runner**: Task preso in carico con successo!\n\n"
            f"- **Host**: `{hostname}`\n"
            f"- **Timestamp**: `{datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}`\n"
            f"- **AI Engine**: `{engine_name}`\n"
            f"- **Working Branch**: `{branch_name}`\n"
            f"- **Status**: ⏳ Elaborazione autonoma in corso...\n"
        )
        self.add_comment(issue_number, comment)
        return True

    def mark_review(self, issue_number: int, pr_url: str, summary: str, engine_name: str):
        """Mark issue as 'agent:review' with PR link and test summary."""
        self.run_gh([
            "issue", "edit", str(issue_number),
            "--remove-label", "agent:running",
            "--add-label", "agent:review",
        ])
        comment = (
            f"✅ **Antigravity Runner**: Task completato con successo!\n\n"
            f"- **AI Engine**: `{engine_name}`\n"
            f"- **Pull Request**: {pr_url}\n"
            f"- **Verifica Locale**: `cargo clippy` e `cargo test` superati.\n\n"
            f"### 📋 Riepilogo Esecuzione\n```\n{summary}\n```\n\n"
            f"Pronto per la code review umana e il merge."
        )
        self.add_comment(issue_number, comment)

    def mark_failed(self, issue_number: int, error_log: str, engine_name: str):
        """Mark issue as 'agent:failed' and attach execution stack trace."""
        self.run_gh([
            "issue", "edit", str(issue_number),
            "--remove-label", "agent:running",
            "--add-label", "agent:failed",
        ])
        trimmed_log = "\n".join(error_log.strip().splitlines()[-40:])
        comment = (
            f"❌ **Antigravity Runner**: Riscontrata anomalia durante l'elaborazione del task.\n\n"
            f"- **Stato**: `agent:failed`\n"
            f"- **AI Engine**: `{engine_name}`\n"
            f"- **Host**: `{platform.node()}`\n\n"
            f"### 🔍 Log dell'Errore / Test Failure\n```\n{trimmed_log}\n```\n\n"
            f"Verificare la specifica del task o i vincoli architetturali."
        )
        self.add_comment(issue_number, comment)

    def requeue_transient(self, issue_number: int, error_log: str, engine_name: str):
        """Put issue back into 'agent:ready' due to temporary API provider outage (HTTP 503/429)."""
        self.run_gh([
            "issue", "edit", str(issue_number),
            "--remove-label", "agent:running",
            "--add-label", "agent:ready",
        ])
        trimmed_log = "\n".join(error_log.strip().splitlines()[-10:])
        comment = (
            f"⏳ **Antigravity Runner**: Il provider AI (`{engine_name}`) è temporaneamente non disponibile "
            f"(HTTP 503 High Demand / Rate Limit).\n\n"
            f"```\n{trimmed_log}\n```\n\n"
            f"Il task è stato **riaccodato automaticamente** in `agent:ready` per essere riprovato "
            f"al prossimo ciclo di polling non appena i server torneranno disponibili."
        )
        self.add_comment(issue_number, comment)

    def add_comment(self, issue_number: int, comment: str):
        try:
            self.run_gh(["issue", "comment", str(issue_number), "--body", comment])
        except Exception as e:
            logger.error(f"Failed to post comment on issue #{issue_number}: {e}")

    def create_draft_pr(self, branch: str, issue_number: int, title: str, body: str, engine_name: str) -> str:
        """Create a Draft PR linked to the issue."""
        pr_title = f"feat(issue-{issue_number}): {title}"
        pr_body = (
            f"## 🤖 Antigravity Autonomous PR\n\n"
            f"Closes #{issue_number}\n\n"
            f"- **Generated by**: Antigravity Runner (`{engine_name}`)\n\n"
            f"### Descrizione Modifiche\n{body}\n\n"
            f"### Verification Gate\n"
            f"- [x] `cargo check --workspace`\n"
            f"- [x] `cargo clippy --workspace --all-targets`\n"
            f"- [x] `cargo test --workspace`\n"
        )
        res = self.run_gh([
            "pr", "create",
            "--draft",
            "--head", branch,
            "--title", pr_title,
            "--body", pr_body,
        ])
        return res.stdout.strip()


# ------------------------------------------------------------------------------
# Git Workspace Controller
# ------------------------------------------------------------------------------
class GitController:
    """Handles branching, syncing, staging, and committing."""

    def __init__(self, repo_dir: Path):
        self.repo_dir = repo_dir

    def run_git(self, args: List[str], check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["git"] + args,
            capture_output=True,
            text=True,
            check=check,
            cwd=self.repo_dir,
            encoding="utf-8",
            errors="replace",
        )

    def ensure_clean_workspace(self):
        status = self.run_git(["status", "--porcelain"]).stdout.strip()
        if status:
            logger.warning(f"Workspace has unstaged or untracked changes:\n{status}")
            logger.info("Stashing current changes for safety...")
            self.run_git(["stash", "save", "antigravity-auto-stash"])

    def checkout_main(self):
        logger.info("Switching to main branch and syncing with origin...")
        self.run_git(["checkout", "main"])
        try:
            self.run_git(["pull", "--rebase", "origin", "main"])
        except subprocess.CalledProcessError:
            logger.warning("Failed to rebase pull from origin/main, continuing with local main.")

    def create_and_checkout_branch(self, issue_number: int, title: str, is_bug: bool = False) -> str:
        prefix = "fix" if is_bug else "feature"
        slug = re.sub(r"[^a-zA-Z0-9_-]", "-", title.lower())
        slug = re.sub(r"-+", "-", slug).strip("-")[:40]
        branch_name = f"{prefix}/issue-{issue_number}-{slug}"

        logger.info(f"Creating and checking out branch '{branch_name}'...")
        branches = self.run_git(["branch", "--list", branch_name]).stdout.strip()
        if branches:
            self.run_git(["checkout", branch_name])
            self.run_git(["reset", "--hard", "main"])
        else:
            self.run_git(["checkout", "-b", branch_name])
        return branch_name

    def has_new_commits_or_changes(self) -> bool:
        diff = self.run_git(["status", "--porcelain"]).stdout.strip()
        diff_main = self.run_git(["rev-list", "main..HEAD"]).stdout.strip()
        return bool(diff or diff_main)

    def commit_all_if_dirty(self, issue_number: int, title: str, engine_name: str):
        status = self.run_git(["status", "--porcelain"]).stdout.strip()
        if status:
            logger.info("Staging and committing residual changes from agent...")
            self.run_git(["add", "-A"])
            msg = (
                f"feat(issue-{issue_number}): {title}\n\n"
                f"Implemented autonomously via Antigravity Runner ({engine_name})."
            )
            self.run_git(["commit", "-m", msg])

    def push_branch(self, branch_name: str):
        logger.info(f"Pushing branch {branch_name} to origin...")
        self.run_git(["push", "-u", "--force-with-lease", "origin", branch_name])

    def get_recent_commits(self, count: int = 5) -> str:
        res = self.run_git(["log", f"-n{count}", "--oneline"])
        return res.stdout.strip()


# ------------------------------------------------------------------------------
# Context Prompt Synthesizer
# ------------------------------------------------------------------------------
class PromptSynthesizer:
    """Assembles rich context combining repository rules, issue specs, and workspace state."""

    @staticmethod
    def build_prompt(issue: Dict, branch_name: str, recent_commits: str, engine_name: str) -> str:
        issue_number = issue.get("number")
        title = issue.get("title", "")
        body = issue.get("body", "")
        author = issue.get("author", {}).get("login", "unknown")
        labels = [l.get("name", "") for l in issue.get("labels", [])]

        context_file = REPO_ROOT / "PROJECT_CONTEXT.md"
        context_content = ""
        if context_file.exists():
            context_content = context_file.read_text(encoding="utf-8")
        else:
            arch_file = REPO_ROOT / "docs" / "ARCHITECTURE.md"
            if arch_file.exists():
                context_content = arch_file.read_text(encoding="utf-8")

        prompt = f"""# TASK CONTEXT FOR AUTONOMOUS AGENT ({engine_name.upper()})
You are working autonomously on the repository `painter`.
Your task is defined by GitHub Issue #{issue_number}.

================================================================================
PROJECT ARCHITECTURAL RULES & CONSTRAINTS
================================================================================
{context_content}

================================================================================
ASSIGNED GITHUB TASK SPECIFICATION
================================================================================
- **Issue**: #{issue_number}
- **Title**: {title}
- **Author**: @{author}
- **Labels**: {', '.join(labels)}
- **Dedicated Branch**: `{branch_name}`

--- ISSUE BODY SPECIFICATION ---
{body}
--- END ISSUE BODY ---

================================================================================
CURRENT WORKSPACE STATUS
================================================================================
Recent Git Commits:
{recent_commits}

================================================================================
MANDATORY EXECUTION INSTRUCTIONS FOR THE AGENT
================================================================================
1. Read and analyze the relevant files in `crates/` mentioned in the task.
2. Implement the solution cleanly while adhering strictly to:
   - Zero breaking changes to existing public APIs.
   - Mathematical and functional parity between CPU tile rasterization and GPU WGSL shaders.
   - 64x64 premultiplied RGBA8 tile invariants.
3. Test and verify your changes:
   - Run `cargo check --workspace`
   - Run `cargo clippy --workspace --all-targets`
   - Run `cargo test --workspace`
   Fix any compilation errors, linter warnings, or failing tests.
4. Commit your changes cleanly with Conventional Commits (e.g. `feat(scope): description` or `fix(scope): description`).
5. When finished, provide a concise summary of what was implemented and what tests passed.
"""
        return prompt


# ------------------------------------------------------------------------------
# AI Engine Provider Implementations (Google Antigravity & Claude Code)
# ------------------------------------------------------------------------------
def is_transient_error(error_text: str) -> bool:
    """Detect temporary upstream capacity or network errors (HTTP 503, 429, etc.)."""
    patterns = [
        r"503",
        r"high demand",
        r"unavailable",
        r"temporarily unavailable",
        r"429",
        r"resource exhausted",
        r"rate limit",
        r"too many requests",
        r"502",
        r"bad gateway",
        r"504",
        r"gateway timeout",
        r"connection reset",
        r"timed out",
        r"unreachable",
    ]
    text_lower = error_text.lower()
    return any(re.search(p, text_lower) for p in patterns)


def is_billing_exhausted(error_text: str) -> bool:
    """Detect prepayment or billing depletion error (HTTP 402)."""
    patterns = [r"402", r"prepayment credits", r"depleted", r"billing#prepay"]
    text_lower = error_text.lower()
    return any(re.search(p, text_lower) for p in patterns)


class BaseAIEngine:
    """Abstract interface for AI execution backends."""
    name: str = "base"

    def execute_headless(self, prompt: str, issue_number: int, log_file: Path, dry_run: bool) -> Tuple[bool, str]:
        raise NotImplementedError

    def execute_interactive(self, prompt: str, issue_number: int):
        raise NotImplementedError


class ClaudeCodeEngine(BaseAIEngine):
    """Execution backend using Anthropic's Claude Code CLI (`claude`)."""
    name = "Claude Code"

    def __init__(self, executable: str = "claude"):
        self.executable = shutil.which(executable) or executable

    def execute_headless(self, prompt: str, issue_number: int, log_file: Path, dry_run: bool) -> Tuple[bool, str]:
        cmd = [self.executable, "-p", prompt, "--dangerously-skip-permissions"]
        if dry_run:
            logger.info(f"[DRY-RUN] [{self.name}] Would execute: {' '.join(cmd[:2])} ... (prompt len {len(prompt)})")
            return True, "Dry-run execution simulated successfully."

        try:
            with open(log_file, "w", encoding="utf-8") as lf:
                proc = subprocess.Popen(
                    cmd,
                    cwd=REPO_ROOT,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    shell=(platform.system() == "Windows"),
                )
                captured = []
                for line in proc.stdout:
                    try:
                        sys.stdout.write(line)
                        sys.stdout.flush()
                    except Exception:
                        pass
                    lf.write(line)
                    captured.append(line)
                proc.wait()

            return (proc.returncode == 0), "".join(captured)
        except FileNotFoundError:
            err = (
                f"Executable '{self.executable}' not found on PATH.\n"
                f"Install Claude Code via: npm install -g @anthropic-ai/claude-code"
            )
            logger.error(err)
            return False, err
        except Exception as e:
            return False, str(e)

    def execute_interactive(self, prompt: str, issue_number: int):
        prompt_file = PROMPTS_DIR / f"interactive-prompt-issue-{issue_number}.md"
        prompt_file.write_text(prompt, encoding="utf-8")
        logger.info(f"[{self.name}] Prompt written to {prompt_file}. Launching interactive CLI...")
        try:
            subprocess.run([self.executable], cwd=REPO_ROOT, shell=(platform.system() == "Windows"))
        except FileNotFoundError:
            logger.error(f"Executable '{self.executable}' not found on PATH.")
            sys.exit(1)


DEFAULT_CASCADING_MODELS = [
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-flash-latest",
    "gemini-3.1-flash-lite",
]


class GoogleAntigravityEngine(BaseAIEngine):
    """
    Execution backend using Google Antigravity.
    Supports either the native Python SDK (`google-antigravity`) or the CLI (`agy`).
    Includes automatic model cascading when encountering HTTP 503 high demand spikes
    or 404 model availability changes.
    """
    name = "Google Antigravity"

    def __init__(self, cli_executable: str = "agy", api_key: Optional[str] = None, model: Optional[str] = None):
        self.cli_executable = cli_executable
        self.api_key = api_key
        self.model = model or os.environ.get("ANTIGRAVITY_MODEL", "gemini-3.8-flash")
        self._sdk_available = self._check_sdk()

    @staticmethod
    def _check_sdk() -> bool:
        try:
            import importlib.util
            return importlib.util.find_spec("google.antigravity") is not None
        except Exception:
            return False

    def get_api_key(self) -> Optional[str]:
        return self.api_key or os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")

    def execute_headless(self, prompt: str, issue_number: int, log_file: Path, dry_run: bool) -> Tuple[bool, str]:
        if dry_run:
            logger.info(f"[DRY-RUN] [{self.name}] Would execute prompt (length {len(prompt)})...")
            return True, "Dry-run execution simulated successfully."

        # Model cascade list: try primary first, then fallbacks
        primary = self.model or "gemini-3.8-flash"
        candidates = [primary] + [m for m in DEFAULT_CASCADING_MODELS if m != primary]

        last_log = ""
        for idx, model_name in enumerate(candidates):
            logger.info(f"[{self.name}] Trying model '{model_name}' (candidate {idx + 1}/{len(candidates)})...")

            if self._sdk_available:
                ok, log = self._execute_via_sdk(prompt, log_file, model_name)
            else:
                ok, log = self._execute_via_cli(prompt, log_file, model_name)

            if ok:
                logger.info(f"[{self.name}] Execution successful using model '{model_name}'!")
                return True, log

            last_log = log

            # If 402 (prepayment credits), changing model on the same project will not help
            if is_billing_exhausted(log):
                return False, log

            # If transient 503 or model 404 deprecated/unavailable, cascade to next candidate
            is_unavailable = (
                is_transient_error(log)
                or "404" in log
                or "no longer available" in log.lower()
                or "not found" in log.lower()
            )

            if is_unavailable and idx < len(candidates) - 1:
                next_model = candidates[idx + 1]
                logger.warning(
                    f"[{self.name}] Model '{model_name}' unavailable. "
                    f"Cascading to fallback model '{next_model}'..."
                )
                time.sleep(2)
            else:
                return False, log

        return False, last_log

    def _execute_via_sdk(self, prompt: str, log_file: Path, model_name: str) -> Tuple[bool, str]:
        key = self.get_api_key()
        if not key:
            err = (
                "A Gemini API key is required. Set it via:\n"
                "  - .env file in repo root: GEMINI_API_KEY=your_api_key\n"
                "  - Environment variable:   export GEMINI_API_KEY='your_api_key'\n"
                "  - Windows PowerShell:     $env:GEMINI_API_KEY='your_api_key'\n"
                "  - CLI argument:           --gemini-api-key 'your_api_key'\n"
                "You can obtain a key from https://aistudio.google.com/"
            )
            logger.error(err)
            return False, err

        try:
            from google.antigravity import Agent, LocalAgentConfig, CapabilitiesConfig

            async def _run():
                config = LocalAgentConfig(
                    model=model_name,
                    api_key=key,
                    system_instructions=(
                        "You are an expert autonomous software engineer working on Painter. "
                        "Follow PROJECT_CONTEXT.md, implement changes, verify with cargo clippy and tests, "
                        "and commit using Conventional Commits."
                    ),
                    capabilities=CapabilitiesConfig(),
                )
                output_tokens = []
                with open(log_file, "w", encoding="utf-8") as lf:
                    async with Agent(config) as agent:
                        response = await agent.chat(prompt)
                        async for token in response:
                            sys.stdout.write(token)
                            sys.stdout.flush()
                            lf.write(token)
                            output_tokens.append(token)
                return True, "".join(output_tokens)

            return asyncio.run(_run())
        except Exception as e:
            logger.error(f"[{self.name} SDK Error ({model_name})]: {e}")
            return False, str(e)

    def _execute_via_cli(self, prompt: str, log_file: Path, model_name: str) -> Tuple[bool, str]:
        cmd = [self.cli_executable, "--model", model_name, "-p", prompt]
        env = os.environ.copy()
        key = self.get_api_key()
        if key:
            env["GEMINI_API_KEY"] = key
            env["GOOGLE_API_KEY"] = key
        env["ANTIGRAVITY_MODEL"] = model_name

        try:
            with open(log_file, "w", encoding="utf-8") as lf:
                proc = subprocess.Popen(
                    cmd,
                    cwd=REPO_ROOT,
                    env=env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                )
                captured = []
                for line in proc.stdout:
                    try:
                        sys.stdout.write(line)
                        sys.stdout.flush()
                    except Exception:
                        pass
                    lf.write(line)
                    captured.append(line)
                proc.wait()

            return (proc.returncode == 0), "".join(captured)
        except FileNotFoundError:
            err = (
                f"Google Antigravity CLI '{self.cli_executable}' not found on system PATH.\n"
                f"To install Google Antigravity:\n"
                f"  - Python SDK: pip install google-antigravity\n"
                f"  - Or CLI: see https://antigravity.google/docs/cli/reference"
            )
            logger.error(err)
            return False, err
        except Exception as e:
            return False, str(e)

    def execute_interactive(self, prompt: str, issue_number: int):
        prompt_file = PROMPTS_DIR / f"interactive-prompt-issue-{issue_number}.md"
        prompt_file.write_text(prompt, encoding="utf-8")
        logger.info(f"[{self.name}] Prompt written to {prompt_file}.")

        key = self.get_api_key()
        model_name = self.model or "gemini-2.5-flash"

        if self._sdk_available:
            if not key:
                logger.error(
                    "A Gemini API key is required. Set it in .env (GEMINI_API_KEY=...) "
                    "or via $env:GEMINI_API_KEY / export GEMINI_API_KEY."
                )
                sys.exit(1)

            try:
                from google.antigravity import Agent, LocalAgentConfig, CapabilitiesConfig
                from google.antigravity.utils.interactive import run_interactive_loop

                async def _run_interactive():
                    config = LocalAgentConfig(model=model_name, api_key=key, capabilities=CapabilitiesConfig())
                    async with Agent(config) as agent:
                        await agent.chat(prompt)
                        await run_interactive_loop(agent)

                asyncio.run(_run_interactive())
                return
            except Exception as e:
                logger.warning(f"Interactive SDK loop encountered error: {e}. Falling back to CLI.")

        env = os.environ.copy()
        if key:
            env["GEMINI_API_KEY"] = key
            env["GOOGLE_API_KEY"] = key
        env["ANTIGRAVITY_MODEL"] = model_name

        try:
            subprocess.run([self.cli_executable, "--model", model_name], cwd=REPO_ROOT, env=env)
        except Exception:
            subprocess.run([self.cli_executable], cwd=REPO_ROOT, env=env)


# ------------------------------------------------------------------------------
# Antigravity Runner Core Engine
# ------------------------------------------------------------------------------
class AntigravityRunner:
    """Orchestrates task retrieval, branch isolation, agent execution, and verification."""

    def __init__(self, args: argparse.Namespace):
        self.args = args
        self.gh = GitHubClient()
        self.git = GitController(REPO_ROOT)
        self.lock = LockManager(LOCK_FILE)

        ANTIGRAVITY_DIR.mkdir(parents=True, exist_ok=True)
        LOGS_DIR.mkdir(parents=True, exist_ok=True)
        PROMPTS_DIR.mkdir(parents=True, exist_ok=True)

    def resolve_engine(self, issue: Dict) -> BaseAIEngine:
        """
        Dynamically select the AI Engine based on:
        1. Explicit CLI flag (--engine claude or --engine antigravity)
        2. Issue label ('engine:claude' or 'engine:antigravity')
        3. Issue body text (if selected in dropdown)
        4. Auto-detection of installed binaries
        """
        labels = [l.get("name", "") for l in issue.get("labels", [])]
        body = issue.get("body", "")

        # 1. Explicit CLI argument takes TOP priority if not 'auto'
        selected = self.args.engine.lower()
        if selected in ("claude", "anthropic"):
            logger.info("Task routed to Claude Code via explicit CLI flag '--engine claude'.")
            return ClaudeCodeEngine(executable=self.args.claude_cmd)
        if selected in ("antigravity", "agy", "google"):
            logger.info("Task routed to Google Antigravity via explicit CLI flag '--engine antigravity'.")
            return GoogleAntigravityEngine(cli_executable=self.args.agy_cmd, api_key=self.args.gemini_api_key, model=getattr(self.args, "model", None))

        # 2. Issue Label check (when CLI is auto)
        if "engine:claude" in labels:
            logger.info("Task routed to Claude Code via issue label 'engine:claude'.")
            return ClaudeCodeEngine(executable=self.args.claude_cmd)
        if "engine:antigravity" in labels:
            logger.info("Task routed to Google Antigravity via issue label 'engine:antigravity'.")
            return GoogleAntigravityEngine(cli_executable=self.args.agy_cmd, api_key=self.args.gemini_api_key, model=getattr(self.args, "model", None))

        # 3. Body dropdown check
        if "Claude Code" in body and "Google Antigravity" not in body:
            logger.info("Task routed to Claude Code via issue body specification.")
            return ClaudeCodeEngine(executable=self.args.claude_cmd)
        if "Google Antigravity" in body and "Claude Code" not in body:
            logger.info("Task routed to Google Antigravity via issue body specification.")
            return GoogleAntigravityEngine(cli_executable=self.args.agy_cmd, api_key=self.args.gemini_api_key, model=getattr(self.args, "model", None))

        # 4. Auto mode: check what's installed
        has_agy = shutil.which(self.args.agy_cmd) is not None or GoogleAntigravityEngine._check_sdk()
        has_claude = shutil.which(self.args.claude_cmd) is not None

        if has_agy and not has_claude:
            logger.info("Auto-engine: detected Google Antigravity.")
            return GoogleAntigravityEngine(cli_executable=self.args.agy_cmd, api_key=self.args.gemini_api_key, model=getattr(self.args, "model", None))
        elif has_claude and not has_agy:
            logger.info("Auto-engine: detected Claude Code.")
            return ClaudeCodeEngine(executable=self.args.claude_cmd)
        elif has_agy and has_claude:
            # If both, default to Google Antigravity
            logger.info("Auto-engine: both engines available; defaulting to Google Antigravity.")
            return GoogleAntigravityEngine(cli_executable=self.args.agy_cmd, api_key=self.args.gemini_api_key, model=getattr(self.args, "model", None))
        else:
            # Fallback default
            return ClaudeCodeEngine(executable=self.args.claude_cmd)

    def run_tests(self) -> Tuple[bool, str]:
        """Execute the verification test suite."""
        test_cmd = self.args.test_cmd or DEFAULT_TEST_COMMAND
        logger.info(f"Running verification gate: {test_cmd}...")
        try:
            res = subprocess.run(
                test_cmd,
                shell=True,
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            output = f"STDOUT:\n{res.stdout}\n\nSTDERR:\n{res.stderr}"
            if res.returncode == 0:
                logger.info("Verification gate PASSED!")
                return True, output
            else:
                logger.error(f"Verification gate FAILED with return code {res.returncode}")
                return False, output
        except Exception as e:
            logger.error(f"Error running verification gate: {e}")
            return False, str(e)

    def process_issue(self, issue: Dict) -> bool:
        """Full end-to-end processing pipeline for a single issue."""
        issue_number = issue["number"]
        title = issue.get("title", "")
        labels = [l.get("name", "") for l in issue.get("labels", [])]
        is_bug = any("bug" in l for l in labels)

        # Resolve engine
        engine = self.resolve_engine(issue)

        logger.info(f"==================================================")
        logger.info(f"Processing Issue #{issue_number}: {title}")
        logger.info(f"Selected AI Engine: {engine.name}")
        logger.info(f"==================================================")

        # 1. Clean workspace and checkout main
        self.git.ensure_clean_workspace()
        self.git.checkout_main()

        # 2. Create isolated task branch
        branch_name = self.git.create_and_checkout_branch(issue_number, title, is_bug=is_bug)

        # 3. Claim the issue on GitHub
        if not self.args.dry_run:
            self.gh.claim_issue(issue_number, branch_name, engine.name)

        # 4. Synthesize context prompt
        recent_commits = self.git.get_recent_commits(5)
        prompt = PromptSynthesizer.build_prompt(issue, branch_name, recent_commits, engine.name)

        # 5. Handle Interactive vs Headless execution
        if self.args.interactive:
            engine.execute_interactive(prompt, issue_number)
            logger.info("Interactive session completed. Checking verification gate...")
            passed, test_log = self.run_tests()
            if passed:
                logger.info("Verification tests passed!")
            else:
                logger.warning("Verification tests did not pass.")
            return passed

        # Headless Execution
        log_file = LOGS_DIR / f"agent-issue-{issue_number}-{int(time.time())}.log"
        prompt_file = PROMPTS_DIR / f"prompt-issue-{issue_number}.md"
        prompt_file.write_text(prompt, encoding="utf-8")

        agent_ok, agent_log = engine.execute_headless(prompt, issue_number, log_file, self.args.dry_run)

        # Automatic Failover to Claude Code if Google Antigravity suffers from 503 spike or 402 depleted credits
        if not agent_ok and not self.args.dry_run and (is_transient_error(agent_log) or is_billing_exhausted(agent_log)):
            if isinstance(engine, GoogleAntigravityEngine) and shutil.which(self.args.claude_cmd):
                logger.warning(
                    f"[{engine.name}] encountered upstream limits / depleted credits. "
                    "Automatically failing over to Claude Code..."
                )
                fallback_engine = ClaudeCodeEngine(executable=self.args.claude_cmd)
                agent_ok, agent_log = fallback_engine.execute_headless(prompt, issue_number, log_file, self.args.dry_run)
                if agent_ok:
                    engine = fallback_engine

        if not agent_ok and not self.args.dry_run:
            if is_billing_exhausted(agent_log):
                err_msg = (
                    "Google API Error 402: Prepayment credits are depleted.\n"
                    "La chiave API Gemini usata è collegata a un progetto Google Cloud con crediti prepagati a zero.\n\n"
                    "Come risolvere:\n"
                    "1. Crea una chiave API 100% gratuita senza billing su: https://aistudio.google.com/app/apikey\n"
                    "   (Seleziona 'Create API key in NEW project' e NON collegare alcun account di fatturazione).\n"
                    "2. Oppure installa Claude Code: npm install -g @anthropic-ai/claude-code ed esegui con --engine claude."
                )
                logger.error(err_msg)
                self.gh.mark_failed(issue_number, err_msg, engine.name)
            elif is_transient_error(agent_log):
                logger.warning(
                    f"Task #{issue_number} encountered temporary upstream capacity limits (HTTP 503/429). "
                    f"Re-queuing issue in 'agent:ready' for the next cycle..."
                )
                self.gh.requeue_transient(issue_number, agent_log, engine.name)
            else:
                logger.error(f"Agent execution failed for Issue #{issue_number}.")
                self.gh.mark_failed(issue_number, agent_log, engine.name)
            self.git.checkout_main()
            return False

        # 6. Run Project Verification Gate
        tests_passed, test_log = self.run_tests()
        if not tests_passed and not self.args.dry_run:
            logger.error(f"Verification tests failed for Issue #{issue_number}.")
            self.gh.mark_failed(issue_number, f"Test Suite Failure:\n{test_log}", engine.name)
            self.git.checkout_main()
            return False

        # 7. Check for changes and wrap up
        if not self.args.dry_run:
            self.git.commit_all_if_dirty(issue_number, title, engine.name)
            self.git.push_branch(branch_name)

            # Create Draft PR
            pr_body = (
                f"Automated PR generated by Antigravity Runner using **{engine.name}** for #{issue_number}.\n\n"
                f"### Verification Summary\n"
                f"- Linter (`clippy`): Clean\n"
                f"- Tests (`cargo test`): Passed\n"
            )
            pr_url = self.gh.create_draft_pr(branch_name, issue_number, title, pr_body, engine.name)
            logger.info(f"Created Draft PR: {pr_url}")

            # Mark issue as ready for review
            summary = (
                f"AI Engine: {engine.name}\n"
                f"Branch: {branch_name}\n"
                f"Draft PR: {pr_url}\n"
                f"Tests verified."
            )
            self.gh.mark_review(issue_number, pr_url, summary, engine.name)

            logger.info(f"Issue #{issue_number} successfully completed and submitted for review!")
            self.git.checkout_main()

        return True

    def run(self):
        if not self.lock.acquire():
            sys.exit(1)

        try:
            # Interactive mode targeting a specific issue
            if self.args.interactive:
                issue_num = int(self.args.interactive)
                res = self.gh.run_gh(["issue", "view", str(issue_num), "--json", "number,title,body,labels,author"])
                issue = json.loads(res.stdout)
                self.process_issue(issue)
                return

            # Specific issue in headless mode
            if self.args.issue:
                issue_num = int(self.args.issue)
                res = self.gh.run_gh(["issue", "view", str(issue_num), "--json", "number,title,body,labels,author"])
                issue = json.loads(res.stdout)
                self.process_issue(issue)
                return

            # Continuous Daemon / Polling Loop
            interval = max(10, self.args.interval)
            logger.info(f"Starting Antigravity Daemon (polling every {interval}s, engine: {self.args.engine})...")

            while True:
                try:
                    logger.info("Scanning for issues with label 'agent:ready'...")
                    issues = self.gh.fetch_ready_issues()

                    if issues:
                        logger.info(f"Found {len(issues)} ready task(s). Processing highest priority...")
                        target_issue = issues[0]
                        self.process_issue(target_issue)

                        if self.args.once:
                            logger.info("Single task mode completed. Exiting.")
                            break
                    else:
                        logger.info("No tasks currently labeled 'agent:ready'.")

                    if self.args.once:
                        break

                    logger.info(f"Sleeping for {interval}s before next poll...")
                    time.sleep(interval)

                except KeyboardInterrupt:
                    logger.info("Antigravity Runner stopped by user.")
                    break
                except Exception as e:
                    logger.error(f"Error during runner cycle: {e}", exc_info=True)
                    if self.args.once:
                        break
                    time.sleep(interval)

        finally:
            self.lock.release()


# ------------------------------------------------------------------------------
# Entrypoint CLI Parser
# ------------------------------------------------------------------------------
def main():
    parser = argparse.ArgumentParser(
        description="Antigravity Runner: Continuous autonomous agent development engine for Painter."
    )
    parser.add_argument(
        "--engine",
        type=str,
        default=DEFAULT_ENGINE,
        choices=["auto", "antigravity", "claude", "agy"],
        help="AI Agent engine backend: 'antigravity' (Google Antigravity), 'claude' (Claude Code), or 'auto' (default).",
    )
    parser.add_argument(
        "--daemon",
        action="store_true",
        help="Run continuously in background daemon mode polling for new tasks.",
    )
    parser.add_argument(
        "--interval",
        type=int,
        default=60,
        help="Polling interval in seconds for daemon mode (default: 60).",
    )
    parser.add_argument(
        "--once",
        action="store_true",
        help="Process at most one ready task and exit immediately.",
    )
    parser.add_argument(
        "--interactive",
        type=int,
        metavar="ISSUE_ID",
        help="Prepare task workspace, branch, and launch an interactive terminal session with chosen engine.",
    )
    parser.add_argument(
        "--issue",
        type=int,
        metavar="ISSUE_ID",
        help="Directly process a specific issue ID without polling.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Simulate execution without modifying GitHub state or pushing commits.",
    )
    parser.add_argument(
        "--test-cmd",
        type=str,
        help=f"Custom test/verification command (default: '{DEFAULT_TEST_COMMAND}').",
    )
    parser.add_argument(
        "--model",
        type=str,
        default=os.environ.get("ANTIGRAVITY_MODEL", "gemini-3.8-flash"),
        help="Model to use for Google Antigravity (default: 'gemini-3.8-flash', with auto-cascading fallback).",
    )
    parser.add_argument(
        "--gemini-api-key",
        type=str,
        default=None,
        help="Gemini API Key for Google Antigravity (can also be set via GEMINI_API_KEY env var or .env).",
    )
    parser.add_argument(
        "--claude-cmd",
        type=str,
        default="claude",
        help="CLI executable for Claude Code (default: 'claude').",
    )
    parser.add_argument(
        "--agy-cmd",
        type=str,
        default="agy",
        help="CLI executable for Google Antigravity (default: 'agy').",
    )

    args = parser.parse_args()

    # Default to --once if no daemon/interactive/issue mode is specified
    if not (args.daemon or args.interactive or args.issue or args.once):
        args.once = True

    runner = AntigravityRunner(args)
    runner.run()


if __name__ == "__main__":
    main()
