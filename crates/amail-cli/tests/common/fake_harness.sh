#!/bin/sh
# A stand-in for Claude Code and Codex that speaks the hook protocol through
# `amail hook`, for the supervisor tests. Installed as `<dir>/claude` and
# `<dir>/codex`; the name says which harness it plays. No model, no TUI.
#
# Files in $D (the directory of this script):
#   <seat>.launches  one line per start: "<unix ms> <pid> <args...>"
#   <seat>.mode      optional behaviour: trust | exit3 | badresume | slow | noturn |
#                    deaf (ignores native wake-ups; its screen shows an empty
#                    input box) | deafdialog (the same, with a dialog open)
#   <seat>.busy      while it exists, a turn keeps making tool calls
#   <seat>.prompt    created by the test: start a turn (like a person typing)
#   <seat>.answer    answers the trust prompt (mode trust)
#   <seat>.wake      created by the test when the node wakes the seat
#                    (Claude: via the inbox socket; Codex: `codex queue`)
#   <seat>.injected  every hook answer the node gave
#   <seat>.started   "<unix ms> <pid>" when a Claude session sent SessionStart
#   sock/<seat>.sock the seat's inbox socket (served by the test)
#   thread-<id>.seat the seat of a Codex thread (for `codex queue`)
D=$(cd "$(dirname "$0")" && pwd)
kind=$(basename "$0")
now() { date +%s%3N; }

if [ "$kind" = codex ]; then
    case "$1" in
    queue)
        # codex queue --thread <id> --message <text>
        echo "$*" >> "$D/codex.log"
        t=$3
        seat=$(cat "$D/thread-$t.seat" 2>/dev/null) || exit 0
        : > "$D/$seat.wake"
        exit 0
        ;;
    app-server | login | exec)
        echo "$*" >> "$D/codex.log"
        exit 1
        ;;
    esac
fi

AMAIL=${FAKE_AMAIL:?}
seat=${AMAIL_SEAT:?}
printf '%s %s %s\n' "$(now)" "$$" "$(printf '%s' "$*" | tr '\n' ' ')" >> "$D/$seat.launches"
mode=$(cat "$D/$seat.mode" 2>/dev/null)

# Arguments: the session to resume or the id to use, and the last word.
sid=""
resumed=""
last=""
[ "$kind" = codex ] && [ "$1" = resume ] && resumed=1
for a in "$@"; do last=$a; done
while [ $# -gt 0 ]; do
    case "$1" in
    --resume) sid=$2; resumed=1; shift ;;
    --session-id) sid=$2; shift ;;
    esac
    shift
done
if [ "$kind" = codex ] && [ -n "$resumed" ]; then sid=$last; fi

case "$mode" in
trust)
    # Wait at the prompt until the test answers (<seat>.answer), then clear
    # the screen and start up a moment later.
    echo "Do you trust the files in this folder?"
    echo "  1. Yes, proceed"
    echo "  2. No, exit"
    while [ ! -f "$D/$seat.answer" ]; do sleep 0.1; done
    rm -f "$D/$seat.answer"
    printf '\033[2J'
    echo "trusted"
    sleep 1.5
    ;;
exit3)
    echo "crashing on purpose"
    exit 3
    ;;
badresume)
    if [ -n "$resumed" ]; then
        echo "No conversation found with session ID: $sid"
        exit 1
    fi
    ;;
slow)
    sleep 1.5
    ;;
esac

hook() {
    # hook <json>: run the hook, keep its answer.
    if [ "$kind" = claude ]; then
        out=$(printf '%s' "$1" | CLAUDE_CODE_MESSAGING_SOCKET="$D/sock/$seat.sock" CLAUDE_PID=$$ \
            "$AMAIL" hook claude)
    else
        out=$(printf '%s' "$1" | env -u AMAIL_SEAT "$AMAIL" hook codex)
    fi
    [ -n "$out" ] && printf '%s\n' "$out" >> "$D/$seat.injected"
    printf '%s' "$out"
}

turn() {
    # A turn: the prompt hook, tool calls while <seat>.busy exists, then
    # Stop (again while it blocks).
    hook "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"$sid\",\"prompt\":$1}" > /dev/null
    while [ -f "$D/$seat.busy" ]; do
        hook "{\"hook_event_name\":\"PostToolUse\",\"session_id\":\"$sid\",\"tool_name\":\"Bash\"}" > /dev/null
        sleep 0.2
    done
    active=false
    n=0
    while [ $n -lt 5 ]; do
        r=$(hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$sid\",\"stop_hook_active\":$active}")
        case "$r" in *block*) active=true; n=$((n + 1)) ;; *) break ;; esac
    done
}

if [ "$kind" = claude ]; then
    [ -n "$sid" ] || sid="claude-$$"
    src=startup
    [ -n "$resumed" ] && src=resume
    echo "$(now) $$" >> "$D/$seat.started"
    hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$sid\",\"source\":\"$src\"}" > /dev/null
    # A first turn, so the session can be resumed later (Claude Code
    # cannot resume a session that never had one), unless mode `noturn`.
    [ "$mode" = noturn ] || turn "\"first prompt\""
else
    if [ -z "$resumed" ]; then
        sid="thread-$$-$(now)"
        marker=$(printf '%s' "$last" | grep -o '\[amail:seat=[^]]*\]' | head -n 1)
        echo "$seat" > "$D/thread-$sid.seat"
        turn "\"$marker\""
    else
        echo "$seat" > "$D/thread-$sid.seat"
    fi
fi

# A screen like a harness's idle TUI, for the paste fallback.
screen() {
    printf '\033[2J'
    echo "● Done."
    case "$mode" in
    deafdialog)
        echo "Do you want to proceed?"
        echo "❯ 1. Yes"
        echo "  2. No"
        ;;
    *)
        echo "────────────────────"
        echo "> "
        echo "────────────────────"
        ;;
    esac
}
screen

trap 'exit 143' TERM
while :; do
    if [ -f "$D/$seat.wake" ]; then
        rm -f "$D/$seat.wake"
        case "$mode" in deaf*) continue ;; esac
        if [ "$kind" = codex ] && [ -n "$resumed" ]; then
            hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$sid\",\"source\":\"resume\"}" > /dev/null
            resumed=""
        fi
        turn "\"[amail] You have new mail.\""
    fi
    if [ -f "$D/$seat.prompt" ]; then
        rm -f "$D/$seat.prompt"
        turn "\"a prompt\""
        screen
    fi
    if [ -f "$D/$seat.ask" ]; then
        rm -f "$D/$seat.ask"
        hook "{\"hook_event_name\":\"Notification\",\"session_id\":\"$sid\",\"notification_type\":\"permission_prompt\",\"message\":\"needs approval\"}" > /dev/null
        hook "{\"hook_event_name\":\"PermissionRequest\",\"session_id\":\"$sid\",\"tool_name\":\"Bash\"}" > /dev/null
    fi
    sleep 0.05
done
