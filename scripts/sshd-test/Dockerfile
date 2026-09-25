# GuoSSHell 本地验收用的 SSH 服务器：vim / htop / tmux（粘贴、鼠标、主机密钥、私钥认证等验收）。
# 由 scripts/sshd-test.sh 构建与启动，构建上下文是 scripts/。
FROM debian:bookworm-slim

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      openssh-server vim-nox htop tmux less locales python3 ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN sed -i 's/^# *\(en_US.UTF-8\)/\1/; s/^# *\(zh_CN.UTF-8\)/\1/' /etc/locale.gen \
 && locale-gen
ENV LANG=en_US.UTF-8

# 登录 probe / probe（与 demo_server 一致）。
RUN useradd -m -s /bin/bash probe \
 && echo 'probe:probe' | chpasswd \
 && mkdir -p /run/sshd /home/probe/.ssh \
 && chmod 700 /home/probe/.ssh \
 && chown -R probe:probe /home/probe/.ssh

# M1 帧率自检脚本：连上后执行 `m1bar`。
COPY m1bar.sh /usr/local/bin/m1bar
RUN chmod 755 /usr/local/bin/m1bar

EXPOSE 22
# 前台运行、日志进 stderr（docker logs 可见）；SIGHUP 让 sshd 重新读主机密钥。
CMD ["/usr/sbin/sshd", "-D", "-e"]
