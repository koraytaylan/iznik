# The application: a headless Wayland compositor (cage), screenshot and
# keystroke tools (grim, wtype), and the iznik-app binary. Cage runs as
# the compositor with WLR_BACKENDS=headless; grim captures the
# framebuffer; wtype injects keystrokes. Python + Pillow provide pixel
# analysis. The base matches the developer's host OS so the dynamically
# linked iznik-app binary runs without glibc mismatches.
FROM docker.io/library/ubuntu:26.04

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       cage grim wtype imagemagick python3 python3-pil \
       openssh-client xwayland \
       libxkbcommon-x11-0 \
       libwayland-egl1 libwayland-cursor0 libwayland-client0 \
       libwayland-server0 libegl1 libgl1 \
    && rm -rf /var/lib/apt/lists/*

RUN userdel -r ubuntu 2>/dev/null || true \
    && groupadd -f render \
    && useradd --uid 1000 --create-home --shell /bin/bash --groups video,render iznik

USER iznik
WORKDIR /home/iznik

# The binary and the test harness are injected via podman cp at runtime.
