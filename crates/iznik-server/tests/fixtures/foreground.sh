#!/bin/sh
# A foreground job announces its actual PID only after the interactive parent
# has assigned its job-control group. Ignoring hangup makes missing cleanup
# deterministic; the owner must explicitly terminate this foreground group.
trap '' HUP
printf '\033]0;iznik-foreground:%s\007\001%s\001' "$$" "$$"
exec sleep "${1:?sleep duration is required}"
