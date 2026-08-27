#!/usr/bin/env bash
# Two append ops, spaced so each flash gets its own screen window:
# first a type outside the whitelist (tool_result), then one inside
# it (ext_status). Then the script stays alive so the smoke test can
# prove quit leaves no orphan process.
printf '{"v":1,"op":"append","event":{"v":1,"type":"tool_result","ts":"t","id":"x","value":{}}}\n'
sleep 2
printf '{"v":1,"op":"append","event":{"v":1,"type":"ext_status","ts":"t","id":"smoke","value":"ok"}}\n'
sleep 30
