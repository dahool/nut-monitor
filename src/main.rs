use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, Json},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rusqlite::{Connection, params};

struct AppState {
    ups_name: String,
    ups_host: String,
    fcm_config: Option<FcmConfig>,
    last_alerts: Mutex<LastAlertState>,
    db_conn: Mutex<Connection>, // Thread-safe local SQLite connection
}

struct FcmConfig {
    project_id: String,
    client_email: String,
    private_key: String,
}

#[derive(Default)]
struct LastAlertState {
    last_status: String,
    battery_low_sent: bool,
    load_high_sent: bool,
    runtime_low_sent: bool,
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

#[derive(Deserialize)]
struct RegisterDeviceRequest {
    device_token: String,
    device_name: String,
    device_id: String,
}

const HTML_TEMPLATE: &str = include_str!("template.html");

#[tokio::main]
async fn main() {
    let ups_name = env::var("UPS_NAME").unwrap_or_else(|_| "ups".to_string());
    let ups_host = env::var("UPS_HOST").unwrap_or_else(|_| "localhost".to_string());
    
    // Configurable monitor interval via environment variable (default: 10 seconds)
    let interval_secs = env::var("MONITOR_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10);

    // Extracted FCM config
    let fcm_config = if let (Ok(pid), Ok(email), Ok(key)) = (
        env::var("FCM_PROJECT_ID"), env::var("FCM_CLIENT_EMAIL"), env::var("FCM_PRIVATE_KEY")
    ) {
        Some(FcmConfig {
            project_id: pid,
            client_email: email,
            private_key: key.replace("\\n", "\n"),
        })
    } else {
        None
    };

    // Initialize Local SQLite Database
    let conn = Connection::open("devices.db").expect("Failed to open database");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS devices (
            device_id TEXT PRIMARY KEY,
            device_name TEXT NOT NULL,
            device_token TEXT NOT NULL
        )",
        [],
    ).expect("Failed to create devices table");

    let shared_state = Arc::new(AppState {
        ups_name,
        ups_host,
        fcm_config,
        last_alerts: Mutex::new(LastAlertState::default()),
        db_conn: Mutex::new(conn),
    });

    // Background alert loop using configurable intervals
    let monitor_state = shared_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        println!("Background monitoring thread initialized. Polling interval: {}s", interval_secs);
        loop {
            interval.tick().await;
            evaluate_alerts(&monitor_state).await;
        }
    });

    let app = Router::new()
        .route("/", get(html_handler))
        .route("/api/status", get(json_handler))
        .route("/api/register", post(register_device_handler))
        .route("/api/test-fcm", post(test_fcm_handler)) // New Manual Test Route
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("NUT Monitor server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

// --- Handlers ---

async fn register_device_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RegisterDeviceRequest>,
) -> StatusCode {
    let db = state.db_conn.lock().unwrap();
    
    let res = db.execute(
        "INSERT OR REPLACE INTO devices (device_id, device_name, device_token) VALUES (?1, ?2, ?3)",
        params![payload.device_id, payload.device_name, payload.device_token],
    );

    match res {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            eprintln!("Database error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

// Handler to trigger test FCM notifications on demand
async fn test_fcm_handler(State(state): State<Arc<AppState>>) -> (StatusCode, Json<serde_json::Value>) {
    let config = match &state.fcm_config {
        Some(c) => c,
        None => return (
            StatusCode::BAD_REQUEST, 
            Json(serde_json::json!({ "error": "FCM configurations are missing from environment vars" }))
        ),
    };

    let tokens = get_registered_tokens(&state);
    if tokens.is_empty() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "message": "FCM is configured but no devices are registered in database yet." }))
        );
    }

    let title = "⚠️ Test Notification";
    let message = "This is a test notification generated from your NUT Monitor endpoint.";

    let mut successful_sends = 0;
    for token in &tokens {
        if send_fcm_v1_notification(config, token, title, message).await.is_ok() {
            successful_sends += 1;
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "Test completed",
            "targets_found": tokens.len(),
            "successfully_sent": successful_sends
        }))
    )
}

// --- Alert Evaluation & Notification Utilities ---

fn get_registered_tokens(state: &AppState) -> Vec<String> {
    let db = state.db_conn.lock().unwrap();
    let mut stmt = db.prepare("SELECT device_token FROM devices").unwrap();
    let token_iter = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    token_iter.filter_map(|t| t.ok()).collect::<Vec<String>>()
}

async fn evaluate_alerts(state: &AppState) {
    let m = fetch_ups_metrics(state);
    if m.status == "Disconnected" || m.status.contains("Error") { return; }

    let mut trigger = false;
    let mut title = String::new();
    let mut message = String::new();

    {
        let mut alerts = state.last_alerts.lock().unwrap();

        // 1. Status Change Check
        if !alerts.last_status.is_empty() && alerts.last_status != m.status {
            trigger = true;
            title = format!("UPS Status Changed: {}", m.status);
            message = format!("Device shifted from {} to {}.", alerts.last_status, m.status);
        }
        alerts.last_status = m.status.clone();

        // 2. Battery Drop Threshold (< 50%)
        if let Ok(charge) = m.battery_charge.parse::<u32>() {
            if charge < 50 {
                if !alerts.battery_low_sent {
                    trigger = true;
                    title = "Alert: Low Battery Capacity".to_string();
                    message = format!("UPS battery charge dropped under threshold: {}%", charge);
                    alerts.battery_low_sent = true;
                }
            } else { alerts.battery_low_sent = false; }
        }

        // 3. Load Capacity Alert (> 80%)
        if let Ok(load) = m.ups_load.parse::<u32>() {
            if load > 80 {
                if !alerts.load_high_sent {
                    trigger = true;
                    title = "Alert: Critical Overload".to_string();
                    message = format!("UPS load capacity consumption is exceeding metrics: {}%", load);
                    alerts.load_high_sent = true;
                }
            } else { alerts.load_high_sent = false; }
        }

        // 4. Runtime Alert (< 15 min / 900 seconds)
        if let Ok(seconds) = m.runtime_seconds.parse::<u32>() {
            if seconds < 900 {
                if !alerts.runtime_low_sent {
                    trigger = true;
                    title = "Alert: Critical Low Runtime".to_string();
                    message = format!("UPS backup window is under 15 minutes ({} min remaining).", seconds / 60);
                    alerts.runtime_low_sent = true;
                }
            } else { alerts.runtime_low_sent = false; }
        }
    }

    if trigger {
        if let Some(ref config) = state.fcm_config {
            let tokens = get_registered_tokens(state);
            for token in tokens {
                let _ = send_fcm_v1_notification(config, &token, &title, &message).await;
            }
        }
    }
}

async fn send_fcm_v1_notification(config: &FcmConfig, device_token: &str, title: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let now = chrono::Utc::now().timestamp();
    
    let claims = serde_json::json!({
        "iss": config.client_email,
        "scope": "https://www.googleapis.com/auth/firebase.messaging",
        "aud": "https://oauth2.googleapis.com/token",
        "exp": now + 3600,
        "iat": now
    });

    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(config.private_key.as_bytes())?;
    let jwt = jsonwebtoken::encode(&header, &claims, &key)?;

    let token_res: serde_json::Value = client.post("https://oauth2.googleapis.com/token")
        .form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", &jwt)])
        .send().await?.json().await?;

    let access_token = token_res["access_token"].as_str().ok_or("Failed parsing access token")?;
    let url = format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", config.project_id);
    
    let payload = serde_json::json!({
        "message": {
            "token": device_token,
            "notification": { "title": title, "body": body },
            "android": { "priority": "high", "notification": { "sound": "default", "channel_id": "ups_alerts" } }
        }
    });

    client.post(&url).bearer_auth(access_token).json(&payload).send().await?;
    Ok(())
}

// --- Retained Handlers (Unchanged logic) ---

fn fetch_ups_metrics(state: &AppState) -> UpsMetrics {
    let output = Command::new("upsc")
        .arg(format!("{}@{}", state.ups_name, state.ups_host))
        .output();

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
                    "device.mfr" | "ups.mfr" => m.manufacturer = val,
                    "device.model" | "ups.model" => m.model = val,
                    "device.serial" | "ups.serial" => m.serial = val,
                    "battery.charge" => m.battery_charge = val,
                    "battery.voltage" => m.battery_voltage = val,
                    "battery.voltage.nominal" => m.battery_voltage_nominal = val,
                    "input.voltage" => m.input_voltage = val,
                    "input.voltage.nominal" => m.input_voltage_nominal = val,
                    "ups.load" => m.ups_load = val,
                    "ups.status" => {
                        m.status = match val.as_str() {
                            "OL" => "Online (AC)".to_string(),
                            "OB" => "On Battery".to_string(),
                            "LB" => "Low Battery ⚠️".to_string(),
                            _ => val,
                        };
                    }
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
        } else {
            m.status = "Communication Error (upsd)".to_string();
        }
    }
    m
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
        .replace("{battery_voltage_nominal}", &m.battery_voltage_nominal)
        .replace("{battery_voltage}", &m.battery_voltage);
    Html(html_content)
}

async fn json_handler(State(state): State<Arc<AppState>>) -> Json<UpsMetrics> {
    Json(fetch_ups_metrics(&state))
}