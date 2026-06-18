# LLM Wiki —— 在 CentOS Stream 9 服务器上以容器方式部署 Tauri 2 桌面应用。
#
# 为什么用容器：宿主机（CentOS Stream 9）官方仓库只提供 webkit2gtk-4.0（libsoup2），
# 而 Tauri 2 在 Linux 上强依赖 webkit2gtk-4.1 + libsoup-3.0，且宿主 glibc 仅 2.34。
# Ubuntu 24.04 (noble) 自带 webkit2gtk-4.1，容器内构建+运行可彻底绕开这两个限制；
# 容器内用自带 glibc 2.39，只与宿主共享内核与 X11 socket，不受宿主 2.34 限制。
# GUI 通过挂载宿主 X11 socket 渲染到已运行的 VNC 桌面（DISPLAY=:1），见 deploy/run.sh。
#
# 注意：服务器位于国内，apt/npm/cargo/rustup 全部走国内镜像，否则会慢到不可用。
# 基础镜像选 ubuntu:24.04（服务器已缓存），避免再次拉取被镜像加速器拖死。

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# --- apt 换 aliyun 源（Ubuntu 24.04 为 deb822 格式）---
RUN sed -i 's@//archive.ubuntu.com@//mirrors.aliyun.com@g; s@//security.ubuntu.com@//mirrors.aliyun.com@g' \
      /etc/apt/sources.list.d/ubuntu.sources

# --- Tauri 2 Linux 构建 + 运行依赖（libsoup-3.0 随 webkit 带入）+ VNC 软渲染 + 验证小工具 ---
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl wget file ca-certificates pkg-config git xz-utils \
      libssl-dev \
      libwebkit2gtk-4.1-dev \
      libgtk-3-dev \
      libayatana-appindicator3-dev \
      librsvg2-dev \
      libxdo-dev \
      patchelf \
      libgl1 libglx-mesa0 libgl1-mesa-dri mesa-utils \
      dbus-x11 \
      x11-utils wmctrl imagemagick \
    && rm -rf /var/lib/apt/lists/*

# --- Node.js 22（从 npmmirror 下官方二进制，国内快且稳）---
ENV NODE_VERSION=22.23.0
RUN curl -fsSL "https://registry.npmmirror.com/-/binary/node/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" -o /tmp/node.tar.xz \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
    && rm /tmp/node.tar.xz \
    && node -v && npm -v \
    && npm config set registry https://registry.npmmirror.com

# --- Rust（rustup + crates 走 rsproxy.cn 国内镜像）---
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    RUSTUP_DIST_SERVER=https://rsproxy.cn \
    RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
RUN curl --proto '=https' --tlsv1.2 -sSf https://rsproxy.cn/rustup-init.sh \
      | sh -s -- -y --default-toolchain stable --profile minimal \
    && printf '%s\n' \
      '[source.crates-io]' \
      'replace-with = "rsproxy-sparse"' \
      '[source.rsproxy-sparse]' \
      'registry = "sparse+https://rsproxy.cn/index/"' \
      '[net]' \
      'git-fetch-with-cli = true' \
      > "$CARGO_HOME/config.toml"

WORKDIR /app
COPY . .

# 安装依赖并构建 deb 包。
# 用 `npm install`（而非 `npm ci`）：本仓库的 package-lock.json 与 package.json 存在
# 轻微不同步（@emnapi/* 传递依赖），ci 会硬失败；install 会就地重建锁文件。
# 同样预先 install mcp-server，使后续 build:desktop 里硬编码的
# `npm --prefix mcp-server ci` 面对的是已同步的锁文件。
# `tauri build` 触发 beforeBuildCommand=`npm run build:desktop`
# （内部完成 mcp-server ci/build + 前端 tsc/vite build）。
# 仅构建 deb，避免 AppImage 在容器内对 FUSE 的依赖。
RUN npm install --no-audit --no-fund
RUN npm install --prefix mcp-server --no-audit --no-fund
# lance-encoding（lancedb 依赖）的 build 脚本用 prost-build 生成 protobuf 代码，需要 protoc。
# protobuf-compiler 只提供 protoc 二进制；google/protobuf/*.proto 内置类型在 libprotobuf-dev
# （装到 /usr/include/google/protobuf/，protoc 默认 include 路径），否则报 empty.proto not found。
# 单独成层，避免使前面 node/rust/npm install 的缓存层失效。
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*
RUN npm run tauri -- build --bundles deb

# 安装产物。二进制名由 Cargo [[bin]] name = "llm-wiki" 决定。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ./src-tauri/target/release/bundle/deb/*.deb \
    && rm -rf /var/lib/apt/lists/*

# 字体：基础镜像无 CJK/印地语等字体，WebView 渲染中日韩文会变成豆腐块（□）。
# 放在编译层之后单独成层，避免使昂贵的 Rust 编译缓存失效（重建只需几十秒）。
#   fonts-noto-cjk        简繁中文/日文/韩文
#   fonts-noto-core       Devanagari(印地语) 等大量脚本
#   fonts-noto-color-emoji 彩色 emoji
RUN apt-get update && apt-get install -y --no-install-recommends \
      fonts-noto-cjk fonts-noto-core fonts-noto-color-emoji \
    && fc-cache -f \
    && rm -rf /var/lib/apt/lists/*

# pdfium 兜底：把仓库自带的 x86-64 libpdfium.so 放到固定路径，
# 通过 PDFIUM_DYNAMIC_LIB_PATH 让 fs.rs 的加载器优先命中，避免 deb 资源布局差异。
RUN cp /app/src-tauri/pdfium/libpdfium.so /usr/local/lib/libpdfium.so && ldconfig
ENV PDFIUM_DYNAMIC_LIB_PATH=/usr/local/lib/libpdfium.so

# 运行期环境：
#  - DISPLAY=:1            渲染到宿主 VNC 桌面（可被 run.sh 覆盖）
#  - WEBKIT_DISABLE_*      VNC 无 GPU，关闭 DMABUF/合成器，否则 WebView 常黑屏
#  - LIBGL_ALWAYS_SOFTWARE 强制软件渲染
#  - HOME=/data            Tauri store/配置/项目数据落到挂载卷，重启可持久
ENV DISPLAY=:1 \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    LIBGL_ALWAYS_SOFTWARE=1 \
    HOME=/data \
    NO_AT_BRIDGE=1
RUN mkdir -p /data

CMD ["llm-wiki"]
