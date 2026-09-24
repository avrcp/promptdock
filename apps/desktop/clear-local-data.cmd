@echo off
setlocal
set "RESET_SCRIPT=%~dp0clear-local-data.ps1"
if not exist "%RESET_SCRIPT%" set "RESET_SCRIPT=%~dp0scripts\clear-local-data.ps1"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%RESET_SCRIPT%" -Confirm %*
set "RESET_EXIT=%ERRORLEVEL%"
if "%~1"=="" pause
exit /b %RESET_EXIT%
