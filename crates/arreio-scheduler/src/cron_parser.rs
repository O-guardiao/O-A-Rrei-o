use crate::job::JobSchedule;
use anyhow::{bail, Result};

/// Parse de schedule em múltiplos formatos:
/// - Cron expression: "0 2 * * *" (5 campos: min hora dia_mes mes dia_semana)
/// - Interval: "every 30m", "every 1h", "every 1d"
/// - One-shot: "30m" (daqui a 30 minutos), "1h" (daqui a 1 hora)
/// - ISO timestamp: "2026-01-01T00:00:00Z"
pub fn parse_schedule(input: &str) -> Result<JobSchedule> {
    let input = input.trim();

    // Interval: "every 30m"
    if input.starts_with("every ") {
        let rest = input.strip_prefix("every ").unwrap();
        return parse_interval(rest);
    }

    // One-shot: "30m", "1h", "1d"
    if let Some(minutes) = parse_shorthand_duration(input) {
        let now = now_epoch_secs();
        return Ok(JobSchedule::OnceAt(now + minutes * 60));
    }

    // ISO timestamp
    if input.contains('T') || input.contains('-') && input.len() > 10 {
        if let Ok(ts) = parse_iso_timestamp(input) {
            return Ok(JobSchedule::OnceAt(ts));
        }
    }

    // Cron expression (5 campos)
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.len() == 5 {
        return Ok(JobSchedule::CronExpression(input.to_string()));
    }

    bail!("formato de schedule não reconhecido: {}", input)
}

fn parse_interval(rest: &str) -> Result<JobSchedule> {
    let rest = rest.trim();
    if let Some(n) = rest.strip_suffix('m') {
        let mins: u32 = n
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("intervalo inválido"))?;
        return Ok(JobSchedule::IntervalMinutes(mins));
    }
    if let Some(n) = rest.strip_suffix('h') {
        let hours: u32 = n
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("intervalo inválido"))?;
        return Ok(JobSchedule::IntervalMinutes(hours * 60));
    }
    if let Some(n) = rest.strip_suffix('d') {
        let days: u32 = n
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("intervalo inválido"))?;
        return Ok(JobSchedule::IntervalMinutes(days * 24 * 60));
    }
    bail!("intervalo não reconhecido: {}", rest)
}

fn parse_shorthand_duration(input: &str) -> Option<u64> {
    if let Some(n) = input.strip_suffix('m') {
        return n.trim().parse().ok();
    }
    if let Some(n) = input.strip_suffix('h') {
        return n.trim().parse::<u64>().ok().map(|h| h * 60);
    }
    if let Some(n) = input.strip_suffix('d') {
        return n.trim().parse::<u64>().ok().map(|d| d * 24 * 60);
    }
    None
}

fn parse_iso_timestamp(input: &str) -> Result<u64> {
    // Parse simplificado: espera formatos como 2026-01-01T00:00:00Z
    let input = input.trim_end_matches('Z');
    let parts: Vec<&str> = input.split('T').collect();
    if parts.len() != 2 {
        bail!("timestamp ISO inválido");
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        bail!("timestamp ISO inválido");
    }
    let year: i32 = date_parts[0].parse()?;
    let month: u32 = date_parts[1].parse()?;
    let day: u32 = date_parts[2].parse()?;
    let hour: u32 = time_parts[0].parse()?;
    let min: u32 = time_parts[1].parse()?;
    let sec: u32 = time_parts[2].parse()?;

    let days_since_epoch = days_since_1970(year, month, day)?;
    let seconds = days_since_epoch * 86400 + (hour as u64 * 3600) + (min as u64 * 60) + sec as u64;
    Ok(seconds)
}

fn days_since_1970(year: i32, month: u32, day: u32) -> Result<u64> {
    if year < 1970 {
        bail!("ano anterior a 1970 não suportado");
    }
    let mut days = 0u64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += month_days[(m - 1) as usize] as u64;
        if m == 2 && is_leap(year) {
            days += 1;
        }
    }
    days += (day - 1) as u64;
    Ok(days)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Verifica se um cron expression bate no instante atual (simplificado).
pub fn cron_matches(
    cron: &str,
    minute: u8,
    hour: u8,
    day_of_month: u8,
    month: u8,
    day_of_week: u8,
) -> bool {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    matches_field(parts[0], minute as u32, 0, 59)
        && matches_field(parts[1], hour as u32, 0, 23)
        && matches_field(parts[2], day_of_month as u32, 1, 31)
        && matches_field(parts[3], month as u32, 1, 12)
        && matches_field(parts[4], day_of_week as u32, 0, 7)
}

fn matches_field(field: &str, value: u32, _min: u32, _max: u32) -> bool {
    if field == "*" {
        return true;
    }
    // Suporta "*/n" (step)
    if field.starts_with("*/") {
        if let Ok(step) = field[2..].parse::<u32>() {
            return value % step == 0;
        }
    }
    // Suporta listas: "1,2,3"
    for part in field.split(',') {
        if let Ok(v) = part.parse::<u32>() {
            if v == value {
                return true;
            }
        }
        // Suporta ranges: "1-5"
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(s), Ok(e)) = (start.parse::<u32>(), end.parse::<u32>()) {
                if value >= s && value <= e {
                    return true;
                }
            }
        }
    }
    false
}

/// Próximo timestamp onde o cron expression bate (busca simples).
pub fn next_cron_run(cron: &str, after: u64) -> Option<u64> {
    // Busca linear pelos próximos 366 dias
    for offset in 1..=(366 * 24 * 60) {
        let ts = after + (offset * 60);
        let (min, hour, dom, mon, dow) = ts_to_cron_fields(ts);
        if cron_matches(cron, min, hour, dom, mon, dow) {
            return Some(ts);
        }
    }
    None
}

fn ts_to_cron_fields(ts: u64) -> (u8, u8, u8, u8, u8) {
    let mins = ts / 60;
    let min = (mins % 60) as u8;
    let hour = ((mins / 60) % 24) as u8;
    let days_since_1970 = mins / 60 / 24;
    let (_year, month, day) = date_from_days(days_since_1970);
    let dow = ((days_since_1970 + 4) % 7) as u8; // 1970-01-01 foi quinta (4)
    (min, hour, day, month, dow)
}

fn date_from_days(mut days: u64) -> (u32, u8, u8) {
    let mut year = 1970i32;
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u8;
    for (i, &md) in month_days.iter().enumerate() {
        let md = if i == 1 && is_leap(year) { 29 } else { md };
        if days < md as u64 {
            break;
        }
        days -= md as u64;
        month += 1;
    }
    (year as u32, month, (days + 1) as u8)
}

// ── Testes ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_every_30m() {
        let sched = parse_schedule("every 30m").unwrap();
        assert_eq!(sched, JobSchedule::IntervalMinutes(30));
    }

    #[test]
    fn parse_interval_every_1h() {
        let sched = parse_schedule("every 1h").unwrap();
        assert_eq!(sched, JobSchedule::IntervalMinutes(60));
    }

    #[test]
    fn parse_interval_every_1d() {
        let sched = parse_schedule("every 1d").unwrap();
        assert_eq!(sched, JobSchedule::IntervalMinutes(1440));
    }

    #[test]
    fn parse_one_shot_30m() {
        let before = now_epoch_secs();
        let sched = parse_schedule("30m").unwrap();
        if let JobSchedule::OnceAt(ts) = sched {
            assert!(ts >= before + 30 * 60 && ts <= before + 30 * 60 + 5);
        } else {
            panic!("esperado OnceAt");
        }
    }

    #[test]
    fn parse_cron_expression() {
        let sched = parse_schedule("0 2 * * *").unwrap();
        assert_eq!(sched, JobSchedule::CronExpression("0 2 * * *".to_string()));
    }

    #[test]
    fn parse_iso_timestamp() {
        let sched = parse_schedule("2026-01-01T00:00:00Z").unwrap();
        if let JobSchedule::OnceAt(ts) = sched {
            assert_eq!(ts, 1767225600);
        } else {
            panic!("esperado OnceAt");
        }
    }

    #[test]
    fn parse_rejects_invalid() {
        assert!(parse_schedule("invalid stuff").is_err());
    }

    #[test]
    fn cron_matches_star() {
        assert!(cron_matches("* * * * *", 30, 12, 15, 6, 1));
    }

    #[test]
    fn cron_matches_specific() {
        assert!(cron_matches("30 12 * * *", 30, 12, 15, 6, 1));
        assert!(!cron_matches("30 12 * * *", 31, 12, 15, 6, 1));
    }

    #[test]
    fn cron_matches_step() {
        assert!(cron_matches("*/15 * * * *", 0, 12, 15, 6, 1));
        assert!(cron_matches("*/15 * * * *", 15, 12, 15, 6, 1));
        assert!(cron_matches("*/15 * * * *", 30, 12, 15, 6, 1));
        assert!(!cron_matches("*/15 * * * *", 10, 12, 15, 6, 1));
    }

    #[test]
    fn cron_matches_range() {
        assert!(cron_matches("1-5 * * * *", 3, 12, 15, 6, 1));
        assert!(!cron_matches("1-5 * * * *", 10, 12, 15, 6, 1));
    }

    #[test]
    fn next_cron_run_finds_next() {
        let now = now_epoch_secs();
        let next = next_cron_run("0 0 * * *", now).unwrap();
        assert!(next > now);
        // A diferença deve ser no máximo 25 horas
        assert!(next - now <= 25 * 3600);
    }
}
