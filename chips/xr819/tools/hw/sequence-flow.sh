# shellcheck shell=bash
# Source through lib.sh. Both endpoints' Python tools are staged independently
# of the test link. The sender's actual count returns through SSH/stdout, then
# reaches the receiver through a file, never through UDP on the link under test.

# sequence_flow up|down HOST_ADDR BOARD_ADDR PORT MBIT SIZE SECONDS [NETNS]
# up: board -> host; down: host -> board. NETNS is the board's network namespace.
# Nonzero means an invalid/incomplete measurement, not ordinary packet loss.
sequence_flow() (
  local direction=$1 host=$2 address=$3 port=$4 rate=$5 size=$6 seconds=$7
  local namespace=${8:-} hello=$(($4 + 1))
  local tools work remote= recv_pid= receiver_remote= prefix= rc=0
  tools=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  work=$(mktemp -d) || return 1
  cleanup_flow() {
    if [ -n "$recv_pid" ]; then
      if [ -n "$receiver_remote" ]; then
        board "test ! -f $remote/receiver.pid || kill \$(cat $remote/receiver.pid) 2>/dev/null" 10 >/dev/null 2>&1 || true
      fi
      kill "$recv_pid" 2>/dev/null || true
      wait "$recv_pid" 2>/dev/null || true
    fi
    [ -z "$remote" ] || board "rm -rf $remote" 10 >/dev/null 2>&1 || true
    rm -rf "$work"
  }
  trap cleanup_flow EXIT
  case $direction in up|down) ;; *) echo "FLOW_ERROR invalid direction: $direction"; return 1 ;; esac
  remote=$(board 'mktemp -d /run/xr819-flow.XXXXXX' 10) || return 1
  board_put "$tools/seq-send.py" "$remote/seq-send.py" || return 1
  board_put "$tools/seq-recv.py" "$remote/seq-recv.py" || return 1
  [ -z "$namespace" ] || prefix="ip netns exec $namespace"

  if [ "$direction" = down ]; then
    receiver_remote=1
    board "echo \$\$ > $remote/receiver.pid; exec $prefix python3 $remote/seq-recv.py \
      $address $host $port $((seconds + 30)) $hello --sent-file $remote/sender.txt" \
      $((seconds + 45)) >"$work/receiver.txt" 2>&1 &
  else
    python3 "$tools/seq-recv.py" "$host" "$address" "$port" \
      "$((seconds + 30))" "$hello" --sent-file "$work/sender.txt" \
      >"$work/receiver.txt" 2>&1 &
  fi
  recv_pid=$!

  if [ "$direction" = down ]; then
    timeout "$((seconds + 20))" python3 "$tools/seq-send.py" "$address" "$port" \
      "$rate" "$size" "$seconds" "$hello" >"$work/sender.tmp" 2>&1 || rc=$?
  else
    board "exec $prefix python3 $remote/seq-send.py $host $port $rate $size $seconds $hello" \
      $((seconds + 20)) >"$work/sender.tmp" 2>&1 || rc=$?
  fi
  cat "$work/sender.tmp"
  if [ "$rc" -ne 0 ]; then
    echo "FLOW_ERROR sender exited rc=$rc"
    return 1
  fi
  # Atomic publication: the receiver must not read a half-copied report.
  if [ "$direction" = down ]; then
    board_put "$work/sender.tmp" "$remote/sender.tmp" \
      && board "mv $remote/sender.tmp $remote/sender.txt" 10 || return 1
  else
    mv "$work/sender.tmp" "$work/sender.txt" || return 1
  fi
  wait "$recv_pid" || rc=$?
  recv_pid=
  cat "$work/receiver.txt"
  [ "$rc" -eq 0 ] || { echo "FLOW_ERROR receiver exited rc=$rc"; return 1; }
)
