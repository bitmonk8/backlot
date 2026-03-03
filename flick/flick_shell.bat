@echo off
title Flick shell

rem -- Configure environment --
set PATH=%~dp0target\debug;%PATH%

rem -- Keep shell open --
cmd /k echo Ready.
