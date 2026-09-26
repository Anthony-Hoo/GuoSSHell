#!/usr/bin/env bash
# 本地验收 SSH 服务器（Docker，镜像定义见 scripts/sshd-test/Dockerfile）。
# 登录 probe / probe，只监听 127.0.0.1（iOS 模拟器与 "Designed for iPad" 可直接访问）。
#
#   ./scripts/sshd-test.sh up       构建镜像并启动容器（幂等；镜像变了就换新容器）
#   ./scripts/sshd-test.sh rekey    重新生成主机密钥（验收「主机密钥变更」告警）
#   ./scripts/sshd-test.sh authorize <公钥文件>   追加到 probe 的 authorized_keys
#   ./scripts/sshd-test.sh down     停止并删除容器
#
# 端口默认 2223，可用 GUOSH_SSHD_PORT 覆盖。M6 的假 AI 上游（请求日志、按键回显记录）
# 映射到 127.0.0.1:2224，可用 GUOSH_M6_PORT 覆盖。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAME="guosh-sshd"
IMAGE="guosh-sshd:dev"
PORT="${GUOSH_SSHD_PORT:-2223}"
M6_PORT="${GUOSH_M6_PORT:-2224}"

usage() {
  sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

case "${1:-up}" in
  up)
    docker build -t "${IMAGE}" -f "${HERE}/sshd-test/Dockerfile" "${HERE}"
    if docker ps -a --format '{{.Names}}' | grep -qx "${NAME}"; then
      # 容器还是旧镜像建的：换成新的（主机密钥随之重新生成）。
      if [ "$(docker inspect -f '{{.Image}}' "${NAME}")" != "$(docker image inspect -f '{{.Id}}' "${IMAGE}")" ]; then
        docker rm -f "${NAME}" >/dev/null
      fi
    fi
    if docker ps -a --format '{{.Names}}' | grep -qx "${NAME}"; then
      docker start "${NAME}" >/dev/null
    else
      docker run -d --name "${NAME}" \
        -p "127.0.0.1:${PORT}:22" -p "127.0.0.1:${M6_PORT}:4010" "${IMAGE}" >/dev/null
    fi
    echo "ssh -p ${PORT} probe@127.0.0.1   (password: probe)"
    echo "M6 假上游：http://127.0.0.1:${M6_PORT}/__aimock/journal"
    ;;
  rekey)
    docker exec "${NAME}" sh -c 'rm -f /etc/ssh/ssh_host_* && ssh-keygen -A >/dev/null && kill -HUP 1'
    echo "host keys regenerated"
    ;;
  authorize)
    [ -n "${2:-}" ] || { usage; exit 2; }
    docker exec -i "${NAME}" sh -c \
      'cat >> /home/probe/.ssh/authorized_keys && chown probe:probe /home/probe/.ssh/authorized_keys && chmod 600 /home/probe/.ssh/authorized_keys' \
      < "$2"
    echo "authorized: $2"
    ;;
  down)
    docker rm -f "${NAME}" >/dev/null 2>&1 || true
    ;;
  *)
    usage
    exit 2
    ;;
esac
