use std::path::Path;
use std::process::Command;
use crate::types::BmcHealth;

pub struct BmcScraper {
    has_ipmi_device: bool,
    has_ipmitool: bool,
}

impl BmcScraper {
    pub fn new() -> Self {
        let has_ipmi_device = Path::new("/dev/ipmi0").exists() || Path::new("/dev/ipmi/0").exists();
        let has_ipmitool = Path::new("/usr/bin/ipmitool").exists() || Path::new("/usr/local/bin/ipmitool").exists();

        Self {
            has_ipmi_device,
            has_ipmitool,
        }
    }

    pub fn scrape(&self) -> Option<BmcHealth> {
        if self.has_ipmitool || self.has_ipmi_device {
            if let Some(bmc) = self.scrape_ipmitool() {
                return Some(bmc);
            }
        }

        // Fallback for non-servers: scrape motherboard hwmon
        self.scrape_hwmon_fallback()
    }

    fn scrape_ipmitool(&self) -> Option<BmcHealth> {
        let output = Command::new("ipmitool")
            .args(["sdr", "elist"])
            .output()
            .or_else(|_| Command::new("sudo").args(["-n", "ipmitool", "sdr", "elist"]).output())
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let text = String::from_utf8_lossy(&output.stdout);
        let mut ambient_temp: Option<f32> = None;
        let mut cpu_temp: Option<f32> = None;
        let mut fan_rpm: Option<u32> = None;
        let mut power_watts: Option<f32> = None;
        let mut psu_status = "OK".to_string();
        let mut bmc_type = "IPMI BMC".to_string();

        for line in text.lines() {
            let parts: Vec<&str> = line.split('|').map(|s| s.trim()).collect();
            if parts.len() < 3 {
                continue;
            }

            let name = parts[0];
            let reading_part = parts[parts.len() - 1];

            // Detect Dell iDRAC vs HP iLO vs Supermicro
            if name.contains("Pwr Consumption") {
                bmc_type = "Dell iDRAC (T440)".to_string();
                if let Some(w_str) = reading_part.split_whitespace().next() {
                    if let Ok(w) = w_str.parse::<f32>() {
                        power_watts = Some(w);
                    }
                }
            } else if name.contains("Inlet Ambient") || name.contains("01-Inlet") {
                bmc_type = "HP iLO 4 (ML350e)".to_string();
                if let Some(t_str) = reading_part.split_whitespace().next() {
                    if let Ok(t) = t_str.parse::<f32>() {
                        ambient_temp = Some(t);
                    }
                }
            } else if name.contains("Inlet Temp") || name.contains("Ambient Temp") {
                if let Some(t_str) = reading_part.split_whitespace().next() {
                    if let Ok(t) = t_str.parse::<f32>() {
                        ambient_temp = Some(t);
                    }
                }
            }

            // CPU Temp
            if (name.contains("CPU 1") || name.contains("CPU Temp") || name.contains("Temp") && name.contains("CPU"))
                && cpu_temp.is_none()
            {
                if let Some(t_str) = reading_part.split_whitespace().next() {
                    if let Ok(t) = t_str.parse::<f32>() {
                        cpu_temp = Some(t);
                    }
                }
            }

            // Fan RPM
            if (name.starts_with("Fan") || name.contains("FAN")) && reading_part.contains("RPM") && fan_rpm.is_none() {
                if let Some(rpm_str) = reading_part.split_whitespace().next() {
                    if let Ok(rpm) = rpm_str.parse::<u32>() {
                        fan_rpm = Some(rpm);
                    }
                }
            }

            // Power Supply redundancy
            if name.contains("PS Redundancy") || name.contains("Status") && name.contains("Presence") {
                if reading_part.contains("Disabled") {
                    psu_status = "Non-Redundant".to_string();
                } else if reading_part.contains("Presence detected") && psu_status == "OK" {
                    psu_status = "Redundant Present".to_string();
                }
            }
        }

        Some(BmcHealth {
            bmc_detected: true,
            bmc_type,
            ambient_temp_c: ambient_temp,
            cpu_temp_c: cpu_temp,
            fan_rpm_avg: fan_rpm,
            power_watts,
            psu_status,
        })
    }

    fn scrape_hwmon_fallback(&self) -> Option<BmcHealth> {
        let hwmon_path = Path::new("/sys/class/hwmon");
        if !hwmon_path.exists() {
            return None;
        }

        let mut cpu_temp: Option<f32> = None;
        let mut fan_rpm: Option<u32> = None;

        if let Ok(entries) = std::fs::read_dir(hwmon_path) {
            for entry in entries.flatten() {
                let p = entry.path();
                // Check name
                let name = std::fs::read_to_string(p.join("name")).unwrap_or_default();
                if name.contains("coretemp") || name.contains("k10temp") || name.contains("zenpower") {
                    if let Ok(t_str) = std::fs::read_to_string(p.join("temp1_input")) {
                        if let Ok(t_val) = t_str.trim().parse::<f32>() {
                            cpu_temp = Some(t_val / 1000.0);
                        }
                    }
                }

                if fan_rpm.is_none() {
                    if let Ok(fan_str) = std::fs::read_to_string(p.join("fan1_input")) {
                        if let Ok(f_val) = fan_str.trim().parse::<u32>() {
                            fan_rpm = Some(f_val);
                        }
                    }
                }
            }
        }

        if cpu_temp.is_some() || fan_rpm.is_some() {
            Some(BmcHealth {
                bmc_detected: false,
                bmc_type: "Host Linux hwmon".to_string(),
                ambient_temp_c: None,
                cpu_temp_c: cpu_temp,
                fan_rpm_avg: fan_rpm,
                power_watts: None,
                psu_status: "Standard ATX".to_string(),
            })
        } else {
            None
        }
    }
}
