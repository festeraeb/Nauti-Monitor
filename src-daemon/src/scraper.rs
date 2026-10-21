use crate::bmc_scraper::BmcScraper;
use crate::lemonade::LemonadeScraper;
use crate::llama_scraper::LlamaScraper;
use crate::mail_scraper::MailScraper;
use crate::model_scraper::ModelScraper;
use crate::types::{GpuStats, ModelServingInfo, NicRate, UnifiedTelemetry};
use all_smi::AllSmi;
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Networks, RefreshKind, System};
use tracing::{info, warn};

pub struct HardwareScraper {
    node_name: String,
    os_name: String,
    system: System,
    networks: Networks,
    lemonade_scraper: LemonadeScraper,
    mail_scraper: MailScraper,
    bmc_scraper: BmcScraper,
    model_scraper: ModelScraper,
    llama_scraper: LlamaScraper,
    all_smi: Option<AllSmi>,
    last_nic_samples: HashMap<String, (u64, u64, Instant)>,
    denylist_prefixes: Vec<String>,
    allowlist_interfaces: Option<Vec<String>>,
    pub polaris_fallback_active: bool,
    pub all_smi_ok: bool,
}

impl HardwareScraper {
    pub fn new(
        node_name: String,
        lemonade_url: String,
        denylist_prefixes: Option<Vec<String>>,
        allowlist_interfaces: Option<Vec<String>>,
    ) -> Self {
        let refresh_kind = RefreshKind::new()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything());
        let mut system = System::new_with_specifics(refresh_kind);
        system.refresh_cpu_usage();
        system.refresh_memory();

        let networks = Networks::new_with_refreshed_list();

        let os_name = System::long_os_version()
            .or_else(System::os_version)
            .unwrap_or_else(|| "Linux (Omarchy)".to_string());

        let all_smi = match AllSmi::new() {
            Ok(smi) => {
                info!("all-smi initialized successfully");
                Some(smi)
            }
            Err(e) => {
                warn!("all-smi failed to initialize: {}. Polaris sysfs fallback enabled", e);
                None
            }
        };

        let default_denylist = vec![
            "lo".to_string(),
            "veth".to_string(),
            "docker".to_string(),
            "br-".to_string(),
            "virbr".to_string(),
            "tap".to_string(),
            "tun".to_string(),
            "tailscale".to_string(),
            "wg".to_string(),
            "flannel".to_string(),
            "cni".to_string(),
            "cali".to_string(),
            "kube".to_string(),
            "dummy".to_string(),
            "bond_masters".to_string(),
        ];

        let denylist_prefixes = denylist_prefixes.unwrap_or(default_denylist);
        let lemonade_scraper = LemonadeScraper::new(lemonade_url);
        let mail_scraper = MailScraper::new(None);
        let bmc_scraper = BmcScraper::new();
        let model_scraper = ModelScraper::new();
        let llama_scraper = LlamaScraper::new();

        Self {
            node_name,
            os_name,
            system,
            networks,
            lemonade_scraper,
            mail_scraper,
            bmc_scraper,
            model_scraper,
            llama_scraper,
            all_smi,
            last_nic_samples: HashMap::new(),
            denylist_prefixes,
            allowlist_interfaces,
            polaris_fallback_active: false,
            all_smi_ok: false,
        }
    }

    /// Check if a given interface is a physical NIC based on allowlist and prefix denylist.
    fn is_physical_nic(&self, iface: &str) -> bool {
        if let Some(ref allowlist) = self.allowlist_interfaces {
            if allowlist.iter().any(|a| a == iface) {
                return true;
            }
        }

        for prefix in &self.denylist_prefixes {
            if iface.starts_with(prefix) {
                return false;
            }
        }

        // Additional Linux verification: physical NICs typically have /sys/class/net/<iface>/device
        let device_path = format!("/sys/class/net/{}/device", iface);
        if Path::new(&device_path).exists() {
            return true;
        }

        true
    }

    /// Scrape sysinfo CPU and RAM.
    fn scrape_system(&mut self) -> (f32, u64, u64) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();

        let cpu_pct = self.system.global_cpu_usage();
        let ram_used = self.system.used_memory();
        let ram_total = self.system.total_memory();

        (cpu_pct, ram_used, ram_total)
    }

    /// Compute real byte rates (delta_bytes / elapsed_secs) for physical NICs.
    fn scrape_nics(&mut self) -> HashMap<String, NicRate> {
        self.networks.refresh();
        let now = Instant::now();
        let mut nics = HashMap::new();

        for (interface_name, net_data) in &self.networks {
            if !self.is_physical_nic(interface_name) {
                continue;
            }

            let total_rx = net_data.total_received();
            let total_tx = net_data.total_transmitted();

            let rate = if let Some(&(prev_rx, prev_tx, prev_time)) = self.last_nic_samples.get(interface_name) {
                let elapsed = (now - prev_time).as_secs_f64();
                if elapsed > 0.05 {
                    let delta_rx = total_rx.saturating_sub(prev_rx);
                    let delta_tx = total_tx.saturating_sub(prev_tx);
                    let rx_bps = (delta_rx as f64 / elapsed).round() as u64;
                    let tx_bps = (delta_tx as f64 / elapsed).round() as u64;
                    NicRate { rx_bps, tx_bps }
                } else {
                    NicRate { rx_bps: 0, tx_bps: 0 }
                }
            } else {
                NicRate { rx_bps: 0, tx_bps: 0 }
            };

            self.last_nic_samples
                .insert(interface_name.clone(), (total_rx, total_tx, now));
            nics.insert(interface_name.clone(), rate);
        }

        nics
    }

    /// Scrape GPUs using all-smi, falling back to sysfs for AMD Polaris and nvidia-smi for NVIDIA.
    fn scrape_gpus(&mut self) -> Vec<GpuStats> {
        let mut stats = Vec::new();

        if let Some(ref smi) = self.all_smi {
            let gpus = smi.get_gpu_info();
            if !gpus.is_empty() {
                self.all_smi_ok = true;
                for gpu in gpus {
                    let vendor = if gpu.name.to_lowercase().contains("amd")
                        || gpu.name.to_lowercase().contains("radeon")
                    {
                        "AMD".to_string()
                    } else if gpu.name.to_lowercase().contains("nvidia")
                        || gpu.name.to_lowercase().contains("geforce")
                        || gpu.name.to_lowercase().contains("tesla")
                    {
                        "NVIDIA".to_string()
                    } else if gpu.name.to_lowercase().contains("intel") {
                        "Intel".to_string()
                    } else {
                        gpu.device_type.clone()
                    };

                    stats.push(GpuStats {
                        name: gpu.name,
                        vendor,
                        utilization: gpu.utilization as f32,
                        vram_used: gpu.used_memory,
                        vram_total: gpu.total_memory,
                        model: None,
                        temp_celsius: if gpu.temperature > 0 {
                            Some(gpu.temperature as f32)
                        } else {
                            None
                        },
                        power_watts: if gpu.power_consumption > 0.0 {
                            Some(gpu.power_consumption as f32)
                        } else {
                            None
                        },
                    });
                }
            }
        }

        // Check and supplement with AMD Polaris sysfs if AMD accelerators exist
        let polaris_gpus = self.scrape_polaris_sysfs();
        if !polaris_gpus.is_empty() {
            self.polaris_fallback_active = true;
            for p_gpu in polaris_gpus {
                if !stats.iter().any(|g| g.name == p_gpu.name) {
                    stats.push(p_gpu);
                }
            }
        }

        // Check and supplement with direct NVIDIA query if any NVIDIA GPU is absent
        let nvidia_gpus = self.scrape_nvidia_stats();
        for n_gpu in nvidia_gpus {
            if !stats.iter().any(|g| g.name == n_gpu.name) {
                stats.push(n_gpu);
            }
        }

        stats
    }

    /// Dedicated direct NVIDIA GPU reader for bare-metal Linux.
    fn scrape_nvidia_stats(&self) -> Vec<GpuStats> {
        let mut gpus = Vec::new();
        let output = Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw",
                "--format=csv,noheader,nounits",
            ])
            .output();

        if let Ok(out) = output {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                    if parts.len() >= 6 {
                        let name = parts[0].to_string();
                        let utilization = parts[1].parse::<f32>().unwrap_or(0.0);
                        let vram_used_mb = parts[2].parse::<u64>().unwrap_or(0);
                        let vram_total_mb = parts[3].parse::<u64>().unwrap_or(0);
                        let temp = parts[4].parse::<f32>().ok();
                        let power = parts[5].parse::<f32>().ok();

                        gpus.push(GpuStats {
                            name,
                            vendor: "NVIDIA".to_string(),
                            utilization,
                            vram_used: vram_used_mb * 1024 * 1024,
                            vram_total: vram_total_mb * 1024 * 1024,
                            temp_celsius: temp,
                            power_watts: power,
                            model: None,
                        });
                    }
                }
            }
        }
        gpus
    }

    /// Dedicated AMD Polaris sysfs reader for bare-metal Linux.
    fn scrape_polaris_sysfs(&self) -> Vec<GpuStats> {
        let mut gpus = Vec::new();
        let drm_path = Path::new("/sys/class/drm");
        if !drm_path.exists() {
            return gpus;
        }

        if let Ok(entries) = fs::read_dir(drm_path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().to_string_lossy().to_string();
                // Match card0, card1, card2, but not card0-DP-1
                if file_name.starts_with("card") && !file_name.contains('-') {
                    let dev_path = entry.path().join("device");
                    let vram_total_path = dev_path.join("mem_info_vram_total");
                    let vram_used_path = dev_path.join("mem_info_vram_used");
                    let busy_path = dev_path.join("gpu_busy_percent");

                    if let Ok(total_str) = fs::read_to_string(&vram_total_path) {
                        let total_bytes = total_str.trim().parse::<u64>().unwrap_or(0);
                        if total_bytes > 0 {
                            let used_bytes = fs::read_to_string(&vram_used_path)
                                .ok()
                                .and_then(|s| s.trim().parse::<u64>().ok())
                                .unwrap_or(0);

                            let busy_pct = fs::read_to_string(&busy_path)
                                .ok()
                                .and_then(|s| s.trim().parse::<f32>().ok())
                                .unwrap_or(0.0);

                            // Read temperature and power from hwmon
                            let (temp, power) = Self::read_hwmon(&dev_path);

                            // Discover product or card name
                            let card_name = Self::read_card_name(&dev_path, &file_name);

                            gpus.push(GpuStats {
                                name: card_name,
                                vendor: "AMD (Polaris)".to_string(),
                                utilization: busy_pct,
                                vram_used: used_bytes,
                                vram_total: total_bytes,
                                temp_celsius: temp,
                                power_watts: power,
                                model: None,
                            });
                        }
                    }
                }
            }
        }

        gpus
    }

    fn read_hwmon(dev_path: &Path) -> (Option<f32>, Option<f32>) {
        let hwmon_dir = dev_path.join("hwmon");
        if let Ok(entries) = fs::read_dir(hwmon_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let temp_path = p.join("temp1_input");
                let temp = fs::read_to_string(temp_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<f32>().ok())
                    .map(|m| m / 1000.0);

                let power_path = p.join("power1_input");
                let power_path_alt = p.join("power1_average");
                let power = fs::read_to_string(&power_path)
                    .or_else(|_| fs::read_to_string(&power_path_alt))
                    .ok()
                    .and_then(|s| s.trim().parse::<f32>().ok())
                    .map(|m| m / 1_000_000.0);

                return (temp, power);
            }
        }
        (None, None)
    }

    fn read_card_name(dev_path: &Path, default_name: &str) -> String {
        let uevent_path = dev_path.join("uevent");
        if let Ok(content) = fs::read_to_string(uevent_path) {
            for line in content.lines() {
                if line.starts_with("PCI_ID=") {
                    let id = line.trim_start_matches("PCI_ID=");
                    if id.eq_ignore_ascii_case("1002:67DF") {
                        return "AMD Radeon RX 470/480/570/580 (Ellesmere)".to_string();
                    } else if id.eq_ignore_ascii_case("1002:67C7") {
                        return "AMD Radeon Pro WX 5100 (Ellesmere)".to_string();
                    }
                    return format!("AMD GPU [{}]", id);
                }
            }
        }
        default_name.to_string()
    }

    /// Perform a full telemetry tick.
    pub async fn scrape(&mut self) -> UnifiedTelemetry {
        let (cpu_pct, ram_used, ram_total) = self.scrape_system();
        let physical_nics = self.scrape_nics();
        let mut accelerators = self.scrape_gpus();
        let lemonade = self.lemonade_scraper.scrape().await;
        let mail_health = self.mail_scraper.scrape().await;
        let bmc_health = self.bmc_scraper.scrape();
        let model_info = self.model_scraper.scrape().await;
        let llama_info = self.llama_scraper.scrape().await;

        // Merge model-serving telemetry from both scrapers by GPU index.
        // Llama-scraper (runtime-discovered) entries take priority over
        // model-scraper (hardcoded endpoint) entries, so insert them first.
        let mut merged_model_info: HashMap<u32, Vec<ModelServingInfo>> = HashMap::new();
        for (gpu_idx, models) in &llama_info {
            merged_model_info
                .entry(*gpu_idx)
                .or_default()
                .extend(models.clone());
        }
        for (gpu_idx, models) in &model_info {
            merged_model_info
                .entry(*gpu_idx)
                .or_default()
                .extend(models.clone());
        }

        // Attach model-serving telemetry to each GPU by index.
        for (i, gpu) in accelerators.iter_mut().enumerate() {
            if let Some(models) = merged_model_info.get(&(i as u32)) {
                if let Some(first) = models.first() {
                    gpu.model = Some(first.clone());
                }
            }
        }

        let mut rx_bps_total = 0.0;
        let mut tx_bps_total = 0.0;
        for nic in physical_nics.values() {
            rx_bps_total += nic.rx_bps as f64;
            tx_bps_total += nic.tx_bps as f64;
        }

        let now = Utc::now();
        UnifiedTelemetry {
            timestamp: now,
            timestamp_unix: now.timestamp(),
            node_name: self.node_name.clone(),
            os: self.os_name.clone(),
            cpu_pct,
            cpu_usage: cpu_pct,
            ram_used_bytes: ram_used,
            memory_used_bytes: ram_used,
            ram_total_bytes: ram_total,
            memory_total_bytes: ram_total,
            rx_bps: rx_bps_total,
            tx_bps: tx_bps_total,
            physical_nics,
            accelerators: accelerators.clone(),
            gpu_stats: accelerators,
            lemonade_healthy: lemonade.healthy,
            lemonade,
            mail_health,
            bmc_health,
        }
    }
}
