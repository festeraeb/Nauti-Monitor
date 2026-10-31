use std::path::Path;
use std::time::Instant;
use tokio::net::TcpStream;
use crate::types::MailHealth;

pub struct MailScraper {
    smtp_addr: String,
    queue_dirs: Vec<String>,
}

impl MailScraper {
    pub fn new(smtp_addr: Option<String>) -> Self {
        Self {
            smtp_addr: smtp_addr.unwrap_or_else(|| "127.0.0.1:25".to_string()),
            queue_dirs: vec![
                "/var/spool/postfix".to_string(),
                "/var/spool/mail".to_string(),
                "/var/mail".to_string(),
            ],
        }
    }

    pub async fn scrape(&self) -> MailHealth {
        let start = Instant::now();
        let smtp_online = match tokio::time::timeout(
            std::time::Duration::from_millis(1000),
            TcpStream::connect(&self.smtp_addr),
        )
        .await
        {
            Ok(Ok(_)) => true,
            _ => false,
        };
        let last_handshake_ms = start.elapsed().as_millis() as u32;

        // Count messages in active/incoming/deferred queues
        let mut queue_count = 0u32;
        let mut target_dir = "";

        for qdir in &self.queue_dirs {
            let p = Path::new(qdir);
            if p.exists() {
                target_dir = qdir;
                for sub in &["active", "incoming", "deferred", "hold", "corrupt", "maildir"] {
                    let sub_p = p.join(sub);
                    if let Ok(entries) = std::fs::read_dir(&sub_p) {
                        queue_count += entries.filter_map(Result::ok).count() as u32;
                    }
                }
                break;
            }
        }

        // Measure spool mount storage using sysinfo
        let mut storage_used_bytes = 0u64;
        let mut storage_total_bytes = 100 * 1024 * 1024 * 1024; // Default 100GB fallback

        if !target_dir.is_empty() {
            let disks = sysinfo::Disks::new_with_refreshed_list();
            for disk in &disks {
                if Path::new(target_dir).starts_with(disk.mount_point()) {
                    storage_total_bytes = disk.total_space();
                    storage_used_bytes = disk.total_space().saturating_sub(disk.available_space());
                    break;
                }
            }
        }

        MailHealth {
            smtp_online,
            queue_count,
            storage_used_bytes,
            storage_total_bytes,
            last_handshake_ms,
        }
    }
}
