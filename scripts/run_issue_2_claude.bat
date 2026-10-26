@echo off
cd /d "C:\Users\utente\Documents\Painter"
set "PATH=C:\Users\utente\AppData\Local\Programs\Python\Python312;C:\Users\utente\AppData\Roaming\npm;%PATH%"
if not exist ".antigravity\logs" mkdir ".antigravity\logs"
echo [%date% %time%] Starting scheduled execution of Issue #2 with Claude Code... >> .antigravity\logs\issue_2_scheduled.log
python scripts\antigravity_runner.py --issue 2 --engine claude >> .antigravity\logs\issue_2_scheduled.log 2>&1
echo [%date% %time%] Execution completed with exit code %errorlevel%. >> .antigravity\logs\issue_2_scheduled.log
