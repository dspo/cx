#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${CX_GITLAB_BASE_URL:-https://git.huayi.tech}"
PROJECT_PATH="${CX_GITLAB_PROJECT_PATH:-awesome/cx}"
INSTALL_DIR="${CX_INSTALL_DIR:-$HOME/.local/bin}"
TARGET_BIN="${INSTALL_DIR}/cx"
TOKEN="${CX_GITLAB_TOKEN:-${GITLAB_TOKEN:-}}"

if [[ -z "$TOKEN" ]]; then
  echo "请先设置 CX_GITLAB_TOKEN 或 GITLAB_TOKEN，用于下载私有 GitLab Release 产物。" >&2
  exit 1
fi

detect_platform() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux) os="linux" ;;
    Darwin) os="darwin" ;;
    *)
      echo "暂不支持的操作系统: $os" >&2
      exit 1
      ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="arm64" ;;
    *)
      echo "暂不支持的 CPU 架构: $arch" >&2
      exit 1
      ;;
  esac

  printf 'cx-%s-%s' "$os" "$arch"
}

asset_name="$(detect_platform)"
release_base="${BASE_URL}/${PROJECT_PATH}/-/releases/permalink/latest/downloads"
binary_url="${release_base}/binaries/${asset_name}"
checksum_url="${release_base}/checksums/SHA256SUMS"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

binary_path="${tmp_dir}/${asset_name}"
checksum_path="${tmp_dir}/SHA256SUMS"

curl --fail --show-error --silent --location \
  --header "PRIVATE-TOKEN: ${TOKEN}" \
  --output "$binary_path" \
  "$binary_url"

curl --fail --show-error --silent --location \
  --header "PRIVATE-TOKEN: ${TOKEN}" \
  --output "$checksum_path" \
  "$checksum_url"

expected_sum="$(grep " ${asset_name}$" "$checksum_path" | awk '{print $1}')"
if [[ -z "$expected_sum" ]]; then
  echo "未在 SHA256SUMS 中找到 ${asset_name} 的校验值。" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual_sum="$(sha256sum "$binary_path" | awk '{print $1}')"
else
  actual_sum="$(shasum -a 256 "$binary_path" | awk '{print $1}')"
fi

if [[ "$expected_sum" != "$actual_sum" ]]; then
  echo "校验失败: 期望 ${expected_sum}，实际 ${actual_sum}" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
install "$binary_path" "$TARGET_BIN"

echo "已安装到: $TARGET_BIN"
