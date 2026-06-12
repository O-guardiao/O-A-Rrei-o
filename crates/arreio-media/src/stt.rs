use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Resultado de transcrição de fala.
#[derive(Debug, Clone)]
pub struct SttResult {
    pub text: String,
    pub confidence: Option<f32>,
    pub language: String,
    pub duration_secs: Option<f64>,
}

/// Capacidade de converter áudio em texto.
pub trait SpeechRecognizer {
    fn transcribe(&self, audio_path: &Path, language: Option<&str>) -> Result<SttResult>;
    fn is_available(&self) -> bool;
}

/// STT via whisper (whisper-cli ou whisper como subprocesso).
/// Espera que o áudio já esteja em formato WAV 16kHz mono,
/// ou tenta converter via ffmpeg se disponível.
pub struct WhisperStt {
    command: String,
    model: String,
    ffmpeg_path: Option<String>,
}

impl WhisperStt {
    pub fn new() -> Self {
        let cmd = if command_exists("whisper-cli") {
            "whisper-cli"
        } else if command_exists("whisper") {
            "whisper"
        } else {
            "whisper-cli" // default, vai falhar com erro limpo se não existir
        };
        Self {
            command: cmd.into(),
            model: "tiny".into(),
            ffmpeg_path: if command_exists("ffmpeg") {
                Some("ffmpeg".into())
            } else {
                None
            },
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

impl SpeechRecognizer for WhisperStt {
    fn transcribe(&self, audio_path: &Path, language: Option<&str>) -> Result<SttResult> {
        // Verifica se precisa converter formato
        let wav_path = self.ensure_wav(audio_path)?;
        let wav_path_str = wav_path.to_string_lossy();

        let lang = language.unwrap_or("auto");

        let mut cmd = Command::new(&self.command);
        cmd.arg(&*wav_path_str)
            .arg("--model")
            .arg(&self.model)
            .arg("--output_format")
            .arg("txt");

        if lang != "auto" {
            cmd.arg("--language").arg(lang);
        }

        let output = cmd
            .output()
            .with_context(|| format!("executando {}", self.command))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("{} falhou: {}", self.command, stderr);
        }

        // whisper gera arquivo .txt no mesmo diretório do input
        let txt_path = wav_path.with_extension("txt");
        let text = if txt_path.exists() {
            std::fs::read_to_string(&txt_path).context("lendo transcrição")?
        } else {
            // Fallback: parse stdout
            String::from_utf8_lossy(&output.stdout).to_string()
        };

        let text = text.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("transcrição vazia");
        }

        Ok(SttResult {
            text,
            confidence: None,
            language: lang.to_string(),
            duration_secs: estimate_audio_duration(&wav_path),
        })
    }

    fn is_available(&self) -> bool {
        command_exists(&self.command)
    }
}

/// STT mock para testes. Retorna o nome do arquivo como "transcrição".
pub struct MockStt;

impl SpeechRecognizer for MockStt {
    fn transcribe(&self, audio_path: &Path, language: Option<&str>) -> Result<SttResult> {
        let text = format!(
            "[mock transcript of {}]",
            audio_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("audio")
        );
        Ok(SttResult {
            text,
            confidence: Some(1.0),
            language: language.unwrap_or("en").to_string(),
            duration_secs: Some(0.0),
        })
    }

    fn is_available(&self) -> bool {
        true
    }
}

impl WhisperStt {
    /// Converte áudio para WAV 16kHz mono se necessário.
    fn ensure_wav(&self, path: &Path) -> Result<std::path::PathBuf> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "wav" {
            return Ok(path.to_path_buf());
        }

        let ffmpeg = self
            .ffmpeg_path
            .as_deref()
            .context("ffmpeg não disponível para conversão de áudio")?;

        let temp_wav = tempfile::NamedTempFile::with_suffix(".wav")?;
        let wav_path = temp_wav.path().to_path_buf();
        let _guard = temp_wav;

        let output = Command::new(ffmpeg)
            .arg("-i")
            .arg(path)
            .arg("-ar")
            .arg("16000")
            .arg("-ac")
            .arg("1")
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg("-y")
            .arg(&wav_path)
            .output()
            .context("executando ffmpeg")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg falhou: {}", stderr);
        }

        Ok(wav_path)
    }
}

fn command_exists(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Estima duração lendo tamanho do arquivo WAV (16kHz mono 16-bit = 32000 bytes/sec).
fn estimate_audio_duration(path: &Path) -> Option<f64> {
    let meta = std::fs::metadata(path).ok()?;
    let file_size = meta.len() as f64;
    if file_size < 44.0 {
        return None;
    }
    let data_size = file_size - 44.0;
    let bytes_per_sec = 32000.0; // 16000 Hz * 2 bytes * 1 channel
    Some(data_size / bytes_per_sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_stt_returns_text() {
        let stt = MockStt;
        let result = stt.transcribe(Path::new("test.wav"), Some("pt")).unwrap();
        assert!(result.text.contains("mock transcript"));
        assert_eq!(result.language, "pt");
    }

    #[test]
    fn whisper_availability() {
        let stt = WhisperStt::new();
        // Apenas verifica que não panic
        let _ = stt.is_available();
    }
}
