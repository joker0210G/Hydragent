@echo off
REM ======================================================================
REM  Hydragent v0.5.0 — Phase 5 Non-Technical User Demo
REM ======================================================================
REM  This script walks through the user-facing surfaces of Phase 5 as a
REM  non-technical operator would experience them. There is no code, no
REM  compiler, no JSON editing — just run it and read the output.
REM
REM  Usage:
REM     tests\demo_phase5.bat
REM
REM  Press any key when prompted to advance to the next section.
REM ======================================================================

setlocal

set "REPO=%~dp0.."
pushd "%REPO%"
set "EXE=%REPO%\target\debug\swarm_status.exe"
set "SPEC=%REPO%\tests\phase5_demo_sample.json"
set "REPORT=%REPO%\tests\phase5_demo_report.json"

cls
echo.
echo ======================================================================
echo   HYDRAGENT v0.5.0 -- PHASE 5 USER DEMO
echo ======================================================================
echo.
echo   This script shows what the new Phase 5 surfaces look like to
echo   a non-technical operator. There are 4 sections:
echo.
echo     1. Help and discoverability (--help)
echo     2. Inspect a DAG before running it
echo     3. Inspect an ExecutionReport after a run
echo     4. One-line mode for log shipping
echo.
echo   Each section pauses for a keypress so you can read the output.
echo.
pause

REM ----------------------------------------------------------------------
echo.
echo ======================================================================
echo   Section 1 -- Help and discoverability
echo ======================================================================
echo.
echo   $ swarm_status --help
echo.
"%EXE%" --help
echo.
pause

REM ----------------------------------------------------------------------
echo.
echo ======================================================================
echo   Section 2 -- Inspect a DAG before running it
echo ======================================================================
echo.
echo   The file tests\phase5_demo_sample.json describes a 6-node research
echo   workflow: 1 plan step, 3 parallel research steps, 1 compare step,
echo   1 report step. Let's see what it looks like:
echo.
echo   $ swarm_status --from-spec tests\phase5_demo_sample.json
echo.
"%EXE%" --from-spec "%SPEC%"
echo.
pause

REM ----------------------------------------------------------------------
echo.
echo ======================================================================
echo   Section 3 -- Inspect an ExecutionReport after a run
echo ======================================================================
echo.
echo   The file tests\phase5_demo_report.json is what you'd see after
echo   the swarm actually ran. Notice: framework_b succeeded after one
echo   retry, framework_c failed terminally, and the downstream nodes
echo   were skipped because they depended on framework_c.
echo.
echo   $ swarm_status --from-report tests\phase5_demo_report.json
echo.
"%EXE%" --from-report "%REPORT%"
echo.
pause

REM ----------------------------------------------------------------------
echo.
echo ======================================================================
echo   Section 4 -- One-line mode for log shipping
echo ======================================================================
echo.
echo   For log aggregators (ELK, Datadog, Loki) you usually want a single
echo   line per run. Add --one-line to any of the above commands.
echo.
echo   $ swarm_status --one-line --from-report tests\phase5_demo_report.json
echo.
"%EXE%" --one-line --from-report "%REPORT%"
echo.
echo   (this is what your log shipper would index line-by-line)
echo.

REM ----------------------------------------------------------------------
echo.
echo ======================================================================
echo   Bonus -- Try it yourself
echo ======================================================================
echo.
echo   a) Drop the --from-report flag and pass stdin instead:
echo        type tests\phase5_demo_report.json ^ | swarm_status --stdin-report
echo.
echo   b) Suppress the header (handy in dashboards):
echo        swarm_status --no-header --from-spec tests\phase5_demo_sample.json
echo.
echo   c) All in one line for your CI/CD log:
echo        swarm_status --one-line --no-header --from-report tests\phase5_demo_report.json
echo.
echo   d) Help text is the same as the agent's help. Try:
echo        swarm_status --help
echo.
popd
endlocal
