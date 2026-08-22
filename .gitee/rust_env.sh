# Gitee 云端任务共用：国内 rustup / crates.io 镜像 + stable toolchain。
# 由 .gitee-ci.yml 各 step `source`，不是给本机开发用的。
set -eu

export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://rsproxy.cn}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-https://rsproxy.cn/rustup}"

if command -v apt-get >/dev/null 2>&1; then
  if [ "$(id -u)" -eq 0 ]; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      curl ca-certificates libffi-dev pkg-config build-essential >/dev/null
  elif command -v sudo >/dev/null 2>&1; then
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      curl ca-certificates libffi-dev pkg-config build-essential >/dev/null
  fi
fi

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://rsproxy.cn/rustup-init.sh \
    | sh -s -- -y --default-toolchain stable --profile minimal
fi

# rustup-init 装到 $HOME/.cargo；部分镜像已有 rustc 但不在 PATH。
# shellcheck source=/dev/null
. "${HOME}/.cargo/env"

mkdir -p "${HOME}/.cargo"
if ! grep -q 'rsproxy-sparse' "${HOME}/.cargo/config.toml" 2>/dev/null; then
  cat >> "${HOME}/.cargo/config.toml" << 'EOF'
[source.crates-io]
replace-with = "rsproxy-sparse"
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
EOF
fi

rustc --version
cargo --version
