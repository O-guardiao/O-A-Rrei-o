use anyhow::{Context, Result};
use std::process::Command;

/// Resultado de síntese de fala.
#[derive(Debug, Clone)]
pub struct TtsResult {
    pub audio_bytes: Vec<u8>,
    pub format: crate::MediaFormat,
    pub duration_secs: Option<f64>,
}

/// Capacidade de converter texto em fala.
pub trait SpeechSynthesizer {
    fn synthesize(&self, text: &str, language: Option<&str>) -> Result<TtsResult>;
    fn is_available(&self) -> bool;
}

/// TTS via espeak-ng (ou espeak) como subprocesso.
/// Gera arquivo WAV que é lido e retornado como bytes.
pub struct EspeakTts {
    command: String,
    voice: String,
}

impl EspeakTts {
    pub fn new() -> Self {
        // Tenta espeak-ng primeiro (mais moderno), fallback para espeak
        let cmd = if command_exists("espeak-ng") {
            "espeak-ng"
        } else {
            "espeak"
        };
        Self {
            command: cmd.into(),
            voice: "default".into(),
        }
    }

    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice = voice.into();
        self
    }
}

impl SpeechSynthesizer for EspeakTts {
    fn synthesize(&self, text: &str, language: Option<&str>) -> Result<TtsResult> {
        let temp_wav = tempfile::NamedTempFile::with_suffix(".wav")?;
        let wav_path = temp_wav.path().to_path_buf();
        // Manter o arquivo vivo até lermos
        let _guard = temp_wav;

        let voice = language.unwrap_or(&self.voice);

        let output = Command::new(&self.command)
            .arg("-v")
            .arg(voice)
            .arg("-w")
            .arg(&wav_path)
            .arg(text)
            .output()
            .with_context(|| format!("executando {} para TTS", self.command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("{} falhou: {}", self.command, stderr);
        }

        let audio_bytes = std::fs::read(&wav_path).context("lendo arquivo WAV gerado")?;
        let duration = estimate_wav_duration(&audio_bytes);

        Ok(TtsResult {
            audio_bytes,
            format: crate::MediaFormat::Wav,
            duration_secs: duration,
        })
    }

    fn is_available(&self) -> bool {
        command_exists(&self.command)
    }
}

/// TTS mock para testes. Não gera áudio real, apenas retorna placeholder.
pub struct MockTts;

impl SpeechSynthesizer for MockTts {
    fn synthesize(&self, text: &str, _language: Option<&str>) -> Result<TtsResult> {
        // Placeholder: retorna um "WAV" minimalista (header apenas)
        let fake_wav = build_minimal_wav_header(text.len() as u32);
        Ok(TtsResult {
            audio_bytes: fake_wav,
            format: crate::MediaFormat::Wav,
            duration_secs: Some(0.0),
        })
    }

    fn is_available(&self) -> bool {
        true
    }
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Estima duração de WAV PCM 16-bit mono 22050Hz (padrão do espeak).
fn estimate_wav_duration(bytes: &[u8]) -> Option<f64> {
    if bytes.len() < 44 {
        return None;
    }
    // WAV header: bytes 40-43 = data chunk size (little endian)
    let data_size = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]) as f64;
    // espeak default: 22050 Hz, 16-bit, 1 channel = 44100 bytes/sec
    let bytes_per_sec = 44100.0;
    Some(data_size / bytes_per_sec)
}

/// Constrói um WAV header mínimo válido (sem dados, só header).
fn build_minimal_wav_header(data_len: u32) -> Vec<u8> {
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    let file_len = 36 + data_len;
    wav.extend_from_slice(&file_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // subchunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&22050u32.to_le_bytes()); // sample rate
    wav.extend_from_slice(&44100u32.to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tts_produces_wav_header() {
        let tts = MockTts;
        let result = tts.synthesize("hello", None).unwrap();
        assert_eq!(result.format, crate::MediaFormat::Wav);
        assert!(result.audio_bytes.len() >= 44);
        assert_eq!(&result.audio_bytes[0..4], b"RIFF");
        assert_eq!(&result.audio_bytes[8..12], b"WAVE");
    }

    #[test]
    fn espeak_availability_check() {
        let tts = EspeakTts::new();
        // Não falha, apenas retorna true/false
        let _ = tts.is_available();
    }
}
