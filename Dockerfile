FROM rust:1.78-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true

COPY src ./src
COPY skill.md ./
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    tesseract-ocr \
    tesseract-ocr-eng \
    ffmpeg \
    python3 \
    python3-pip \
    wget \
    unzip \
    && rm -rf /var/lib/apt/lists/*

RUN pip3 install --break-system-packages vosk

RUN mkdir -p /opt/vosk-model && \
    cd /opt/vosk-model && \
    wget -q https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip && \
    unzip -q vosk-model-small-en-us-0.15.zip && \
    mv vosk-model-small-en-us-0.15/* . && \
    rm -rf vosk-model-small-en-us-0.15 vosk-model-small-en-us-0.15.zip

COPY --from=builder /app/target/release/backpack /usr/local/bin/backpack
COPY skill.md /app/skill.md

COPY docker/vosk-transcribe.py /opt/vosk-transcribe.py

RUN mkdir -p /app/uploads /app/data

ENV UPLOAD_DIR=/app/uploads
ENV DB_PATH=/app/data/backpack.db
ENV SKILL_PATH=/app/skill.md
ENV VOSK_MODEL_PATH=/opt/vosk-model

EXPOSE 8080

CMD ["backpack"]
