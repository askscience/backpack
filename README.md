# AI Cloud Backpack

A production-ready personal cloud tool that uses an external LLM to automatically catalog every uploaded file. It extracts all readable text, generates structured metadata (title, summary, tags, category), computes embeddings for semantic search, and provides a RAG-powered Q&A interface over your personal file collection.

## Features

- **Multi-format text extraction**: TXT, PDF, DOCX, XLSX, PPTX, EPUB, EML, images (OCR via Tesseract), audio/video (transcription via Vosk)
- **LLM-powered cataloging**: Each uploaded file is analyzed by an LLM using a configurable prompt (`skill.md`)
- **Semantic search**: Embeddings via external LLM APIs (OpenAI, Ollama, etc.), brute-force cosine similarity in SQLite
- **RAG Q&A**: Ask questions about your files — relevant content is retrieved and used as context for the LLM
- **Full CRUD**: Upload, search, ask, list inventory, download, delete
- **Iroh P2P**: Peer-to-peer connectivity via the [Iroh](https://iroh.computer) protocol — share a single ticket string to grant access. Encrypted QUIC, DHT-based discovery, no public relays, no IP in the invite
- **Docker-ready**: Multi-stage Dockerfile with Tesseract OCR, ffmpeg, and Vosk speech recognition

## Architecture

```
Upload → Extract text → LLM catalogs → Compute embedding → Store in SQLite
Search → Embed query → Cosine similarity → Return ranked results
Ask   → Embed question → Retrieve top-K files → LLM answers with context
```

- **Web framework**: Axum (Tokio async)
- **Database**: SQLite via sqlx
- **Embeddings**: Brute-force cosine similarity (stored as BLOB in SQLite)
- **LLM**: External API (OpenAI, Anthropic, Ollama, or OpenAI-compatible)
- **P2P**: Iroh protocol — encrypted QUIC tunnels with DHT-based peer discovery, no relays
- **Extraction**: pdf-extract, calamine (xlsx), epub, mailparse, zip+regex (docx/pptx), Tesseract (OCR), Vosk (transcription)

## Quick Start

### Prerequisites

- Rust 1.78+
- Optional (for local extraction): Tesseract OCR, ffmpeg, Vosk model

### 1. Clone and set up

```bash
cp .env.example .env
# Edit .env with your LLM API key
```

### 2. Run with Cargo

```bash
cargo run --release
```

The server starts on `http://0.0.0.0:8080`.

### 3. Run with Docker

```bash
docker compose up -d
```

Docker includes Tesseract OCR, ffmpeg, and Vosk for full text extraction.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LLM_PROVIDER` | `openai` | `openai`, `anthropic`, `ollama`, or `generic` |
| `LLM_API_KEY` | — | API key for the LLM provider |
| `LLM_MODEL` | `gpt-4o-mini` | Model name for cataloging + RAG |
| `LLM_ENDPOINT` | `https://api.openai.com/v1` | API endpoint |
| `EMBEDDING_MODEL` | `text-embedding-3-small` | Model for text embeddings |
| `EMBEDDING_DIM` | `1536` | Embedding vector dimension |
| `EMBEDDING_ENDPOINT` | (same as LLM) | Separate embedding endpoint if needed |
| `EMBEDDING_API_KEY` | (same as LLM) | Separate embedding API key if needed |
| `MAX_FILE_SIZE_MB` | `100` | Maximum upload file size |
| `UPLOAD_DIR` | `./uploads` | Where files are stored on disk |
| `DB_PATH` | `./data/backpack.db` | SQLite database path |
| `SKILL_PATH` | `./skill.md` | Path to the cataloging prompt file |
| `VOSK_MODEL_PATH` | `/opt/vosk-model` | Path to Vosk speech model |
| `BIND_ADDR` | `0.0.0.0:8080` | Server bind address |

## API Endpoints

### `POST /upload`

Upload a file (multipart form-data, field name `file`).

```bash
curl -X POST http://localhost:8080/upload \
  -F "file=@document.pdf"
```

Response:
```json
{
  "id": "a1b2c3d4-...",
  "original_name": "document.pdf",
  "mime": "application/pdf",
  "file_size": 12345,
  "title": "Quarterly Financial Report",
  "summary": "A financial report covering Q1 2024...",
  "tags": "finance, quarterly, report, 2024",
  "category": "document",
  "extracted_text_length": 5432,
  "created_at": "2024-01-15 10:30:00"
}
```

### `GET /search?q=...`

Semantic search across all cataloged files.

```bash
curl "http://localhost:8080/search?q=financial+report+Q1"
```

Response:
```json
{
  "query": "financial report Q1",
  "results": [
    {
      "id": "a1b2c3d4-...",
      "original_name": "document.pdf",
      "title": "Quarterly Financial Report",
      "summary": "A financial report...",
      "tags": "finance, quarterly, report",
      "category": "document",
      "score": 0.92,
      "created_at": "2024-01-15 10:30:00"
    }
  ]
}
```

### `POST /ask`

Ask a question about your files (RAG).

```bash
curl -X POST http://localhost:8080/ask \
  -H "Content-Type: application/json" \
  -d '{"question": "What was the revenue in Q1?"}'
```

Response:
```json
{
  "answer": "Based on the quarterly report, Q1 revenue was $2.3M...",
  "sources": [
    {
      "id": "a1b2c3d4-...",
      "title": "Quarterly Financial Report",
      "summary": "A financial report..."
    }
  ]
}
```

### `GET /inventory`

List all files grouped by category.

```bash
curl http://localhost:8080/inventory
```

### `GET /download/{id}`

Download the original file.

```bash
curl -O http://localhost:8080/download/a1b2c3d4-...
```

### `DELETE /files/{id}`

Delete a file and its metadata.

```bash
curl -X DELETE http://localhost:8080/files/a1b2c3d4-...
```

## Iroh P2P Connectivity

Start the server with the `--iroh` flag to enable peer-to-peer access. The server prints a **ticket** — a single string containing only a `NodeId` (no IP, no port, no relay URL). Share this ticket to grant access.

### How it works

```
Server ($ backpack --iroh)
   |
   |  1. Generates a SecretKey (NodeId = public key hash)
   |  2. Publishes its address to the Mainline DHT under its NodeId
   |  3. Prints the ticket string
   |
   v

NodeId:   2vx6ym67hxqqy5pk...
Ticket:   nodeid:2vx6ym67hxqqy5pk3j4zt2lc6nh6fznhrngeicf52lq7tabzsrrq

   |
   |  Share the ticket string
   |
   v

Client ($ backpack-cli nodeid:2vx6ym67...)
   |
   |  1. Parses the NodeId from the ticket
   |  2. Looks up the server's QUIC address via the DHT
   |  3. Establishes an encrypted QUIC connection directly
   |  4. Opens a local HTTP proxy on :9090
   |
   v

curl http://localhost:9090/  →  proxied to server's Axum HTTP API
```

No relays. No IP addresses in the ticket. No port forwarding needed (uses hole-punching via DHT). The `NodeId` acts as both identifier and pre-shared access token — only someone who knows it can discover the address.

### Server

```bash
backpack --iroh
```

Output:
```
Iroh NodeId:  2vx6ym67hxqqy5pk3j4zt2lc6nh6fznhrngeicf52lq7tabzsrrq
Iroh ticket:  nodeid:2vx6ym67hxqqy5pk3j4zt2lc6nh6fznhrngeicf52lq7tabzsrrq
HTTP listening on 0.0.0.0:8080
 ─────────────────────────────────────────────
 Share the ticket to grant P2P access.
 No relay used — DHT discovery, direct QUIC connections.
 ─────────────────────────────────────────────
```

The HTTP server still listens on `:8080` for local access. Iroh connections are bridged to it.

### Client

Build the client binary:

```bash
cargo build -p backpack-cli --release
```

Connect using the ticket:

```bash
backpack-cli nodeid:2vx6ym67hxqqy5pk3j4zt2lc6nh6fznhrngeicf52lq7tabzsrrq
```

Output:
```
Resolving node: 2vx6ym67... via DHT...
Connected. Proxy listening on http://127.0.0.1:9090
Use: curl http://localhost:9090/
```

From another terminal:

```bash
curl http://localhost:9090/
# → {"name":"AI Cloud Backpack","version":"0.1.0",...}

curl -X POST http://localhost:9090/upload -F "file=@doc.pdf"
# → { "total_files": 1, "results": [...] }

curl http://localhost:9090/search?q=budget
# → { "results": [...] }
```

### Security model

- **No relay server** — the DHT only stores address mappings, no traffic passes through it
- **No IP leak** — the ticket string contains only the NodeId (ed25519 public key hash)
- **End-to-end encrypted** — all traffic is QUIC with TLS 1.3
- **Knowledge-based auth** — whoever holds the ticket can connect. Treat the ticket like a password
- **No third-party servers** — the Mainline DHT is the same decentralized network used by BitTorrent

### Custom port

```bash
# Server
backpack --iroh                # Iroh binds random port, auto-published to DHT

# Client  
backpack-cli --port 3000 <ticket>   # Proxy on :3000 instead of :9090
```

## Provider-Specific Setup

### OpenAI

```env
LLM_PROVIDER=openai
LLM_API_KEY=sk-...
LLM_MODEL=gpt-4o-mini
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_DIM=1536
```

### Anthropic

```env
LLM_PROVIDER=anthropic
LLM_API_KEY=sk-ant-...
LLM_MODEL=claude-3-haiku-20240307
EMBEDDING_MODEL=text-embedding-3-small
EMBEDDING_DIM=1536
EMBEDDING_ENDPOINT=https://api.openai.com/v1
EMBEDDING_API_KEY=sk-...
```

Note: Anthropic does not provide an embedding API. You must provide `EMBEDDING_ENDPOINT` and `EMBEDDING_API_KEY` (e.g., pointing to OpenAI).

### Ollama (local)

```env
LLM_PROVIDER=ollama
LLM_ENDPOINT=http://localhost:11434
LLM_MODEL=llama3.2
EMBEDDING_MODEL=nomic-embed-text
EMBEDDING_DIM=768
```

### Generic OpenAI-compatible

```env
LLM_PROVIDER=generic
LLM_ENDPOINT=https://your-api.example.com/v1
LLM_API_KEY=your-key
LLM_MODEL=your-model
```

## The `skill.md` File

The cataloging prompt lives in `skill.md` at the project root. You can customize it to change how the LLM generates titles, summaries, tags, and categories. The file content is sent as the system message to the LLM; the extracted text is sent as the user message.

## Supported File Formats

| Format | Extraction Method |
|--------|------------------|
| `.txt`, `.md`, `.csv`, `.json`, `.log`, `.yaml`, `.toml`, code files | Direct read |
| `.pdf` | pdf-extract crate |
| `.docx` | Zip + XML parsing (`w:t` tags) |
| `.xlsx` | calamine crate |
| `.pptx` | Zip + XML parsing (`a:t` tags) |
| `.epub` | epub crate |
| `.eml` | mailparse crate |
| Images (png, jpg, etc.) | Tesseract OCR |
| Audio/Video | ffmpeg → 16kHz WAV → Vosk |

## License

GNU General Public License v3.0 - see [LICENSE](LICENSE) for details.
