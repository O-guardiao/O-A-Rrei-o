//! arreio-media — Media Generation e Processing para O Arreio.
//!
//! Arquitetura síncrona, sem async. Engines externas via subprocesso.
//! Blackboard-centered: resultados de media podem ser persistidos como tuplas.

pub mod stt;
pub mod tts;
pub mod vision;

pub use stt::{SpeechRecognizer, SttResult, WhisperStt};
pub use tts::{EspeakTts, SpeechSynthesizer, TtsResult};
pub use vision::{ImageDescriber, OllamaVisionDescriber};

use anyhow::Result;

/// Formato de mídia suportado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFormat {
    Png,
    Jpeg,
    Webp,
    Wav,
    Mp3,
    Ogg,
    Text,
}

impl MediaFormat {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("png") => Some(MediaFormat::Png),
            Some("jpg") | Some("jpeg") => Some(MediaFormat::Jpeg),
            Some("webp") => Some(MediaFormat::Webp),
            Some("wav") => Some(MediaFormat::Wav),
            Some("mp3") => Some(MediaFormat::Mp3),
            Some("ogg") => Some(MediaFormat::Ogg),
            Some("txt") | Some("md") => Some(MediaFormat::Text),
            _ => None,
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            MediaFormat::Png => "image/png",
            MediaFormat::Jpeg => "image/jpeg",
            MediaFormat::Webp => "image/webp",
            MediaFormat::Wav => "audio/wav",
            MediaFormat::Mp3 => "audio/mpeg",
            MediaFormat::Ogg => "audio/ogg",
            MediaFormat::Text => "text/plain",
        }
    }
}

/// Metadados de um arquivo de mídia.
#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub path: std::path::PathBuf,
    pub format: MediaFormat,
    pub size_bytes: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f64>,
}

/// Localiza o diretório de mídia do projeto.
pub fn media_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".arreio/media")
}

/// Garante que o diretório de mídia existe.
pub fn ensure_media_dir() -> Result<std::path::PathBuf> {
    let dir = media_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Salva bytes de mídia no diretório do projeto com nome único.
pub fn save_media(bytes: &[u8], name: &str) -> Result<std::path::PathBuf> {
    let dir = ensure_media_dir()?;
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}
