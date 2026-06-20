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
    
    // Extracted FCM config (FCM_DEVICE_TOKEN is no longer required here)
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

    // Background alert loop
    let monitor_state = shared_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            evaluate_alerts(&monitor_state).await;
        }
    });

    let app = Router::new()
        .route("/", get(html_handler))
        .route("/api/status", get(json_handler))
        .route("/api/register", post(register_device_handler)) // New Endpoint
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
    
    // INSERT or REPLACE if device_id already exists (updates token/name)
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

// --- Alert Evaluation & Notification Utilities ---

async fn evaluate_alerts(state: &AppState) {
    let m = fetch_ups_metrics(state);
    if m.status == "Disconnected" || m.status.contains("Error") { return; }

    let mut trigger = false;
    let mut title = String::new();
    let mut message = String::new();

    // Limit the scope of the `alerts` and `db_conn` Mutex locks completely.
    // By enclosing this in a block, the locks are automatically dropped at the final `}`
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
    } // <-- `alerts` MutexGuard is dropped right here!

    // Now it is safe to await network requests because no locks are held
    if trigger {
        if let Some(ref config) = state.fcm_config {
            // Read tokens into a local vector, locking and unlocking immediately
            let tokens = {
                let db = state.db_conn.lock().unwrap();
                let mut stmt = db.prepare("SELECT device_token FROM devices").unwrap();
                let token_iter = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
                token_iter.filter_map(|t| t.ok()).collect::<Vec<String>>()
            }; // <-- `db` MutexGuard is dropped right here!

            // Loop through all saved devices and broadcast notifications safely
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
            "token": device_token, // Token now parsed dynamically per-device
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