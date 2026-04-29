# =============================================================================
# Megadrive Studio — Dev Container
# Basé sur l'image SGDK officielle (Doragasu), ajoute BlastEm + ClownMDEmu
# =============================================================================
FROM registry.gitlab.com/doragasu/docker-sgdk:latest

LABEL maintainer="megadrive-studio"
LABEL description="Megadrive homebrew dev env: SGDK + BlastEm + ClownMDEmu"

# Dépendances supplémentaires
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake ninja-build \
    libsdl2-dev libsdl3-dev libglew-dev libpng-dev libfreetype-dev \
    xvfb gdb-multiarch python3 python3-pip \
    curl git \
    && rm -rf /var/lib/apt/lists/*

# Rust pour megalink
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
ENV PATH="/root/.cargo/bin:$PATH"

# BlastEm
RUN git clone --depth=1 https://github.com/libretro/blastem /opt/blastem-src \
    && cd /opt/blastem-src \
    && make -j$(nproc) CFLAGS="-O2" \
    && cp blastem /usr/local/bin/ \
    && rm -rf /opt/blastem-src
ENV BLASTEM=/usr/local/bin/blastem

# ClownMDEmu
RUN git clone --depth=1 --recursive \
    https://github.com/Clownacy/clownmdemu-frontend /opt/clownmdemu-src \
    && cd /opt/clownmdemu-src \
    && cmake -B build -DCMAKE_BUILD_TYPE=Release -G Ninja \
    && cmake --build build -j$(nproc) \
    && cp build/clownmdemu-frontend /usr/local/bin/clownmdemu \
    && rm -rf /opt/clownmdemu-src
ENV CLOWNMDEMU=/usr/local/bin/clownmdemu

# megalink-rs
RUN git clone --depth=1 https://github.com/ricky26/megalink-rs /opt/megalink-src \
    && cd /opt/megalink-src \
    && cargo build --release \
    && cp target/release/megalink /usr/local/bin/ \
    && rm -rf /opt/megalink-src
ENV MEGALINK=/usr/local/bin/megalink

# Python deps
RUN pip3 install --no-cache-dir pyserial

WORKDIR /src

CMD ["/bin/bash"]
