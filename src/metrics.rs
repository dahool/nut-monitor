use std::process::Command;
use serde::Serialize;
use crate::AppState;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct UpsMetrics {
    pub manufacturer: String,
    pub model: String,
    pub serial: String,
    pub status: String,
    pub battery_charge: String,
    pub battery_voltage: String,
    pub battery_voltage_nominal: String,
    pub input_voltage: String,
    pub input_voltage_nominal: String,
    pub ups_load: String,
    pub ups_load_watt: String,
    pub runtime_seconds: String,
    pub runtime_formatted: String,
}

pub fn status_to_message(status: &str, fallback: String) -> String {
    match status {
        "OL CHRG" => "Online (Charging)".to_string(),
        s if s.starts_with("OL") => "Online (AC)".to_string(),
        s if s.starts_with("OB") => "On Battery".to_string(),
        s if s.starts_with("LB") => "Low Battery ⚠️".to_string(),
        _ => fallback,
    }
}

pub fn parse_upsc_output(stdout_str: &str) -> UpsMetrics {
    let mut m = UpsMetrics {
        manufacturer: "Generic".to_string(),
        model: "NUT Device".to_string(),
        serial: "N/A".to_string(),
        status: "Disconnected".to_string(),
        battery_charge: "N/A".to_string(),
        battery_voltage: "N/A".to_string(),
        battery_voltage_nominal: "N/A".to_string(),
        input_voltage: "N/A".to_string(),
        input_voltage_nominal: "N/A".to_string(),
        ups_load: "N/A".to_string(),
        ups_load_watt: "N/A".to_string(),
        runtime_seconds: "N/A".to_string(),
        runtime_formatted: "N/A".to_string(),
    };

    let mut ups_load_val: Option<f64> = None;
    let mut realpower_nominal_val: Option<f64> = None;
    for line in stdout_str.lines() {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 2 { continue; }
        let key = parts[0].trim();
        let val = parts[1].trim().to_string();

        match key {
            "device.mfr" | "ups.mfr" => m.manufacturer = val,
            "device.model" | "ups.model" => m.model = val,
            "device.serial" | "ups.serial" => m.serial = val,
            "battery.charge" => m.battery_charge = val,
            "battery.voltage" => m.battery_voltage = val,
            "battery.voltage.nominal" => m.battery_voltage_nominal = val,
            "input.voltage" => m.input_voltage = val,
            "input.voltage.nominal" => m.input_voltage_nominal = val,
            "ups.load" => {
                m.ups_load = val.clone();
                ups_load_val = val.parse::<f64>().ok();
            }
            "ups.realpower.nominal" => {
                realpower_nominal_val = val.parse::<f64>().ok();
            }
            "ups.status" => {
                m.status = status_to_message(val.as_str(), val.clone());
            },
            "battery.runtime" => {
                m.runtime_seconds = val.clone();
                if let Ok(seconds) = val.parse::<u32>() {
                    m.runtime_formatted = format!("{} min", seconds / 60);
                } else {
                    m.runtime_formatted = val;
                }
            }
            _ => {}
        }
    }
    if let (Some(load), Some(nominal)) = (ups_load_val, realpower_nominal_val) {
        let watt = (load / 100.0) * nominal;
        m.ups_load_watt = format!("{:.1}", watt);
    }
    m
}

pub fn fetch_ups_metrics(state: &AppState) -> UpsMetrics {
    let output = Command::new("upsc")
        .arg(format!("{}@{}", state.ups_name, state.ups_host))
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            let stdout_str = String::from_utf8_lossy(&out.stdout);
            parse_upsc_output(&stdout_str)
        } else {
            let mut m = parse_upsc_output("");
            m.status = "Communication Error (upsd)".to_string();
            m
        }
    } else {
        let mut m = parse_upsc_output("");
        m.status = "Disconnected".to_string();
        m
    }
}
