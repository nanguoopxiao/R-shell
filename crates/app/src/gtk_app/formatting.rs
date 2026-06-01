use super::*;

pub(super) fn format_host_stats(host: &str, stats: &HostStats) -> String {
    format!(
        "Host: {}    CPU: {:.0}%    Mem: {}/{} MB    Net: ↓{}/s ↑{}/s    Uptime: {}",
        host,
        stats.cpu_percent,
        stats.mem_used_mb,
        stats.mem_total_mb,
        format_bytes(stats.rx_bytes_per_sec),
        format_bytes(stats.tx_bytes_per_sec),
        format_uptime(stats.uptime_seconds),
    )
}

pub(super) fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn format_permissions(permissions: Option<u32>, is_dir: bool) -> String {
    let Some(mode) = permissions else {
        return if is_dir {
            "d---------".to_string()
        } else {
            "----------".to_string()
        };
    };

    let mut result = String::with_capacity(10);
    result.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        result.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        result.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        result.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    result
}

pub(super) fn format_unix_timestamp(timestamp: u64) -> String {
    if timestamp == 0 {
        return "-".to_string();
    }

    let days = (timestamp / 86_400) as i64;
    let seconds = timestamp % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

pub(super) fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

pub(super) fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}
