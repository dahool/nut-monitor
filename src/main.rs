use axum::{
    extract::State,
    response::{Html, Json},
    routing::get,
    Router,
};
use serde::Serialize;
use std::env;
use std::process::Command;
use std::sync::Arc;

struct AppState {
    ups_name: String,
    ups_host: String,
}

#[derive(Serialize, Clone)]
struct UpsMetrics {
    manufacturer: String,
    model: String,
    serial: String,
    status: String,
    battery_charge: String,
    battery_voltage: String,
    battery_voltage_nominal: String,
    input_voltage: String,
    input_voltage_nominal: String,
    ups_load: String,
    runtime_seconds: String,
    runtime_formatted: String,
}

const HTML_TEMPLATE: &str = include_str!("template.html");

#[tokio::main]
async fn main() {
    let ups_name = env::var("UPS_NAME").unwrap_or_else(|_| "ups".to_string());
    let ups_host = env::var("UPS_HOST").unwrap_or_else(|_| "localhost".to_string());

    let shared_state = Arc::new(AppState { ups_name, ups_host });

    let app = Router::new()
        .route("/", get(html_handler))
        .route("/api/status", get(json_handler))
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("NUT Monitor server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

// Función auxiliar para ejecutar upsc y parsear las métricas reales disponibles
fn fetch_ups_metrics(state: &AppState) -> UpsMetrics {
    let output = Command::new("upsc")
        .arg(format!("{}@{}", state.ups_name, state.ups_host))
        .output();

    let mut metrics = UpsMetrics {
        manufacturer: "APC".to_string(),
        model: "Back-UPS RS 900G".to_string(),
        serial: "N/A".to_string(),
        status: "Disconnected".to_string(),
        battery_charge: "N/A".to_string(),
        battery_voltage: "N/A".to_string(),
        battery_voltage_nominal: "N/A".to_string(),
        input_voltage: "N/A".to_string(),
        input_voltage_nominal: "N/A".to_string(),
        ups_load: "N/A".to_string(),
        runtime_seconds: "N/A".to_string(),
        runtime_formatted: "N/A".to_string(),
    };

    if let Ok(out) = output {
        if out.status.success() {
            let stdout_str = String::from_utf8_lossy(&out.stdout);
            for line in stdout_str.lines() {
                let parts: Vec<&str> = line.split(':').collect();
                if parts.len() < 2 { continue; }
                let key = parts[0].trim();
                let val = parts[1].trim().to_string();

                match key {
                    "device.mfr" | "ups.mfr" => metrics.manufacturer = val,
                    "device.model" | "ups.model" => metrics.model = val,
                    "device.serial" | "ups.serial" => metrics.serial = val,
                    "battery.charge" => metrics.battery_charge = val,
                    "battery.voltage" => metrics.battery_voltage = val,
                    "battery.voltage.nominal" => metrics.battery_voltage_nominal = val,
                    "input.voltage" => metrics.input_voltage = val,
                    "input.voltage.nominal" => metrics.input_voltage_nominal = val,
                    "ups.load" => metrics.ups_load = val,
                    "ups.status" => {
                        metrics.status = match val.as_str() {
                            "OL" => "Online (AC)".to_string(),
                            "OB" => "On Battery".to_string(),
                            "LB" => "Low Battery ⚠️".to_string(),
                            _ => val,
                        };
                    }
                    "battery.runtime" => {
                        metrics.runtime_seconds = val.clone();
                        if let Ok(seconds) = val.parse::<u32>() {
                            let mins = seconds / 60;
                            metrics.runtime_formatted = format!("{} min", mins);
                        } else {
                            metrics.runtime_formatted = val;
                        }
                    }
                    _ => {}
                }
            }
        } else {
            metrics.status = "Error connecting to upsd".to_string();
        }
    }

    metrics
}

async fn html_handler(State(state): State<Arc<AppState>>) -> Html<String> {
    let m = fetch_ups_metrics(&state);

    let status_class = match m.status.as_str() {
        "Online (AC)" => "status-online",
        "On Battery" => "status-battery",
        "Low Battery ⚠️" => "status-critical",
        _ => "status-unknown",
    };

    let html_content = HTML_TEMPLATE
        .replace("{ups_name}", &state.ups_name)
        .replace("{ups_host}", &state.ups_host)
        .replace("{mfr_model}", &m.model)
        .replace("{ups_status}", &m.status)
        .replace("{status_class}", status_class)
        .replace("{battery_charge}", &m.battery_charge)
        .replace("{charge_pct}", if m.battery_charge == "N/A" { "0" } else { &m.battery_charge })
        .replace("{ups_load}", &m.ups_load)
        .replace("{load_pct}", if m.ups_load == "N/A" { "0" } else { &m.ups_load })
        .replace("{ups_runtime}", &m.runtime_formatted)
        .replace("{input_voltage}", &m.input_voltage)
        .replace("{input_voltage_nominal}", &m.input_voltage_nominal)
        .replace("{battery_voltage}", &m.battery_voltage)
        .replace("{battery_voltage_nominal}", &m.battery_voltage_nominal);

    Html(html_content)
}

async fn json_handler(State(state): State<Arc<AppState>>) -> Json<UpsMetrics> {
    Json(fetch_ups_metrics(&state))
}