#!/usr/bin/env bash
# daemon 的啟動器，systemd（enil.service）與 launchd（launchd/*.plist）共用。
#
# 服務管理器給的 PATH 極簡，node 幾乎都不在裡面，而且每台機器裝法不同：
# Linux 這台是 nvm（設定寫在 bash 的 login profile），mac 那台是 mise（設定寫在
# .zshrc，bash -l 看不到）。服務檔用 `bash -l` 呼叫這支，先吃 login profile；
# 還找不到就試下面這些「不含版本號」的位置 —— 寫死含版本號的路徑會在升級 node 後壞掉。
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v npx >/dev/null; then
  for d in \
    "$HOME/.local/share/mise/shims" \
    "$HOME/.local/bin" \
    "$HOME/.volta/bin" \
    "$HOME/.asdf/shims" \
    /opt/homebrew/bin \
    /usr/local/bin; do
    [ -d "$d" ] && PATH="$PATH:$d"
  done
fi

if ! command -v npx >/dev/null; then
  # nvm 是 shell 函式，沒有 shim；載入時會切到 default alias 那個版本
  for f in "${NVM_DIR:-}/nvm.sh" "$HOME/.config/nvm/nvm.sh" "$HOME/.nvm/nvm.sh"; do
    # shellcheck disable=SC1090
    [ -s "$f" ] && { set +u; . "$f"; set -u; break; }
  done
fi

if ! command -v npx >/dev/null; then
  echo "找不到 node／npx。PATH=$PATH" >&2
  exit 1
fi

exec npx tsx daemon.ts
