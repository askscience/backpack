use std::io::Read;

use anyhow::{Context, Result};
use std::fs;
use std::process::Command;
use tracing::{debug, info, warn};

pub async fn extract_text(file_path: &str, mime: &str, original_name: &str) -> Result<String> {
    let mime_lower = mime.to_lowercase();
    let fp = file_path.to_string();
    let on = original_name.to_string();

    let result = tokio::task::spawn_blocking(move || {
        extract_text_blocking(&fp, &mime_lower, &on)
    })
    .await??;

    Ok(result.trim().to_string())
}

fn extract_text_blocking(file_path: &str, mime: &str, original_name: &str) -> Result<String> {
    let path = std::path::Path::new(original_name);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    debug!("Extracting: mime={}, ext={}, path={}", mime, ext, file_path);

    match () {
        _ if mime.starts_with("text/")
            || matches!(
                ext.as_str(),
                "txt" | "md"
                    | "csv"
                    | "json"
                    | "log"
                    | "yaml"
                    | "yml"
                    | "toml"
                    | "xml"
                    | "html"
                    | "htm"
                    | "rtf"
                    | "tex"
                    | "py"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "rs"
                    | "go"
                    | "java"
                    | "c"
                    | "cpp"
                    | "cc"
                    | "h"
                    | "hpp"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "css"
                    | "scss"
                    | "less"
                    | "sql"
                    | "r"
                    | "rb"
                    | "php"
                    | "swift"
                    | "kt"
                    | "lua"
                    | "cfg"
                    | "ini"
                    | "conf"
                    | "env"
                    | "dockerfile"
                    | "makefile"
            ) =>
        {
            extract_text_file(file_path)
        }
        _ if mime == "application/pdf" || ext == "pdf" => extract_pdf(file_path),
        _ if mime
            == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            || ext == "docx" =>
        {
            extract_docx(file_path)
        }
        _ if mime
            == "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            || ext == "xlsx" =>
        {
            extract_xlsx(file_path)
        }
        _ if mime
            == "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            || ext == "pptx" =>
        {
            extract_pptx(file_path)
        }
        _ if mime == "application/epub+zip" || ext == "epub" => extract_epub(file_path),
        _ if mime == "message/rfc822" || ext == "eml" => extract_eml(file_path),
        _ if mime.starts_with("image/")
            || matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tiff" | "tif" | "webp"
            ) =>
        {
            extract_ocr(file_path)
        }
        _ if mime.starts_with("audio/")
            || mime.starts_with("video/")
            || matches!(
                ext.as_str(),
                "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" | "wma" | "mp4" | "avi"
                    | "mkv" | "mov" | "webm" | "flv" | "wmv"
            ) =>
        {
            extract_transcribe(file_path)
        }
        _ => {
            warn!("Unsupported format: mime={}, ext={}", mime, ext);
            Ok(String::new())
        }
    }
}

fn extract_text_file(file_path: &str) -> Result<String> {
    fs::read_to_string(file_path).context("Failed to read text file")
}

fn extract_pdf(file_path: &str) -> Result<String> {
    info!("Extracting PDF: {}", file_path);
    pdf_extract::extract_text(file_path).context("Failed to extract PDF text")
}

fn extract_docx(file_path: &str) -> Result<String> {
    info!("Extracting DOCX: {}", file_path);
    let file = fs::File::open(file_path).context("Failed to open DOCX file")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read DOCX as ZIP")?;

    let mut text = String::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().to_string();
        if name == "word/document.xml" || name.starts_with("word/header") || name.starts_with("word/footer") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            text.push_str(&extract_xml_text(&content, "w:t"));
            text.push(' ');
        }
    }

    if text.trim().is_empty() {
        warn!("No text found in DOCX");
    }
    Ok(text.trim().to_string())
}

fn extract_xlsx(file_path: &str) -> Result<String> {
    info!("Extracting XLSX: {}", file_path);
    use calamine::{open_workbook, Data, Reader, Xlsx};

    let mut workbook: Xlsx<_> =
        open_workbook(file_path).context("Failed to open XLSX workbook")?;

    let mut text = String::new();
    let sheet_names = workbook.sheet_names().to_owned();
    for sheet_name in &sheet_names {
        text.push_str(&format!("\n--- Sheet: {} ---\n", sheet_name));
        if let Ok(range) = workbook.worksheet_range(sheet_name) {
            for row in range.rows() {
                let row_text: Vec<String> = row
                    .iter()
                    .filter_map(|cell| match cell {
                        Data::String(s) if !s.is_empty() => Some(s.clone()),
                        Data::Float(f) => Some(f.to_string()),
                        Data::Int(i) => Some(i.to_string()),
                        Data::Bool(b) => Some(b.to_string()),
                        _ => None,
                    })
                    .collect();
                if !row_text.is_empty() {
                    text.push_str(&row_text.join("\t"));
                    text.push('\n');
                }
            }
        }
    }

    Ok(text.trim().to_string())
}

fn extract_pptx(file_path: &str) -> Result<String> {
    info!("Extracting PPTX: {}", file_path);
    let file = fs::File::open(file_path).context("Failed to open PPTX file")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read PPTX as ZIP")?;

    let mut text = String::new();
    let mut slide_paths: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        let name = archive.by_index(i)?.name().to_string();
        if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
            slide_paths.push(name);
        }
    }
    slide_paths.sort();

    for slide_path in slide_paths {
        let slide_num = slide_path
            .trim_start_matches("ppt/slides/slide")
            .trim_end_matches(".xml");
        text.push_str(&format!("\n--- Slide {} ---\n", slide_num));

        let mut entry = archive.by_name(&slide_path)?;
        let mut content = String::new();
        entry.read_to_string(&mut content)?;
        text.push_str(&extract_xml_text(&content, "a:t"));
        text.push(' ');
    }

    Ok(text.trim().to_string())
}

fn extract_epub(file_path: &str) -> Result<String> {
    info!("Extracting EPUB: {}", file_path);
    let mut doc = epub::doc::EpubDoc::new(file_path).context("Failed to open EPUB")?;

    let mut text = String::new();
    let spine_ids: Vec<String> = doc.spine.iter().map(|s| s.idref.clone()).collect();

    for spine_id in &spine_ids {
        if let Some((resource, _mime)) = doc.get_resource(spine_id) {
            let html = String::from_utf8_lossy(&resource);
            text.push_str(&strip_html(&html));
            text.push('\n');
        }
    }

    if text.trim().is_empty() {
        let resource_keys: Vec<String> = doc.resources.keys().cloned().collect();
        for key in resource_keys {
            if let Some((resource, _mime)) = doc.get_resource(&key) {
                let html = String::from_utf8_lossy(&resource);
                let stripped = strip_html(&html);
                if !stripped.is_empty() {
                    text.push_str(&stripped);
                    text.push('\n');
                }
            }
        }
    }

    Ok(text.trim().to_string())
}

fn extract_eml(file_path: &str) -> Result<String> {
    info!("Extracting EML: {}", file_path);
    let bytes = fs::read(file_path).context("Failed to read EML file")?;
    let mail = mailparse::parse_mail(&bytes).context("Failed to parse EML")?;

    let mut text = String::new();

    for header in &mail.headers {
        match header.get_key().to_lowercase().as_str() {
            "subject" => {
                text.push_str("Subject: ");
                text.push_str(&header.get_value());
                text.push('\n');
            }
            "from" => {
                text.push_str("From: ");
                text.push_str(&header.get_value());
                text.push('\n');
            }
            "to" => {
                text.push_str("To: ");
                text.push_str(&header.get_value());
                text.push('\n');
            }
            "date" => {
                text.push_str("Date: ");
                text.push_str(&header.get_value());
                text.push('\n');
            }
            _ => {}
        }
    }
    text.push('\n');

    text.push_str(&extract_mail_body(&mail));

    Ok(text.trim().to_string())
}

fn extract_mail_body(mail: &mailparse::ParsedMail) -> String {
    let mut text = String::new();

    if mail.subparts.is_empty() {
        let body = mail.get_body().unwrap_or_default();
        let ct = &mail.ctype;
        if ct.mimetype.starts_with("text/plain") || ct.mimetype == "text/plain" {
            text.push_str(&body);
        } else if ct.mimetype == "text/html" {
            text.push_str(&strip_html(&body));
        } else {
            text.push_str(&body);
        }
    } else {
        for part in &mail.subparts {
            let ct = &part.ctype;
            if ct.mimetype == "text/plain" {
                text.push_str(&part.get_body().unwrap_or_default());
            } else if ct.mimetype == "text/html" {
                text.push_str(&strip_html(&part.get_body().unwrap_or_default()));
            }
        }
    }

    text
}

fn extract_ocr(file_path: &str) -> Result<String> {
    info!("Running OCR on: {}", file_path);

    let output = Command::new("tesseract")
        .arg(file_path)
        .arg("stdout")
        .arg("-l")
        .arg("eng")
        .output()
        .context("Failed to run tesseract. Ensure tesseract-ocr is installed.")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Tesseract stderr: {}", stderr);
        return Err(anyhow::anyhow!("OCR failed: {}", stderr.trim()));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(text)
}

fn extract_transcribe(file_path: &str) -> Result<String> {
    info!("Transcribing audio/video: {}", file_path);

    let temp_wav = std::env::temp_dir().join(format!(
        "backpack_transcribe_{}.wav",
        uuid::Uuid::new_v4()
    ));
    let temp_wav_str = temp_wav.to_string_lossy().to_string();

    let ffmpeg = Command::new("ffmpeg")
        .args([
            "-i",
            file_path,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-sample_fmt",
            "s16",
            &temp_wav_str,
            "-y",
            "-loglevel",
            "error",
        ])
        .output()
        .context("Failed to run ffmpeg for audio extraction. Ensure ffmpeg is installed.")?;

    if !ffmpeg.status.success() {
        let stderr = String::from_utf8_lossy(&ffmpeg.stderr);
        let _ = std::fs::remove_file(&temp_wav);
        return Err(anyhow::anyhow!("Audio extraction failed: {}", stderr.trim()));
    }

    let vosk_result = run_vosk(&temp_wav_str);
    let _ = std::fs::remove_file(&temp_wav);
    vosk_result
}

fn run_vosk(wav_path: &str) -> Result<String> {
    let vosk_model_path = std::env::var("VOSK_MODEL_PATH").unwrap_or_else(|_| "/opt/vosk-model".into());
    let vosk_script = "/opt/vosk-transcribe.py";

    if std::path::Path::new(&vosk_script).exists() {
        let output = Command::new("python3")
            .arg(&vosk_script)
            .arg(wav_path)
            .env("VOSK_MODEL_PATH", &vosk_model_path)
            .output()
            .context("Failed to run Vosk python transcriber")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Vosk stderr: {}", stderr);
            return Err(anyhow::anyhow!("Transcription failed: {}", stderr.trim()));
        }
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let output = Command::new("vosk-transcriber")
        .arg(wav_path)
        .arg("--model")
        .arg(&vosk_model_path)
        .output()
        .context("Failed to run vosk-transcriber")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Vosk stderr: {}", stderr);
        return Err(anyhow::anyhow!("Transcription failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn extract_xml_text(xml_content: &str, tag: &str) -> String {
    let closing = tag.replace(':', "/");
    let pattern = format!(
        r"<{}\s*[^>]*>(.*?)</{}>",
        regex::escape(tag),
        regex::escape(&closing)
    );
    let re = regex::Regex::new(&pattern).unwrap_or_else(|_| {
        regex::Regex::new(&format!(
            r"<{}\s*[^>]*>(.*?)</{}>",
            regex::escape(tag),
            regex::escape(&closing)
        ))
        .unwrap()
    });

    let mut text = String::new();
    for cap in re.captures_iter(xml_content) {
        if let Some(m) = cap.get(1) {
            text.push_str(m.as_str());
            text.push(' ');
        }
    }
    text
}

fn strip_html(html: &str) -> String {
    let re_tag = regex::Regex::new(r"<[^>]*>").unwrap();
    let re_ws = regex::Regex::new(r"\s+").unwrap();
    let text = re_tag.replace_all(html, " ");
    let text = decode_html_entities(&text);
    re_ws.replace_all(&text, " ").trim().to_string()
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&copy;", "©")
        .replace("&reg;", "®")
}
