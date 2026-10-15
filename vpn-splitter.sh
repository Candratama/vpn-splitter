#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_FILE="${VPN_SPLITTER_CONFIG:-$SCRIPT_DIR/config.txt}"
STATE_FILE="${VPN_SPLITTER_STATE:-${TMPDIR:-/tmp}/vpn-splitter-${UID}.state}"

read_config() {
  local file="${1:?config file required}"
  [[ -r "$file" ]] || { echo "Config tidak bisa dibaca: $file" >&2; return 1; }
  sed -E 's/[[:space:]]*#.*$//; /^[[:space:]]*$/d; s/^[[:space:]]+//; s/[[:space:]]+$//' "$file" |
    while IFS= read -r entry; do
      if [[ "$entry" =~ ^[0-9]+(\.[0-9]+){3}(/[0-9]{1,2})?$ || "$entry" =~ ^[A-Za-z0-9.-]+$ ]]; then
        printf '%s\n' "$entry"
      else
        echo "Entry config tidak valid: $entry" >&2
        return 1
      fi
    done
}

vpn_interface() {
  if [[ -n "${VPN_SPLITTER_INTERFACE:-}" ]]; then
    printf '%s\n' "$VPN_SPLITTER_INTERFACE"
    return
  fi
  local iface
  for iface in ppp0 ppp1; do
    if ifconfig "$iface" 2>/dev/null | grep -q 'inet '; then
      printf '%s\n' "$iface"
      return
    fi
  done
  echo "Tidak menemukan VPN L2TP aktif." >&2
  return 1
}

direct_route() {
  local services iface service info gw
  services="$(networksetup -listnetworkserviceorder)" || return 1
  while IFS='|' read -r iface service; do
    [[ -n "$iface" ]] || continue
    if ! ifconfig "$iface" 2>/dev/null | grep -q 'inet '; then continue; fi
    info="$(networksetup -getinfo "$service")" || continue
    gw="$(awk '$1 == "Router:" && $2 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ {print $2; exit}' <<< "$info")"
    if [[ -n "$gw" && "$gw" != 0.0.0.0 ]]; then
      printf '%s %s\n' "$gw" "$iface"
      return
    fi
  done < <(awk '
    /^\([0-9]+\) / {service=$0; sub(/^\([0-9]+\) /, "", service)}
    /Device: / {
      iface=$0; sub(/^.*Device: /, "", iface); sub(/\).*$/, "", iface)
      if (iface != "" && iface !~ /^(ppp|utun)/) print iface "|" service
    }
  ' <<< "$services")
  echo 'Gateway Wi-Fi/LAN tidak ditemukan dari konfigurasi network service.' >&2
  return 1
}

resolve_entry() {
  local entry="$1"
  if [[ "$entry" =~ ^[0-9]+(\.[0-9]+){3}(/[0-9]{1,2})?$ ]]; then
    printf '%s\n' "$entry"
  else
    dscacheutil -q host -a name "$entry" | awk '/ip_address:/ && $2 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ {print $2}'
  fi
}

route_cmd() {
  if [[ "${DRY_RUN:-0}" == 1 ]]; then
    printf 'sudo route -n %s\n' "$*"
  else
    sudo route -n "$@"
  fi
}

build_plan() {
  local vpn_if="$1" direct="$2" gw="$3" entries entry addresses ip kind dns route_info iface gateway
  entries="$(read_config "$CONFIG_FILE")" || return 1
  # Preserve each DNS server's current path before internet routes change.
  dns="$(scutil --dns | awk '/nameserver\[/ && $3 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ {print $3}' | sort -u)" || return 1
  [[ -n "$dns" ]] || { echo 'Tidak menemukan DNS IPv4 aktif.' >&2; return 1; }
  while IFS= read -r ip; do
    route_info="$(route -n get "$ip")" || return 1
    iface="$(awk '$1 == "interface:" {print $2}' <<< "$route_info")"
    gateway="$(awk '$1 == "gateway:" {print $2}' <<< "$route_info")"
    [[ -n "$iface" ]] || return 1
    case "$iface" in
      ppp*|utun*) gateway='';;
      *)
        if [[ ! "$gateway" =~ ^[0-9]+(\.[0-9]+){3}$ ]]; then
          if [[ "$route_info" == *GATEWAY* ]]; then
            echo "Gateway DNS $ip tidak valid." >&2
            return 1
          fi
          gateway=''
        fi;;
    esac
    printf 'host|%s|%s|%s\n' "$ip" "$iface" "$gateway"
  done <<< "$dns"
  while IFS= read -r entry; do
    [[ -n "$entry" ]] || continue
    addresses="$(resolve_entry "$entry")" || return 1
    [[ -n "$addresses" ]] || { echo "Hostname tidak bisa di-resolve: $entry" >&2; return 1; }
    while IFS= read -r ip; do
      kind=host
      [[ "$ip" == */* ]] && kind=net
      printf '%s|%s|%s|\n' "$kind" "$ip" "$vpn_if"
    done <<< "$addresses"
  done <<< "$entries"
  printf 'net|0.0.0.0/1|%s|%s\nnet|128.0.0.0/1|%s|%s\n' "$direct" "$gw" "$direct" "$gw"
}

add_planned_route() {
  local kind="$1" ip="$2" iface="$3" gateway="$4" output current
  local args=(add "-$kind" "$ip")
  if [[ -n "$gateway" ]]; then args+=("$gateway" -ifp "$iface"); else args+=(-interface "$iface"); fi
  if output="$(route_cmd "${args[@]}" 2>&1)"; then
    if [[ "${DRY_RUN:-0}" == 1 ]]; then
      printf '%s\n' "$output"
    else
      printf '%s|%s|%s|%s\n' "$kind" "$ip" "$iface" "$gateway" >> "$STATE_FILE/routes" || return 1
    fi
    return
  fi
  # Do not own/delete a pre-existing route, and do not accept a conflicting one.
  if [[ "$output" == *'File exists'* ]]; then
    current="$(route -n get "$ip")" || return 1
    if [[ "$(awk '$1 == "interface:" {print $2}' <<< "$current")" == "$iface" ]] &&
       { [[ -z "$gateway" ]] || [[ "$(awk '$1 == "gateway:" {print $2}' <<< "$current")" == "$gateway" ]]; }; then
      return 0
    fi
  fi
  printf '%s\n' "$output" >&2
  return 1
}

valid_state() {
  [[ -d "$STATE_FILE" && ! -L "$STATE_FILE" && -O "$STATE_FILE" && -f "$STATE_FILE/routes" ]]
}

apply_routes() (
  local vpn_if direct gw plan kind ip iface gateway physical
  vpn_if="$(vpn_interface)" || return 1
  if [[ -e "$STATE_FILE" || -L "$STATE_FILE" ]]; then
    if valid_state && [[ -f "$STATE_FILE/active" && "$(cat "$STATE_FILE/vpn")" == "$vpn_if" ]]; then
      echo 'Split routing sudah aktif. Untuk refresh config, jalankan clear lalu apply.'
      return 0
    fi
    echo "State sebelumnya belum bersih atau format lama: $STATE_FILE. Jalankan clear terlebih dahulu." >&2
    return 1
  fi
  physical="$(direct_route)" || return 1
  read -r gw direct <<< "$physical"
  [[ -n "$gw" && -n "$direct" ]] || return 1
  plan="$(build_plan "$vpn_if" "$direct" "$gw")" || return 1
  if [[ "${DRY_RUN:-0}" != 1 ]]; then
    umask 077
    mkdir "$STATE_FILE" || return 1
    : > "$STATE_FILE/routes" || { rmdir "$STATE_FILE"; return 1; }
    trap 'rc=$?; if [[ "$rc" != 0 ]]; then echo "Apply gagal; membatalkan perubahan." >&2; clear_routes || echo "Cleanup belum selesai; jalankan clear lagi." >&2; fi' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    printf '%s\n' "$vpn_if" > "$STATE_FILE/vpn" || return 1
  fi
  while IFS='|' read -r kind ip iface gateway; do
    add_planned_route "$kind" "$ip" "$iface" "$gateway" || return 1
  done <<< "$plan"
  if [[ "${DRY_RUN:-0}" != 1 ]]; then : > "$STATE_FILE/active" || return 1; fi
  if [[ "${DRY_RUN:-0}" == 1 ]]; then
    echo "Dry run selesai; belum ada route yang diubah."
  else
    echo "Split routing aktif via $vpn_if; internet umum via $direct ($gw)."
  fi
)

clear_routes() {
  local kind ip iface gateway output failed=0
  if [[ ! -e "$STATE_FILE" && ! -L "$STATE_FILE" ]]; then echo 'Tidak ada state splitter untuk dibersihkan.'; return 0; fi
  valid_state || { echo "State tidak valid: $STATE_FILE; tidak ada route yang diubah." >&2; return 1; }
  if [[ "${DRY_RUN:-0}" != 1 ]]; then
    : > "$STATE_FILE/remaining" || return 1
    rm -f "$STATE_FILE/active" || return 1
  fi
  while IFS='|' read -r kind ip iface gateway; do
    [[ -n "$ip" ]] || continue
    if output="$(route_cmd delete "-$kind" "$ip" 2>&1)"; then
      [[ "${DRY_RUN:-0}" != 1 ]] || printf '%s\n' "$output"
    elif [[ "$output" != *'not in table'* && "$output" != *'No such process'* ]]; then
      printf '%s\n' "$output" >&2
      if [[ "${DRY_RUN:-0}" != 1 ]]; then printf '%s|%s|%s|%s\n' "$kind" "$ip" "$iface" "$gateway" >> "$STATE_FILE/remaining" || return 1; fi
      failed=1
    fi
  done < "$STATE_FILE/routes"
  if [[ "${DRY_RUN:-0}" != 1 ]]; then
    mv "$STATE_FILE/remaining" "$STATE_FILE/routes" || return 1
    if [[ "$failed" == 0 ]]; then
      rm -f "$STATE_FILE/routes" "$STATE_FILE/vpn" "$STATE_FILE/active"
      rmdir "$STATE_FILE" || return 1
    else
      rm -f "$STATE_FILE/active"
    fi
  fi
  [[ "$failed" == 0 ]] || return 1
  if [[ "${DRY_RUN:-0}" == 1 ]]; then
    echo 'Dry run selesai; belum ada route yang dihapus.'
  else
    echo 'Route milik splitter dihapus; default route dan konfigurasi DNS tetap dipertahankan.'
  fi
}

status() {
  scutil --nc list 2>/dev/null || true
  netstat -rn -f inet | awk '$1 == "default" || $1 == "0/1" || $1 == "128.0/1" || $4 ~ /^ppp[0-9]+$/ {print}'
  [[ ! -e "$STATE_FILE" ]] || printf 'State splitter: %s\n' "$STATE_FILE"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  case "${1:-}" in
    apply) apply_routes ;;
    clear) clear_routes ;;
    status) status ;;
    *) echo "Usage: $0 {apply|clear|status}" >&2; exit 2 ;;
  esac
fi
