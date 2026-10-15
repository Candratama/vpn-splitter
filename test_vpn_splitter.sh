#!/bin/bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/vpn-splitter.sh"

if [[ "${1:-}" != --case ]]; then
  failed=0
  for name in gateway no_gateway apply_repeat conflict rollback clear_missing clear_error dry_run dns_failure dns_local config_failure parser; do
    if bash "$0" --case "$name"; then echo "PASS $name"; else echo "FAIL $name"; failed=1; fi
  done
  exit "$failed"
fi

test_dir="$(mktemp -d)"
trap 'rm -rf "$test_dir"' EXIT
STATE_FILE="$test_dir/state"
CONFIG_FILE="$test_dir/config"
table="$test_dir/routes"
log="$test_dir/log"
printf '198.51.100.0/25\n198.51.100.128/25\n' > "$CONFIG_FILE"
: > "$table"
: > "$log"
fail_target=''
fail_delete=''
dns_interface=ppp0
dns_gateway=203.0.113.1
lookup_interface=ppp0
no_gateway=0

# Fixtures use documentation address ranges, never real network endpoints.
# Mock only OS boundaries; route mutations stay in a temporary table.
vpn_interface() { echo ppp0; }
networksetup() {
  case "$1" in
    -listnetworkserviceorder) printf '(1) Wi-Fi\n(Hardware Port: Wi-Fi, Device: en0)\n';;
    -getinfo) [[ "$no_gateway" == 0 ]] && printf 'IP address: 192.0.2.30\nRouter: 192.0.2.1\n';;
    *) return 1;;
  esac
}
ifconfig() { printf 'en0: flags=8863<UP,BROADCAST,RUNNING>\n\tinet 192.0.2.30 netmask 0xffffff00\n'; }
netstat() {
  printf 'Destination Gateway Flags Netif Expire\ndefault link#11 UCSg en0\n203.0.113.195 192.0.2.1 UGHS en0\n203.0.113.0/24 192.0.2.254 UGSc en0\n'
}
scutil() { printf 'resolver #1\n  nameserver[0] : 203.0.113.53\n  if_index : 12 (ppp0)\n'; }
dscacheutil() { return 1; }
sudo() {
  [[ "$1" == route ]] || { echo "Unexpected sudo: $*" >&2; return 1; }
  shift
  route "$@"
}
route() {
  [[ "$1" == -n ]] && shift
  if [[ "$1" == get ]]; then
    if [[ "$2" == 203.0.113.53 ]]; then
      printf '   route to: %s\ndestination: default\n    gateway: %s\n  interface: %s\n' "$2" "$dns_gateway" "$dns_interface"
    else
      printf '   route to: %s\ndestination: 198.51.100.0\n  interface: %s\n' "$2" "$lookup_interface"
    fi
    return
  fi
  printf '%s\n' "$*" >> "$log"
  local operation="$1" kind="$2" target="$3" row
  row="$kind $target"
  case "$operation" in
    add)
      [[ "$target" != "$fail_target" ]] || { echo 'route: Network is unreachable' >&2; return 1; }
      if grep -Fxq -- "$row" "$table"; then echo 'route: File exists' >&2; return 1; fi
      printf '%s\n' "$row" >> "$table";;
    delete)
      [[ "$target" != "$fail_delete" ]] || { echo 'route: Permission denied' >&2; return 1; }
      grep -Fxq -- "$row" "$table" || { echo 'route: not in table' >&2; return 1; }
      awk -v row="$row" '$0 != row' "$table" > "$table.next"
      mv "$table.next" "$table";;
    *) echo "Unexpected route: $*" >&2; return 1;;
  esac
}

case "$2" in
  gateway)
    [[ "$(direct_route)" == '192.0.2.1 en0' ]];;
  no_gateway)
    no_gateway=1
    if apply_routes; then echo 'Expected missing gateway failure' >&2; exit 1; fi
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  apply_repeat)
    printf '%s\n' '-net 198.51.100.0/25' > "$table"
    apply_routes
    before="$(cat "$table")"
    apply_routes
    [[ "$(cat "$table")" == "$before" ]]
    grep -Fq 'add -host 203.0.113.53 -interface ppp0' "$log"
    ! grep -q 'change default' "$log"
    clear_routes
    [[ "$(cat "$table")" == '-net 198.51.100.0/25' ]]
    [[ ! -e "$STATE_FILE" ]];;
  conflict)
    printf '%s\n' '-net 198.51.100.0/25' > "$table"
    lookup_interface=en0
    if apply_routes; then echo 'Expected route conflict' >&2; exit 1; fi
    [[ "$(cat "$table")" == '-net 198.51.100.0/25' && ! -e "$STATE_FILE" ]];;
  rollback)
    fail_target=128.0.0.0/1
    if apply_routes; then echo 'Expected apply failure' >&2; exit 1; fi
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  clear_missing)
    apply_routes
    sed '/198.51.100.0/d' "$table" > "$table.next"
    mv "$table.next" "$table"
    printf 'different.example\n' > "$CONFIG_FILE"
    clear_routes
    clear_routes
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  clear_error)
    apply_routes
    fail_delete=198.51.100.0/25
    if clear_routes; then echo 'Expected cleanup failure' >&2; exit 1; fi
    [[ "$(cat "$table")" == '-net 198.51.100.0/25' && -e "$STATE_FILE" ]]
    fail_delete=''
    clear_routes
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  dry_run)
    DRY_RUN=1 apply_routes
    [[ ! -s "$table" && ! -e "$STATE_FILE" && ! -s "$log" ]]
    apply_routes
    before="$(cat "$table")"
    DRY_RUN=1 clear_routes
    [[ "$(cat "$table")" == "$before" && -e "$STATE_FILE" ]];;
  dns_failure)
    route() { return 1; }
    if apply_routes; then echo 'Expected DNS preflight failure' >&2; exit 1; fi
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  dns_local)
    dns_interface=en0
    dns_gateway='link#11'
    apply_routes
    grep -Fq 'add -host 203.0.113.53 -interface en0' "$log"
    clear_routes
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  config_failure)
    printf 'missing.example\n' > "$CONFIG_FILE"
    if apply_routes; then echo 'Expected unresolved hostname failure' >&2; exit 1; fi
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
  parser)
    printf '# office\n198.51.100.0/25 # comment\ngit.office.example\n' > "$CONFIG_FILE"
    [[ "$(read_config "$CONFIG_FILE")" == $'198.51.100.0/25\ngit.office.example' ]]
    printf 'https://invalid.example/path\n' > "$CONFIG_FILE"
    if apply_routes; then echo 'Expected invalid config failure' >&2; exit 1; fi
    [[ ! -s "$table" && ! -e "$STATE_FILE" ]];;
esac
