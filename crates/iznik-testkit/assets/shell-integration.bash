# A minimal bash shell integration for iznik's tests and scenarios: the four
# OSC 133 semantic-prompt marks and one OSC 7 working-directory report around
# every command, and a fixed prompt so the bytes are predictable. Sourced with
# `bash --rcfile`, so no test depends on the developer's shell configuration.

PS1='$ '

# One OSC command, terminated by BEL.
__iznik_osc() { printf '\033]%s\007' "$1"; }

# Before a command runs (preexec): OSC 133 C, once for the user's command and
# not for the prompt's own commands. The DEBUG trap fires for every simple
# command, so it is gated by a flag the precmd sets.
__iznik_preexec() {
    [[ -n "$COMP_LINE" ]] && return
    [[ "$BASH_COMMAND" == "$PROMPT_COMMAND" ]] && return
    if [[ -n "$__iznik_at_prompt" ]]; then
        __iznik_at_prompt=
        __iznik_osc '133;C'
    fi
}
trap '__iznik_preexec' DEBUG

# Before each prompt (precmd): OSC 133 D with the finished command's status,
# OSC 7 with the working directory, OSC 133 A for the new prompt, and arm the
# preexec for the next command.
__iznik_precmd() {
    local status=$?
    __iznik_osc "133;D;${status}"
    __iznik_osc "7;file://${HOSTNAME}${PWD}"
    __iznik_osc '133;A'
    __iznik_at_prompt=1
}
PROMPT_COMMAND='__iznik_precmd'

# OSC 133 B (command start) at the end of the prompt itself, non-printing.
PS1="${PS1}\[\033]133;B\007\]"
