use anyhow::{Context, Result};
use std::process::Command;

/// Target de entrega de output de jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTarget {
    Local,    // stdout local / blackboard
    Origin,   // plataforma de origem (telegram, discord, etc)
    Telegram, // Telegram bot
    Discord,  // Discord webhook
    Slack,    // Slack webhook
    Email,    // Email
    Webhook,  // Webhook genérico
}

impl DeliveryTarget {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "local" => Some(Self::Local),
            "origin" => Some(Self::Origin),
            "telegram" => Some(Self::Telegram),
            "discord" => Some(Self::Discord),
            "slack" => Some(Self::Slack),
            "email" => Some(Self::Email),
            "webhook" => Some(Self::Webhook),
            _ => None,
        }
    }
}

/// Delivery de output com suporte a media extraction e truncation.
pub struct DeliveryService;

impl DeliveryService {
    pub fn new() -> Self {
        Self
    }

    /// Entrega output para o target especificado.
    /// Para targets remotos (Webhook, Discord, Slack, Telegram, Origin),
    /// tenta enviar via `curl` se disponível no sistema.
    pub fn deliver(
        &self,
        target: &DeliveryTarget,
        output: &str,
        metadata: &DeliveryMetadata,
    ) -> Result<String> {
        let truncated = Self::truncate(output, 4000);
        match target {
            DeliveryTarget::Local => {
                println!("[job output] {}", truncated);
                Ok(truncated)
            }
            DeliveryTarget::Origin => {
                Self::try_webhook_post("ARREIO_ORIGIN_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Origin delivery falhou: {} — retornando stub", e);
                        Ok(format!("[origin] {}", truncated))
                    })
            }
            DeliveryTarget::Telegram => {
                Self::try_webhook_post("ARREIO_TELEGRAM_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Telegram delivery falhou: {} — retornando stub", e);
                        Ok(format!("[telegram] {}", truncated))
                    })
            }
            DeliveryTarget::Discord => {
                Self::try_webhook_post("ARREIO_DISCORD_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Discord delivery falhou: {} — retornando stub", e);
                        Ok(format!("[discord] {}", truncated))
                    })
            }
            DeliveryTarget::Slack => {
                Self::try_webhook_post("ARREIO_SLACK_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Slack delivery falhou: {} — retornando stub", e);
                        Ok(format!("[slack] {}", truncated))
                    })
            }
            DeliveryTarget::Email => {
                Self::try_webhook_post("ARREIO_EMAIL_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Email delivery falhou: {} — retornando stub", e);
                        Ok(format!("[email] {}", truncated))
                    })
            }
            DeliveryTarget::Webhook => {
                Self::try_webhook_post("ARREIO_WEBHOOK_URL", &truncated, metadata)
                    .or_else(|e| {
                        eprintln!("[scheduler] Webhook delivery falhou: {} — retornando stub", e);
                        Ok(format!("[webhook] {}", truncated))
                    })
            }
        }
    }

    /// Tenta POST via curl para a URL definida na variável de ambiente.
    fn try_webhook_post(env_var: &str, payload: &str, metadata: &DeliveryMetadata) -> Result<String> {
        let url = std::env::var(env_var)
            .with_context(|| format!("variável de ambiente {} não definida", env_var))?;
        if url.is_empty() {
            anyhow::bail!("{} está vazia", env_var);
        }
        let body = serde_json::json!({
            "job_id": metadata.job_id,
            "job_name": metadata.job_name,
            "output": payload,
        });
        let output = Command::new("curl")
            .args([
                "-sS",
                "-X", "POST",
                "-H", "Content-Type: application/json",
                "-d", &body.to_string(),
                &url,
            ])
            .output()
            .with_context(|| "curl não disponível no sistema")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("curl falhou: {}", stderr);
        }
        Ok(payload.to_string())
    }

    /// Extrai tags MEDIA: do output.
    pub fn extract_media(output: &str) -> Vec<MediaAttachment> {
        let mut media = Vec::new();
        for line in output.lines() {
            if let Some(tag) = line.trim().strip_prefix("MEDIA:") {
                let parts: Vec<&str> = tag.trim().splitn(2, ':').collect();
                if parts.len() == 2 {
                    media.push(MediaAttachment {
                        r#type: parts[0].trim().to_string(),
                        path: parts[1].trim().to_string(),
                    });
                }
            }
        }
        media
    }

    /// Trunca output se exceder max_chars.
    fn truncate(output: &str, max_chars: usize) -> String {
        if output.len() <= max_chars {
            output.to_string()
        } else {
            format!(
                "{}... [truncated: {} chars total]",
                &output[..max_chars],
                output.len()
            )
        }
    }
}

impl Default for DeliveryService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeliveryMetadata {
    pub job_id: String,
    pub job_name: String,
}

#[derive(Debug, Clone)]
pub struct MediaAttachment {
    pub r#type: String,
    pub path: String,
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_target_from_str() {
        assert_eq!(
            DeliveryTarget::from_str("local"),
            Some(DeliveryTarget::Local)
        );
        assert_eq!(
            DeliveryTarget::from_str("telegram"),
            Some(DeliveryTarget::Telegram)
        );
        assert_eq!(DeliveryTarget::from_str("unknown"), None);
    }

    #[test]
    fn deliver_local() {
        let svc = DeliveryService::new();
        let result = svc
            .deliver(&DeliveryTarget::Local, "hello world", &Default::default())
            .unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn truncate_long_output() {
        let svc = DeliveryService::new();
        let long = "a".repeat(5000);
        let result = svc
            .deliver(&DeliveryTarget::Local, &long, &Default::default())
            .unwrap();
        assert!(result.ends_with("... [truncated: 5000 chars total]"));
        assert!(result.len() <= 4050);
    }

    #[test]
    fn extract_media_tags() {
        let output = "Result: ok\nMEDIA:image:/tmp/photo.png\nMEDIA:audio:/tmp/voice.wav";
        let media = DeliveryService::extract_media(output);
        assert_eq!(media.len(), 2);
        assert_eq!(media[0].r#type, "image");
        assert_eq!(media[0].path, "/tmp/photo.png");
        assert_eq!(media[1].r#type, "audio");
        assert_eq!(media[1].path, "/tmp/voice.wav");
    }

    #[test]
    fn extract_no_media() {
        let output = "Just plain text\nNo media here";
        let media = DeliveryService::extract_media(output);
        assert!(media.is_empty());
    }
}
